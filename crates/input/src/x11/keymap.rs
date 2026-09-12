//! Runtime keycode ↔ [`Key`] table, queried once per capture/inject
//! session via the X11 core protocol's `GetKeyboardMapping` request.
//!
//! Unlike macOS's Carbon keycodes or Windows' `VK_*` constants, an X11
//! keycode has no fixed, portable meaning — it's an arbitrary number
//! assigned by this particular X server's keyboard driver, and can
//! differ between machines or even between X sessions on the same
//! machine. Only the keysym layer (see [`super::keysym`]) is portable,
//! so this table exists purely to bridge "the wire protocol gives us
//! raw keycodes" to "key normalization needs keysyms" — this is the
//! actual "thin shim" for X11 keyboard handling, verified by manual QA
//! rather than unit tests (per ADR-0007's HAL-split rationale), since
//! it needs a real X server connection to build.

use std::collections::HashMap;

use kvm_protocol::Key;
use x11rb::connection::Connection;
use x11rb::protocol::xproto;

use super::keysym::keysym_to_key;

/// The physical key one keycode stands for, given its keysyms in column
/// order — `None` for an unassigned keycode (no base keysym).
///
/// The first keysym this crate can *name* wins, not simply column 0.
/// Column 0 is the base symbol for most keys (`a` for the A key), but on
/// the standard XKB keypad it is the Num-Lock-*off* meaning: keypad 7 is
/// `[KP_Home, KP_7]`. Taking column 0 left every number-pad digit and the
/// keypad decimal unnamed, so injecting `Key::Numpad7` found no keycode at
/// all — real hardware (Mac -> Linux): the digit keys were refused, and
/// Num Lock, which *was* injected and toggled, appeared to do nothing
/// (ADR-0007's 2026-09-13 update). Named as `Numpad7`, the keycode is
/// pressed and the X server applies its own Num Lock state, exactly as for
/// a real keypad. A key whose column 0 is already named is unchanged; one
/// with no nameable keysym stays `Key::Unknown` of its base keysym.
fn key_for_keysyms(keysyms: &[u32]) -> Option<Key> {
    let &base = keysyms.first()?;
    if base == 0 {
        return None;
    }
    let named = keysyms
        .iter()
        .filter(|&&keysym| keysym != 0)
        .map(|&keysym| keysym_to_key(keysym))
        .find(|key| !matches!(key, Key::Unknown(_)));
    Some(named.unwrap_or_else(|| keysym_to_key(base)))
}

/// A snapshot of one X server's keycode↔keysym assignment, captured at
/// `start()` time. Not automatically kept in sync with later keyboard
/// layout changes — see ADR-0008's documented limitation.
pub struct KeyMap {
    keycode_to_key: HashMap<u8, Key>,
    key_to_keycode: HashMap<Key, u8>,
}

impl KeyMap {
    /// Queries the current keyboard mapping from the X server this
    /// connection belongs to.
    pub fn query<C: Connection>(conn: &C) -> Result<Self, String> {
        let setup = conn.setup();
        let min_keycode = setup.min_keycode;
        let max_keycode = setup.max_keycode;
        let count = max_keycode.saturating_sub(min_keycode).saturating_add(1);

        let reply = xproto::get_keyboard_mapping(conn, min_keycode, count)
            .map_err(|e| format!("GetKeyboardMapping request failed: {e}"))?
            .reply()
            .map_err(|e| format!("GetKeyboardMapping reply failed: {e}"))?;

        let per_keycode = reply.keysyms_per_keycode as usize;
        let mut keycode_to_key = HashMap::new();
        let mut key_to_keycode = HashMap::new();

        for (offset, keycode) in (min_keycode..=max_keycode).enumerate() {
            if per_keycode == 0 {
                break;
            }
            let start = offset * per_keycode;
            let Some(keysyms) = reply.keysyms.get(start..start + per_keycode) else {
                continue;
            };
            let Some(key) = key_for_keysyms(keysyms) else {
                continue; // Unassigned keycode.
            };
            keycode_to_key.insert(keycode, key);
            // First (lowest) keycode wins the reverse lookup, so
            // injection stays deterministic even if a keysym happens to
            // be bound to more than one keycode.
            key_to_keycode.entry(key).or_insert(keycode);
        }

        Ok(Self {
            keycode_to_key,
            key_to_keycode,
        })
    }

