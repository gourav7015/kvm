//! Cross-platform modifier-role translation — the "killer feature": a
//! shortcut pressed on one OS should feel native on whichever OS is
//! receiving it. Pure, OS-independent, and the reason this file has no
//! `#[cfg(target_os = ...)]` anywhere in it — see ADR-0007 §3.
//!
//! The only asymmetry that exists between real desktop platforms is
//! which key plays the *primary* shortcut-modifier role: Command on
//! macOS, Control everywhere else (Windows and Linux both already agree
//! with each other). So translation only does something when exactly one
//! side of a (source, target) pair is macOS — it swaps Meta ↔ Control in
//! that case, and is the identity function otherwise, including for
//! every other key (letters, digits, function keys, navigation, Shift,
//! Alt/Option, Caps Lock).

use kvm_protocol::{Key, PlatformKind};

/// Translates `key`, as captured on `source`, into what should actually
/// be injected on `target`.
pub fn translate_for_target(key: Key, source: PlatformKind, target: PlatformKind) -> Key {
    if source == target {
        return key;
    }

    let source_is_mac = source == PlatformKind::MacOs;
    let target_is_mac = target == PlatformKind::MacOs;

    if source_is_mac == target_is_mac {
        // Neither side is macOS (Windows <-> Linux): both already treat
        // Control as the primary modifier, nothing to translate.
        return key;
    }

    if source_is_mac {
        // macOS -> Windows/Linux: Command becomes the primary modifier,
        // Control.
        match key {
            Key::MetaLeft => Key::ControlLeft,
            Key::MetaRight => Key::ControlRight,
            other => other,
        }
    } else {
        // Windows/Linux -> macOS: Control becomes the primary modifier,
        // Command.
        match key {
            Key::ControlLeft => Key::MetaLeft,
            Key::ControlRight => Key::MetaRight,
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLATFORMS: [PlatformKind; 3] = [
        PlatformKind::MacOs,
        PlatformKind::Windows,
        PlatformKind::Linux,
    ];

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

    #[test]
    fn mac_command_becomes_control_on_windows_and_linux() {
        for target in [PlatformKind::Windows, PlatformKind::Linux] {
            assert_eq!(
                translate_for_target(Key::MetaLeft, PlatformKind::MacOs, target),
                Key::ControlLeft
            );
            assert_eq!(
                translate_for_target(Key::MetaRight, PlatformKind::MacOs, target),
                Key::ControlRight
            );
        }
    }

    #[test]
    fn windows_and_linux_control_becomes_command_on_mac() {
        for source in [PlatformKind::Windows, PlatformKind::Linux] {
            assert_eq!(
                translate_for_target(Key::ControlLeft, source, PlatformKind::MacOs),
                Key::MetaLeft
            );
            assert_eq!(
                translate_for_target(Key::ControlRight, source, PlatformKind::MacOs),
                Key::MetaRight
            );
        }
    }

    #[test]
    fn mac_control_key_itself_is_not_remapped() {
        // macOS's actual physical Control key is a distinct key from
        // Command and must not also get swapped when translating outward
        // — only Meta maps to Control, Control itself stays Control.
        for target in [PlatformKind::Windows, PlatformKind::Linux] {
            assert_eq!(
                translate_for_target(Key::ControlLeft, PlatformKind::MacOs, target),
                Key::ControlLeft
            );
            assert_eq!(
                translate_for_target(Key::ControlRight, PlatformKind::MacOs, target),
                Key::ControlRight
            );
        }
    }

    #[test]
    fn windows_meta_key_itself_is_not_remapped_going_to_mac() {
        // Symmetric case: the Windows key (Meta on a PC keyboard) is
        // distinct from Control and must not also get swapped.
        for source in [PlatformKind::Windows, PlatformKind::Linux] {
            assert_eq!(
                translate_for_target(Key::MetaLeft, source, PlatformKind::MacOs),
                Key::MetaLeft
            );
            assert_eq!(
                translate_for_target(Key::MetaRight, source, PlatformKind::MacOs),
                Key::MetaRight
            );
        }
    }

    #[test]
    fn option_alt_never_translates_either_direction() {
        // Option (macOS) and Alt (Windows/Linux) already correspond in
        // role as the *secondary* modifier — no swap needed either way.
        for (source, target) in all_cross_platform_pairs() {
            assert_eq!(
                translate_for_target(Key::AltLeft, source, target),
                Key::AltLeft
            );
            assert_eq!(
                translate_for_target(Key::AltRight, source, target),
                Key::AltRight
            );
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
