//! Windows injection via `SendInput`. The other half of the "thin shim"
//! alongside [`crate::windows::capture`] — pure event-shape conversion,
//! no cross-platform translation logic.

use std::collections::HashMap;
use std::mem::size_of;

use kvm_protocol::{ButtonState, InputMessage, Key, MouseButton, PlatformKind};
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, MAPVK_VK_TO_VSC, MOUSEEVENTF_LEFTDOWN,
    MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT,
    MapVirtualKeyW, SendInput, VK_CAPITAL, VK_NUMLOCK,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN, SetCursorPos, XBUTTON1,
};

use crate::error::InputError;
use crate::traits::{Inject, PointerGeometry};
use crate::translate::is_injectable;
use crate::windows::keycode::{key_to_vk, numpad_vk};

/// Injects keyboard/mouse events on this machine via `SendInput`. No UAC
/// elevation is requested — see ADR-0007 §5 for the resulting, documented
/// limitation (this cannot inject into an elevated foreground window).
#[derive(Default)]
pub struct WindowsInject {
    /// The VK actually pressed for each number-pad key still held, so its
    /// release is always the same VK even if Num Lock changed in between
    /// (see [`numpad_vk`]) — otherwise a key could be left stuck down.
    numpad_held: HashMap<Key, u16>,
}

impl WindowsInject {
    pub fn new() -> Self {
        // Real fix for the pointer-range hardware bug (ADR-0009
        // decision 9) -- see crate::windows::dpi's module doc for the
        // full root-cause explanation. Must happen before any of the
        // GetSystemMetrics/GetCursorPos/SetCursorPos calls below.
        crate::windows::dpi::ensure_process_dpi_awareness();
        // Diagnostic tracing (kept, not just for this hunt): this
        // process's now-real, physical-pixel system DPI.
        // this process's system DPI. `GetSystemMetrics`/`GetCursorPos`/
        // `SetCursorPos` all report/accept coordinates in this same
        // process's DPI-awareness space -- if that space isn't 100%
        // scale (96 DPI), the "logical" screen size this process reads
        // is not the panel's native pixel resolution, which is a real
        // candidate for "only part of the screen is reachable" if
        // anything elsewhere in the pipeline assumes native pixels.
        // SAFETY: `GetDpiForSystem` takes no arguments and has no
        // preconditions.
        let dpi = unsafe { GetDpiForSystem() };
        tracing::info!(
            dpi,
            scale_percent = (dpi as f64 / 96.0 * 100.0) as u32,
            "WindowsInject: system DPI at construction"
        );
        Self::default()
    }
}

/// `KEYEVENTF_*` flags for one injected key event. Num Lock is sent as an
/// extended key: Microsoft's own `keybd_event` documentation toggles Num
/// Lock exactly that way (`KEYEVENTF_EXTENDEDKEY`).
fn key_flags(vk: u16, pressed: bool) -> KEYBD_EVENT_FLAGS {
    let mut flags = if pressed {
        KEYBD_EVENT_FLAGS(0)
    } else {
        KEYEVENTF_KEYUP
    };
    if vk == VK_NUMLOCK.0 {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    flags
}

/// The hardware scan code sent alongside `vk`: Num Lock's real one (looked
/// up with `MapVirtualKeyW`, not hard-coded), 0 for every other key.
/// Real hardware: Num Lock sent with the extended flag but scan code 0 did
/// not toggle Windows' Num Lock, while Microsoft's own `keybd_event`
/// sample for toggling it passes the key's scan code too — this matches
/// that sample exactly. Other keys already work with 0 and are unchanged.
fn key_scan_code(vk: u16) -> u16 {
    if vk != VK_NUMLOCK.0 {
        return 0;
    }
    // SAFETY: `MapVirtualKeyW` takes two plain integers and returns one;
    // no pointers or buffers are involved.
    unsafe { MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_VSC) as u16 }
}