    /// Translates a raw X11 keycode into a normalized [`Key`], or
    /// `Key::Unknown(keycode)` if this table has no mapping for it
    /// (an unassigned keycode, or one this session never saw in its
    /// initial query).
    pub fn keycode_to_key(&self, keycode: u8) -> Key {
        self.keycode_to_key
            .get(&keycode)
            .copied()
            .unwrap_or(Key::Unknown(keycode as u32))
    }

    /// The inverse of [`Self::keycode_to_key`], for injection. Returns
    /// `None` if this X server's current mapping has no keycode bound
    /// to `key`'s keysym at all (should only happen for `Key::Unknown`
    /// whose original code came from a *different* platform's raw
    /// value, not a real X11 keysym).
    pub fn key_to_keycode(&self, key: Key) -> Option<u8> {
        self.key_to_keycode.get(&key).copied()
    }

    /// Test-only: builds a `KeyMap` directly from `(keycode, key)`
    /// pairs, without a real X server connection.
    #[cfg(test)]
    pub(crate) fn from_pairs(pairs: &[(u8, Key)]) -> Self {
        let mut keycode_to_key = HashMap::new();
        let mut key_to_keycode = HashMap::new();
        for &(keycode, key) in pairs {
            keycode_to_key.insert(keycode, key);
            key_to_keycode.entry(key).or_insert(keycode);
        }
        Self {
            keycode_to_key,
            key_to_keycode,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn known_keycode_maps_to_its_key() {
        let map = KeyMap::from_pairs(&[(38, Key::A)]);
        assert_eq!(map.keycode_to_key(38), Key::A);
        assert_eq!(map.key_to_keycode(Key::A), Some(38));
    }

    #[test]
    fn unknown_keycode_becomes_unknown_not_a_panic() {
        let map = KeyMap::from_pairs(&[(38, Key::A)]);
        assert_eq!(map.keycode_to_key(255), Key::Unknown(255));
    }

    #[test]
    fn key_with_no_bound_keycode_has_no_injection_target() {
        let map = KeyMap::from_pairs(&[(38, Key::A)]);
        assert_eq!(map.key_to_keycode(Key::B), None);
    }

    const KP_HOME: u32 = 0xff95;
    const KP_7: u32 = 0xffb7;
    const KP_DELETE: u32 = 0xff9f;
    const KP_DECIMAL: u32 = 0xffae;
    const KP_ADD: u32 = 0xffab;
    const NUM_LOCK: u32 = 0xff7f;

    /// Regression test for the Mac -> Linux number pad: the standard XKB
    /// keypad keycode lists its Num-Lock-off symbol first.
    #[test]
    fn a_keypad_digit_is_named_by_its_digit_not_its_num_lock_off_symbol() {
        assert_eq!(key_for_keysyms(&[KP_HOME, KP_7]), Some(Key::Numpad7));
        assert_eq!(
            key_for_keysyms(&[KP_DELETE, KP_DECIMAL]),
            Some(Key::NumpadDecimal)
        );
    }

    #[test]
    fn keys_whose_base_symbol_is_already_named_are_unchanged() {
        assert_eq!(key_for_keysyms(&[0x61, 0x41]), Some(Key::A)); // a, A
        assert_eq!(key_for_keysyms(&[KP_ADD, KP_ADD]), Some(Key::NumpadAdd));
        assert_eq!(key_for_keysyms(&[NUM_LOCK, 0]), Some(Key::NumLock));
    }

    #[test]
    fn unassigned_and_unnameable_keycodes_behave_as_before() {
        assert_eq!(key_for_keysyms(&[0, KP_7]), None);
        assert_eq!(key_for_keysyms(&[]), None);
        assert_eq!(key_for_keysyms(&[KP_HOME, 0]), Some(Key::Unknown(KP_HOME)));
    }
}
