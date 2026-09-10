//! Windows capture via global low-level hooks (`WH_KEYBOARD_LL` /
//! `WH_MOUSE_LL`). This file is the "thin shim" ADR-0007 describes: it
//! converts hook callback data to [`InputMessage`]s and runs the message
//! pump — no cross-platform translation logic lives here.
//!
//! Low-level hook callbacks are bare `unsafe extern "system" fn` pointers
//! — they cannot capture closure state — so the active capture sink and
//! last-known mouse position are smuggled through process-wide statics,
//! set when capture starts and cleared when it stops. This is the
//! standard shape for this Windows API; the alternative (per-instance
//! state) isn't available since Windows itself calls a plain function
//! pointer, not a closure.

use std::sync::{Mutex, mpsc};
use std::thread;

use kvm_protocol::{ButtonState, InputMessage, MouseButton, PlatformKind};
use windows::Win32::Foundation::{LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, HHOOK, KBDLLHOOKSTRUCT, MSG, MSLLHOOKSTRUCT,
    PostThreadMessageW, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, WH_KEYBOARD_LL,
    WH_MOUSE_LL, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP,
    WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_QUIT, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use crate::error::InputError;
use crate::traits::Capture;
use crate::windows::keycode::vk_to_key;

/// The active capture's event sink, if capture is running. `None` when
/// stopped, so a hook callback firing after `stop()` (a benign race
/// during teardown) is a silent no-op rather than a panic or a send to a
/// closed channel.
static SINK: Mutex<Option<mpsc::Sender<InputMessage>>> = Mutex::new(None);

/// Last absolute cursor position seen by the mouse hook, used to turn
/// Windows' absolute `WM_MOUSEMOVE` coordinates into the relative deltas
/// [`InputMessage::MouseMove`] carries. `None` right after (re)starting
/// capture, until the first move event establishes a baseline.
static LAST_MOUSE_POS: Mutex<Option<POINT>> = Mutex::new(None);

fn send(message: InputMessage) {
    if let Ok(guard) = SINK.lock()
        && let Some(sender) = guard.as_ref()
    {
        let _ = sender.send(message);
    }
}

unsafe extern "system" fn keyboard_hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        // SAFETY: the OS guarantees `lparam` points to a valid
        // `KBDLLHOOKSTRUCT` for the duration of this call whenever
        // `code >= 0`, per the documented `LowLevelKeyboardProc` contract.
        let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let message = wparam.0 as u32;
        let state = if message == WM_KEYDOWN || message == WM_SYSKEYDOWN {
            Some(ButtonState::Pressed)
        } else if message == WM_KEYUP || message == WM_SYSKEYUP {
            Some(ButtonState::Released)
        } else {
            None
        };
        if let Some(state) = state {
            let key = vk_to_key(VIRTUAL_KEY(info.vkCode as u16));
            // The low-level keyboard hook doesn't directly report
            // auto-repeat; distinguishing it from a fresh press requires
            // tracking whether the key was already down, which this
            // first pass doesn't do — documented gap (see the crate's
            // manual QA doc), matching macOS's FlagsChanged omission
            // rather than guessing.
            send(InputMessage::Key {
                key,
                state,
                repeat: false,
                source_os: PlatformKind::Windows,
            });
        }
    }
    // SAFETY: passing `None` for `hhk` lets Windows resolve the next hook
    // in this hook chain itself; `code`/`wparam`/`lparam` are forwarded
    // unchanged, as `LowLevelKeyboardProc`'s contract requires for a
    // well-behaved chained hook.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe extern "system" fn mouse_hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        // SAFETY: see `keyboard_hook_proc` — the same contract applies to
        // `LowLevelMouseProc` and `MSLLHOOKSTRUCT`.
        let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        let message = wparam.0 as u32;

        match message {
            m if m == WM_MOUSEMOVE => {
                let mut last = LAST_MOUSE_POS.lock().ok();
                if let Some(last_pos) = last.as_mut() {
                    if let Some(previous) = **last_pos {
                        send(InputMessage::MouseMove {
                            dx: info.pt.x - previous.x,
                            dy: info.pt.y - previous.y,
                        });
                    }
                    **last_pos = Some(info.pt);
                }
            }
            m if m == WM_LBUTTONDOWN => send(mouse_button(MouseButton::Left, ButtonState::Pressed)),
            m if m == WM_LBUTTONUP => send(mouse_button(MouseButton::Left, ButtonState::Released)),
            m if m == WM_RBUTTONDOWN => {
                send(mouse_button(MouseButton::Right, ButtonState::Pressed))
            }
            m if m == WM_RBUTTONUP => send(mouse_button(MouseButton::Right, ButtonState::Released)),
            m if m == WM_MBUTTONDOWN => {
                send(mouse_button(MouseButton::Middle, ButtonState::Pressed));
            }
            m if m == WM_MBUTTONUP => {
                send(mouse_button(MouseButton::Middle, ButtonState::Released));
            }
            m if m == WM_MOUSEWHEEL => {
                // The wheel delta is the signed high word of `mouseData`,
                // in multiples of WHEEL_DELTA (120) — the standard
                // extraction for this field.
                let delta = (info.mouseData >> 16) as i16 as i32;
                send(InputMessage::MouseScroll { dx: 0, dy: delta });
            }
            _ => {}
        }
    }
    // SAFETY: same contract as in `keyboard_hook_proc`.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn mouse_button(button: MouseButton, state: ButtonState) -> InputMessage {
    InputMessage::MouseButton { button, state }
}

