//! Cross-platform modifier mapping — pure, OS-independent, and the reason
//! this file has no `#[cfg(target_os = ...)]` anywhere in it. See ADR-0007
//! decision 3 and its 2026-09-13 update.
//!
//! **Positional, by the project owner's decision (2026-09-13):** a
//! modifier key does the job of the key in the same physical spot on the
//! other machine's keyboard. Left of the space bar, a Mac keyboard has
//! control / option / command where a PC keyboard (Windows or Linux) has
//! Ctrl / Windows / Alt — so between a Mac and a PC, **Option ↔ Windows
//! key** and **Command ↔ Alt** swap, on each side of the keyboard, and
//! **Control stays Control**. Every other key (letters, digits, function
//! keys, navigation, Shift, Caps Lock) is unchanged, and Windows ↔ Linux
//! is always the identity (their keyboards are the same).
//!
//! This replaced Phase 3's role-based mapping (Command ↔ Control), under
//! which no Mac key could ever press the Windows key on the other side —
//! real hardware: the Linux "Show Apps" overview (Windows key) could not
//! be opened from the Mac at all. Consequence, by design: copying on a PC
//! while controlling it from the Mac is Control+C; copying on the Mac from
//! a PC keyboard is Alt+C (the Command spot).

use kvm_protocol::{Key, PlatformKind};

/// Translates `key`, as captured on `source`, into what should actually
/// be injected on `target`.
pub fn translate_for_target(key: Key, source: PlatformKind, target: PlatformKind) -> Key {
    let source_is_mac = source == PlatformKind::MacOs;
    let target_is_mac = target == PlatformKind::MacOs;
    if source_is_mac == target_is_mac {
        // The same platform, or Windows <-> Linux: identical keyboards.
        return key;
    }
    // Exactly one side is a Mac: swap the keys that share a physical spot.
    match key {
        Key::AltLeft => Key::MetaLeft,
        Key::MetaLeft => Key::AltLeft,
        Key::AltRight => Key::MetaRight,
        Key::MetaRight => Key::AltRight,
        other => other,
    }
}