/// Whether Windows currently has `vk`'s toggle (Caps/Num Lock) on — the
/// low bit of `GetKeyState`. Decides Caps Lock syncing and which VK a
/// number-pad key sends, and is logged around lock-key injections so a
/// real-hardware run shows whether a toggle happened.
fn toggle_state(vk: u16) -> bool {
    // SAFETY: `GetKeyState` takes a plain virtual-key code and returns a
    // plain integer; no pointers or buffers are involved.
    unsafe { GetKeyState(i32::from(vk)) & 1 == 1 }
}

fn keyboard_input(vk: u16, pressed: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY(vk),
                wScan: key_scan_code(vk),
                dwFlags: key_flags(vk, pressed),
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn mouse_input(dx: i32, dy: i32, mouse_data: u32, flags: u32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: mouse_data,
                dwFlags: windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS(flags),
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn send(input: INPUT) -> Result<(), InputError> {
    // SAFETY: `input` is a single, fully-initialized `INPUT` value (both
    // union fields are zeroed then explicitly filled by the callers
    // above), matching `SendInput`'s documented contract for a one-input
    // slice.
    let sent = unsafe { SendInput(&[input], size_of::<INPUT>() as i32) };
    if sent == 1 {
        Ok(())
    } else {
        Err(InputError::InjectFailed(
            "SendInput reported 0 events injected".to_string(),
        ))
    }
}

impl Inject for WindowsInject {
    fn inject(&mut self, event: &InputMessage) -> Result<(), InputError> {
        match *event {
            InputMessage::Key {
                key,
                state,
                source_os,
                ..
            } => {
                // A raw code from another platform is that platform's
                // number, not a Windows VK -- see `translate::is_injectable`.
                if !is_injectable(key, source_os, PlatformKind::Windows) {
                    return Err(InputError::Unsupported(format!(
                        "{key:?} is a raw {source_os:?} key code with no meaning on Windows -- not injected"
                    )));
                }
                // Number-pad keys follow this machine's own Num Lock (see
                // `numpad_vk`); a release always repeats its press's VK.
                let vk = match numpad_vk(key, toggle_state(VK_NUMLOCK.0)) {
                    Some(numpad) => match state {
                        ButtonState::Pressed => {
                            self.numpad_held.insert(key, numpad.0);
                            numpad.0
                        }
                        ButtonState::Released => self.numpad_held.remove(&key).unwrap_or(numpad.0),
                    },
                    None => {
                        key_to_vk(key)
                            .ok_or_else(|| {
                                InputError::Unsupported(format!("{key:?} has no Windows VK code"))
                            })?
                            .0
                    }
                };
                let is_lock_key = vk == VK_NUMLOCK.0 || vk == VK_CAPITAL.0;
                let before = is_lock_key.then(|| toggle_state(vk));
                let result = send(keyboard_input(vk, state == ButtonState::Pressed));
                if let Some(before) = before {
                    tracing::info!(
                        ?key,
                        ?state,
                        scan_code = key_scan_code(vk),
                        sent = result.is_ok(),
                        toggle_before = before,
                        toggle_after = toggle_state(vk),
                        "WindowsInject: lock key injected"
                    );
                }
                result
            }
            InputMessage::CapsLockState { on } => {
                // Match the sender's Caps Lock rather than toggle blindly --
                // see ADR-0007's Caps Lock update.
                let before = toggle_state(VK_CAPITAL.0);
                let result = if before == on {
                    Ok(())
                } else {
                    send(keyboard_input(VK_CAPITAL.0, true))
                        .and_then(|()| send(keyboard_input(VK_CAPITAL.0, false)))
                };
                tracing::info!(
                    desired = on,
                    toggle_before = before,
                    pressed = before != on,
                    sent = result.is_ok(),
                    toggle_after = toggle_state(VK_CAPITAL.0),
                    "WindowsInject: Caps Lock state synced"
                );
                result
            }
            InputMessage::MouseMove { dx, dy } => {
                // Real hardware finding (Mac->Windows QA, ADR-0009): a
                // relative SendInput move (MOUSEEVENTF_MOVE without
                // MOUSEEVENTF_ABSOLUTE) goes through the same pointer-
                // acceleration ("Enhance pointer precision") curve as a
                // real mouse -- a nonlinear, speed-dependent transform.
                // The sending side accumulates raw 1:1 deltas
                // (`Router`'s `virtual_cursor`), so accelerated relative
                // injection drifts away from that tracked position --
                // worse on fast swipes, which is exactly the reported
                // "sometimes more, sometimes less" inconsistency and
                // the cursor feeling boxed into a fraction of the real
                // screen. `SetCursorPos` (already used for
                // `PointerGeometry::set_cursor_position`) applies no
                // acceleration at all, so reading the real current
                // position and adding the delta keeps this path exactly
                // in sync with what the sending side expects.
                let mut point = POINT { x: 0, y: 0 };
                // SAFETY: `point` is a valid, fully-initialized `POINT`
                // the API writes into; matches `GetCursorPos`'s
                // documented contract.
                unsafe { GetCursorPos(&mut point) }
                    .map_err(|e| InputError::InjectFailed(format!("GetCursorPos failed: {e}")))?;
                let (target_x, target_y) = (point.x + dx, point.y + dy);
                // SAFETY: plain integer coordinates, no buffer/pointer
                // contract to uphold.
                let result = unsafe { SetCursorPos(target_x, target_y) }
                    .map_err(|e| InputError::InjectFailed(format!("SetCursorPos failed: {e}")));
                // TEMPORARY diagnostic tracing (pointer-range root-cause
                // hunt): read the position back immediately after
                // setting it. If this ever disagrees with
                // (target_x, target_y), the OS itself is clamping or
                // rescaling the write -- direct evidence of target-side
                // clamping/coordinate-space mismatch, not an inference.
                let mut after = POINT { x: 0, y: 0 };
                // SAFETY: same contract as the read above.
                let readback_ok = unsafe { GetCursorPos(&mut after) }.is_ok();
                tracing::info!(
                    stage = "4-windows-inject",
                    before = ?(point.x, point.y),
                    dx, dy,
                    intended = ?(target_x, target_y),
                    actual = ?(readback_ok.then_some((after.x, after.y))),
                    matched = readback_ok && (after.x, after.y) == (target_x, target_y),
                    "WindowsInject: relative move applied via GetCursorPos+SetCursorPos"
                );
                result
            }
            InputMessage::MouseButton { button, state } => {
                // "Other" extra buttons have no dedicated MOUSEEVENTF_*
                // flag; approximate with XBUTTON1 rather than failing
                // the whole event, matching the macOS backend's
                // Center-button approximation for the same case.
                let (flag, mouse_data) = match (button, state) {
                    (MouseButton::Left, ButtonState::Pressed) => (MOUSEEVENTF_LEFTDOWN, 0),
                    (MouseButton::Left, ButtonState::Released) => (MOUSEEVENTF_LEFTUP, 0),
                    (MouseButton::Right, ButtonState::Pressed) => (MOUSEEVENTF_RIGHTDOWN, 0),
                    (MouseButton::Right, ButtonState::Released) => (MOUSEEVENTF_RIGHTUP, 0),
                    (MouseButton::Middle, ButtonState::Pressed) => (MOUSEEVENTF_MIDDLEDOWN, 0),
                    (MouseButton::Middle, ButtonState::Released) => (MOUSEEVENTF_MIDDLEUP, 0),
                    (MouseButton::Other(_), ButtonState::Pressed) => {
                        (MOUSEEVENTF_XDOWN, u32::from(XBUTTON1))
                    }
                    (MouseButton::Other(_), ButtonState::Released) => {
                        (MOUSEEVENTF_XUP, u32::from(XBUTTON1))
                    }
                };
                send(mouse_input(0, 0, mouse_data, flag.0))
            }
            InputMessage::MouseScroll { dy, .. } => {
                // Windows has no horizontal-wheel equivalent wired up
                // here (MOUSEEVENTF_HWHEEL exists but the portable
                // `MouseScroll.dx` axis is left unsupported for now,
                // matching the macOS backend's vertical-first scope);
                // only the vertical delta is injected. `dy` is already in
                // `WHEEL_DELTA` units -- the protocol's scroll unit (see
                // `crate::scroll`), so it passes through unchanged.
                send(mouse_input(0, 0, dy as u32, MOUSEEVENTF_WHEEL.0))
            }
        }
    }
}

impl PointerGeometry for WindowsInject {
    fn cursor_position(&self) -> Result<(i32, i32), InputError> {
        let mut point = POINT { x: 0, y: 0 };
        // SAFETY: `point` is a valid, fully-initialized `POINT` the API
        // writes into; matches `GetCursorPos`'s documented contract.
        unsafe { GetCursorPos(&mut point) }
            .map_err(|e| InputError::InjectFailed(format!("GetCursorPos failed: {e}")))?;
        Ok((point.x, point.y))
    }

    fn screen_size(&self) -> Result<(u32, u32), InputError> {
        // SAFETY: `GetSystemMetrics` takes a plain enum value and
        // returns a plain integer, no buffer/pointer contract to
        // uphold.
        let (width, height) =
            unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
        if width <= 0 || height <= 0 {
            return Err(InputError::InjectFailed(
                "GetSystemMetrics reported a non-positive screen size".to_string(),
            ));
        }
        // TEMPORARY diagnostic tracing (pointer-range root-cause hunt):
        // this is the exact value exchanged over the wire and used by
        // the Mac's Router for its resolution-aware handoff math -- if
        // it disagrees with the panel's real native resolution (e.g.
        // because of DPI virtualization), every downstream rescale is
        // computed against the wrong denominator.
        tracing::info!(
            width,
            height,
            "WindowsInject: screen_size() reported (this value drives the Mac-side handoff rescale math)"
        );
        Ok((width as u32, height as u32))
    }

    fn set_cursor_position(&mut self, x: i32, y: i32) -> Result<(), InputError> {
        // SAFETY: `SetCursorPos` takes plain integer coordinates, no
        // buffer/pointer contract to uphold.
        let result = unsafe { SetCursorPos(x, y) }
            .map_err(|e| InputError::InjectFailed(format!("SetCursorPos failed: {e}")));
        // TEMPORARY diagnostic tracing (pointer-range root-cause hunt):
        // this is the SwitchActive warp -- the very first position the
        // cursor lands at on entering this screen. Read back
        // immediately to check whether the OS actually placed it where
        // asked.
        let mut after = POINT { x: 0, y: 0 };
        // SAFETY: same contract as every other GetCursorPos call above.
        let readback_ok = unsafe { GetCursorPos(&mut after) }.is_ok();
        tracing::info!(
            intended = ?(x, y),
            actual = ?(readback_ok.then_some((after.x, after.y))),
            matched = readback_ok && (after.x, after.y) == (x, y),
            "WindowsInject: set_cursor_position (handoff warp) applied"
        );
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn num_lock_is_sent_as_an_extended_key_both_ways() {
        assert_eq!(key_flags(VK_NUMLOCK.0, true), KEYEVENTF_EXTENDEDKEY);
        assert_eq!(
            key_flags(VK_NUMLOCK.0, false),
            KEYEVENTF_KEYUP | KEYEVENTF_EXTENDEDKEY
        );
    }

    #[test]
    fn ordinary_keys_are_not_extended() {
        assert_eq!(key_flags(0x41, true), KEYBD_EVENT_FLAGS(0)); // 'A'
        assert_eq!(key_flags(0x41, false), KEYEVENTF_KEYUP);
    }
}
