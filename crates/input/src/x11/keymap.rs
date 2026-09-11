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
            // Column 0 is the base/unshifted keysym for this keycode —
            // the physical-key identity we want, independent of
            // whatever modifier state a particular press happens under.
            let Some(&keysym) = reply.keysyms.get(offset * per_keycode) else {
                continue;
            };
            if keysym == 0 {
                continue; // Unassigned keycode.
            }
            let key = keysym_to_key(keysym);
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
}