/// Whether `key`, captured on `source`, means anything when injected on
/// `target`. Every named [`Key`] does. A [`Key::Unknown`] does not travel:
/// it carries a raw code from `source`'s own key-code space, and injected
/// on any other platform that number is simply reinterpreted as whatever
/// *that* platform assigns to it.
///
/// **Found via the Phase 4 real-hardware acceptance run** (ADR-0007's
/// 2026-09-12 update): with punctuation and numpad keys not yet named, a
/// Mac keypad 9 left as `Unknown(0x5C)` and Windows pressed `VK_RWIN` —
/// the Windows key — while Mac keypad 0 typed `R`, `[` pressed Page Up
/// and `-` pressed Escape. Naming those keys fixed them; this rule is what
/// stops the *next* unnamed key from doing the same, however many more
/// keys get named later. A same-platform `Unknown` still round-trips
/// exactly, preserving decision 1's "never silently dropped" guarantee
/// where the code actually means something.
pub(crate) fn is_injectable(key: Key, source: PlatformKind, target: PlatformKind) -> bool {
    !matches!(key, Key::Unknown(_)) || source == target
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLATFORMS: [PlatformKind; 3] = [
        PlatformKind::MacOs,
        PlatformKind::Windows,
        PlatformKind::Linux,
    ];
    const PCS: [PlatformKind; 2] = [PlatformKind::Windows, PlatformKind::Linux];

    #[test]
    fn same_platform_is_always_identity() {
        for platform in PLATFORMS {
            for key in all_test_keys() {
                assert_eq!(
                    translate_for_target(key, platform, platform),
                    key,
                    "key {key:?} on {platform:?} -> {platform:?} must be unchanged"
                );
            }
        }
    }

    #[test]
    fn windows_and_linux_never_translate_between_each_other() {
        for key in all_test_keys() {
            assert_eq!(
                translate_for_target(key, PlatformKind::Windows, PlatformKind::Linux),
                key
            );
            assert_eq!(
                translate_for_target(key, PlatformKind::Linux, PlatformKind::Windows),
                key
            );
        }
    }

    /// Regression test for the real-hardware report that the Windows key
    /// (Linux "Show Apps", the Windows Start menu) could not be pressed from
    /// the Mac at all: Option — the Windows key's spot — now sends it.
    #[test]
    fn mac_option_becomes_the_windows_key_on_a_pc() {
        for target in PCS {
            assert_eq!(
                translate_for_target(Key::AltLeft, PlatformKind::MacOs, target),
                Key::MetaLeft
            );
            assert_eq!(
                translate_for_target(Key::AltRight, PlatformKind::MacOs, target),
                Key::MetaRight
            );
        }
    }

    #[test]
    fn mac_command_becomes_alt_on_a_pc() {
        for target in PCS {
            assert_eq!(
                translate_for_target(Key::MetaLeft, PlatformKind::MacOs, target),
                Key::AltLeft
            );
            assert_eq!(
                translate_for_target(Key::MetaRight, PlatformKind::MacOs, target),
                Key::AltRight
            );
        }
    }

    #[test]
    fn a_pc_windows_key_becomes_option_on_the_mac() {
        for source in PCS {
            assert_eq!(
                translate_for_target(Key::MetaLeft, source, PlatformKind::MacOs),
                Key::AltLeft
            );
            assert_eq!(
                translate_for_target(Key::MetaRight, source, PlatformKind::MacOs),
                Key::AltRight
            );
        }
    }

    /// Also the real-hardware Dock report: the PC's Windows key used to
    /// arrive as Command, so Windows key + click was a Command+click
    /// ("Show in Finder"). Now Alt is the key in the Command spot.
    #[test]
    fn a_pc_alt_becomes_command_on_the_mac() {
        for source in PCS {
            assert_eq!(
                translate_for_target(Key::AltLeft, source, PlatformKind::MacOs),
                Key::MetaLeft
            );
            assert_eq!(
                translate_for_target(Key::AltRight, source, PlatformKind::MacOs),
                Key::MetaRight
            );
        }
    }

    #[test]
    fn control_is_control_everywhere() {
        for (source, target) in all_cross_platform_pairs() {
            assert_eq!(
                translate_for_target(Key::ControlLeft, source, target),
                Key::ControlLeft
            );
            assert_eq!(
                translate_for_target(Key::ControlRight, source, target),
                Key::ControlRight
            );
        }
    }

    /// A key pressed on one machine and handed back unchanged by the other
    /// is the key that was pressed: the mapping is its own inverse.
    #[test]
    fn translating_there_and_back_is_identity() {
        for (source, target) in all_cross_platform_pairs() {
            for key in all_test_keys() {
                let there = translate_for_target(key, source, target);
                assert_eq!(
                    translate_for_target(there, target, source),
                    key,
                    "{key:?} {source:?}->{target:?}->{source:?}"
                );
            }
        }
    }

    #[test]
    fn shift_and_caps_lock_never_translate() {
        for (source, target) in all_cross_platform_pairs() {
            assert_eq!(
                translate_for_target(Key::ShiftLeft, source, target),
                Key::ShiftLeft
            );
            assert_eq!(
                translate_for_target(Key::ShiftRight, source, target),
                Key::ShiftRight
            );
            assert_eq!(
                translate_for_target(Key::CapsLock, source, target),
                Key::CapsLock
            );
        }
    }

    #[test]
    fn function_keys_never_translate() {
        for (source, target) in all_cross_platform_pairs() {
            for key in [
                Key::F1,
                Key::F2,
                Key::F3,
                Key::F4,
                Key::F5,
                Key::F6,
                Key::F7,
                Key::F8,
                Key::F9,
                Key::F10,
                Key::F11,
                Key::F12,
            ] {
                assert_eq!(translate_for_target(key, source, target), key);
            }
        }
    }

    #[test]
    fn printable_and_navigation_keys_never_translate() {
        for (source, target) in all_cross_platform_pairs() {
            for key in [
                Key::A,
                Key::Z,
                Key::Digit0,
                Key::Digit9,
                Key::ArrowUp,
                Key::ArrowDown,
                Key::ArrowLeft,
                Key::ArrowRight,
                Key::Enter,
                Key::Escape,
                Key::Backspace,
                Key::Delete,
                Key::Tab,
                Key::Space,
                Key::NumLock,
                Key::Numpad7,
            ] {
                assert_eq!(translate_for_target(key, source, target), key);
            }
        }
    }

    #[test]
    fn unknown_key_raw_code_is_preserved_across_translation() {
        for (source, target) in all_cross_platform_pairs() {
            assert_eq!(
                translate_for_target(Key::Unknown(12345), source, target),
                Key::Unknown(12345)
            );
        }
    }

    /// Regression test for the Phase 4 acceptance-run keyboard failure
    /// (ADR-0007's 2026-09-12 update), with the raw Mac codes from the
    /// real hub log. Each was injected on Windows as the key that happens
    /// to share its number; none may be injected on another platform.
    #[test]
    fn a_foreign_raw_key_code_from_the_acceptance_log_is_never_injectable() {
        for code in [
            0x5C, // Mac keypad 9 -> Windows pressed the Windows key (VK_RWIN)
            0x21, // Mac [        -> Windows pressed Page Up
            0x1B, // Mac -        -> Windows pressed Escape
            0x2C, // Mac /        -> Windows pressed Print Screen
            0x32, // Mac `        -> Windows typed 2
            0x4C, // Mac keypad Enter -> Windows typed L
            0x47, // Mac keypad Clear -> Windows typed G (named NumLock since 1.2;
                  // as a *raw* code it must still never be injected)
        ] {
            for target in PCS {
                assert!(
                    !is_injectable(Key::Unknown(code), PlatformKind::MacOs, target),
                    "raw macOS code {code:#x} must never be injected on {target:?}"
                );
            }
        }
    }

    #[test]
    fn a_raw_key_code_is_still_injectable_on_its_own_platform() {
        for platform in PLATFORMS {
            assert!(is_injectable(Key::Unknown(42), platform, platform));
        }
    }

    #[test]
    fn every_named_key_is_injectable_across_platforms() {
        for (source, target) in all_cross_platform_pairs() {
            for key in [
                Key::A,
                Key::Enter,
                Key::MetaLeft,
                Key::Backquote,
                Key::BracketLeft,
                Key::Numpad9,
                Key::NumpadEnter,
            ] {
                assert!(
                    is_injectable(key, source, target),
                    "{key:?} {source:?}->{target:?}"
                );
            }
        }
    }

    fn all_cross_platform_pairs() -> Vec<(PlatformKind, PlatformKind)> {
        let mut pairs = Vec::new();
        for source in PLATFORMS {
            for target in PLATFORMS {
                if source != target {
                    pairs.push((source, target));
                }
            }
        }
        pairs
    }

    fn all_test_keys() -> Vec<Key> {
        vec![
            Key::A,
            Key::Digit0,
            Key::ShiftLeft,
            Key::ShiftRight,
            Key::ControlLeft,
            Key::ControlRight,
            Key::AltLeft,
            Key::AltRight,
            Key::MetaLeft,
            Key::MetaRight,
            Key::CapsLock,
            Key::F1,
            Key::ArrowUp,
            Key::Enter,
            Key::Escape,
            Key::Backspace,
            Key::Delete,
            Key::Tab,
            Key::Space,
            Key::Unknown(42),
        ]
    }
}
