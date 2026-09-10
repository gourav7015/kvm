//! Windows injection via `SendInput`. The other half of the "thin shim"
//! alongside [`crate::windows::capture`] — pure event-shape conversion,
//! no cross-platform translation logic.

use std::mem::size_of;

use kvm_protocol::{ButtonState, InputMessage, MouseButton};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP, MOUSEEVENTF_LEFTDOWN,
    MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE,
    MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN,
    MOUSEEVENTF_XUP, MOUSEINPUT, SendInput,
};
use windows::Win32::UI::WindowsAndMessaging::XBUTTON1;

use crate::error::InputError;
use crate::traits::Inject;
use crate::windows::keycode::key_to_vk;

/// Injects keyboard/mouse events on this machine via `SendInput`. No UAC
/// elevation is requested — see ADR-0007 §5 for the resulting, documented
/// limitation (this cannot inject into an elevated foreground window).
#[derive(Default)]
pub struct WindowsInject;

impl WindowsInject {
    pub fn new() -> Self {
        Self
    }
}

fn keyboard_input(vk: u16, pressed: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY(vk),
                wScan: 0,
                dwFlags: if pressed {
                    Default::default()
                } else {
                    KEYEVENTF_KEYUP
                },
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
            InputMessage::Key { key, state, .. } => {
                let vk = key_to_vk(key).ok_or_else(|| {
                    InputError::Unsupported(format!("{key:?} has no Windows VK code"))
                })?;
                send(keyboard_input(vk.0, state == ButtonState::Pressed))
            }
            InputMessage::MouseMove { dx, dy } => send(mouse_input(dx, dy, 0, MOUSEEVENTF_MOVE.0)),
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
                // only the vertical delta is injected.
                send(mouse_input(0, 0, dy as u32, MOUSEEVENTF_WHEEL.0))
            }
        }
    }
}