/// Global, low-level capture of this machine's keyboard and mouse via
/// `WH_KEYBOARD_LL`/`WH_MOUSE_LL`. No elevation is requested — see
/// ADR-0007 §5 for the resulting, documented UAC limitation.
#[derive(Default)]
pub struct WindowsCapture {
    thread: Option<thread::JoinHandle<()>>,
    thread_id: Option<u32>,
}

impl WindowsCapture {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Capture for WindowsCapture {
    fn start(&mut self, sink: mpsc::Sender<InputMessage>) -> Result<(), InputError> {
        {
            let mut guard = SINK
                .lock()
                .map_err(|_| InputError::CaptureFailed("capture sink lock poisoned".to_string()))?;
            *guard = Some(sink);
        }
        {
            let mut last = LAST_MOUSE_POS.lock().map_err(|_| {
                InputError::CaptureFailed("mouse position lock poisoned".to_string())
            })?;
            *last = None;
        }

        let (setup_tx, setup_rx) = mpsc::channel::<Result<u32, String>>();

        let join_handle = thread::Builder::new()
            .name("kvm-input-windows-capture".to_string())
            .spawn(move || {
                // SAFETY: both hook procs are well-formed
                // `LowLevelKeyboardProc`/`LowLevelMouseProc` callbacks
                // (see their own SAFETY comments); `hmod: None` and
                // `dwthreadid: 0` are correct for a global low-level
                // hook installed by the calling (this) thread, per the
                // documented `SetWindowsHookExW` contract for
                // `WH_KEYBOARD_LL`/`WH_MOUSE_LL`.
                let keyboard_hook =
                    unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook_proc), None, 0) };
                let mouse_hook =
                    unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook_proc), None, 0) };

                let (keyboard_hook, mouse_hook) = match (keyboard_hook, mouse_hook) {
                    (Ok(k), Ok(m)) => (k, m),
                    _ => {
                        let _ = setup_tx
                            .send(Err("failed to install keyboard/mouse hooks".to_string()));
                        return;
                    }
                };

                // SAFETY: `GetCurrentThreadId` has no preconditions.
                let thread_id = unsafe { GetCurrentThreadId() };
                if setup_tx.send(Ok(thread_id)).is_err() {
                    unhook(keyboard_hook, mouse_hook);
                    return;
                }

                run_message_pump();
                unhook(keyboard_hook, mouse_hook);
            })
            .map_err(|e| InputError::CaptureFailed(e.to_string()))?;

        match setup_rx.recv() {
            Ok(Ok(thread_id)) => {
                self.thread = Some(join_handle);
                self.thread_id = Some(thread_id);
                Ok(())
            }
            Ok(Err(message)) => Err(InputError::CaptureFailed(message)),
            Err(_) => Err(InputError::CaptureFailed(
                "capture thread exited before completing setup".to_string(),
            )),
        }
    }

    fn stop(&mut self) {
        if let Some(thread_id) = self.thread_id.take() {
            // SAFETY: posting WM_QUIT to a valid thread ID is always
            // sound; a stale/exited thread ID simply makes this a no-op
            // (the OS reports failure, which we intentionally ignore).
            let _ = unsafe { PostThreadMessageW(thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        if let Ok(mut guard) = SINK.lock() {
            *guard = None;
        }
    }
}

fn unhook(keyboard_hook: HHOOK, mouse_hook: HHOOK) {
    // SAFETY: both handles were just returned by a successful
    // `SetWindowsHookExW` call on this same thread and haven't been
    // unhooked yet.
    unsafe {
        let _ = UnhookWindowsHookEx(keyboard_hook);
        let _ = UnhookWindowsHookEx(mouse_hook);
    }
}

fn run_message_pump() {
    let mut msg = MSG::default();
    loop {
        // SAFETY: `msg` is a valid, appropriately-sized out parameter;
        // `hwnd: None` and the zero filter range are the documented way
        // to receive all messages for this thread, which is exactly what
        // a low-level hook's owning thread needs to pump.
        let result = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if !result.as_bool() || msg.message == WM_QUIT {
            break;
        }
        // SAFETY: `msg` was just populated by a successful `GetMessageW`.
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
