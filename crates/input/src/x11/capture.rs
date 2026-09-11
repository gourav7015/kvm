//! X11 capture via XInput2 raw events. This file is the "thin shim"
//! ADR-0007 describes: it converts XI2 events to [`InputMessage`]s and
//! runs the connection's event loop — no cross-platform translation
//! logic lives here. See ADR-0008 for why XInput2 raw events (not
//! `XGrabKeyboard`/`XGrabPointer`, which exclusively steal input, or
//! the older XRecord extension) were chosen for this.

use std::thread;

use kvm_protocol::InputMessage;
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xinput::{ConnectionExt as _, EventMask, XIEventMask};
use x11rb::protocol::xproto::{
    ClientMessageEvent, CreateWindowAux, EventMask as CoreEventMask, WindowClass,
};

use crate::error::InputError;
use crate::traits::Capture;
use crate::x11::events::{
    button_event_to_message, key_event_to_message, motion_event_to_message, scroll_delta_for_button,
};
use crate::x11::keymap::KeyMap;

/// The XI2 raw events this backend needs — see ADR-0008 decision 1 for
/// why raw events (not the exclusive-grab APIs) are the right choice
/// for a listen-only, system-wide capture.
fn raw_event_mask() -> XIEventMask {
    XIEventMask::RAW_KEY_PRESS
        | XIEventMask::RAW_KEY_RELEASE
        | XIEventMask::RAW_BUTTON_PRESS
        | XIEventMask::RAW_BUTTON_RELEASE
        | XIEventMask::RAW_MOTION
}

/// `XIAllDevices`, per the XInput2 protocol spec — selects raw events
/// from every input device, not just one specific keyboard/pointer.
const XI_ALL_DEVICES: u8 = 0;

/// Global, listen-only capture of this machine's keyboard and mouse via
/// XInput2 raw events, selected on the root window. Requires an X11
/// session reachable via the standard `DISPLAY`/`XAUTHORITY`
/// environment — no elevated privileges beyond ordinary X client access
/// (see ADR-0008; this is a real, if permissive, difference from
/// Wayland's security model).
#[derive(Default)]
pub struct X11Capture {
    thread: Option<thread::JoinHandle<()>>,
    /// The dummy window the capture thread's connection listens for a
    /// stop `ClientMessage` on — `stop()` opens its *own*, separate X
    /// connection to send that message, rather than sharing the
    /// capture thread's connection across threads.
    stop_window: Option<u32>,
}

impl X11Capture {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Capture for X11Capture {
    fn start(&mut self, sink: std::sync::mpsc::Sender<InputMessage>) -> Result<(), InputError> {
        let (setup_tx, setup_rx) = std::sync::mpsc::channel::<Result<u32, String>>();

        let join_handle = thread::Builder::new()
            .name("kvm-input-x11-capture".to_string())
            .spawn(move || {
                if let Err(message) = run(sink, &setup_tx) {
                    let _ = setup_tx.send(Err(message));
                }
            })
            .map_err(|e| InputError::CaptureFailed(e.to_string()))?;

        match setup_rx.recv() {
            Ok(Ok(stop_window)) => {
                self.thread = Some(join_handle);
                self.stop_window = Some(stop_window);
                Ok(())
            }
            Ok(Err(message)) => Err(InputError::CaptureFailed(message)),
            Err(_) => Err(InputError::CaptureFailed(
                "capture thread exited before completing setup".to_string(),
            )),
        }
    }

    fn stop(&mut self) {
        if let Some(stop_window) = self.stop_window.take() {
            // A fresh, separate connection purely to deliver the wakeup
            // — deliberately not sharing the capture thread's
            // connection across threads.
            if let Ok((conn, _)) = x11rb::connect(None) {
                let event = ClientMessageEvent::new(32, stop_window, 0u32, [0u8; 20]);
                let _ = x11rb::protocol::xproto::send_event(
                    &conn,
                    false,
                    stop_window,
                    CoreEventMask::NO_EVENT,
                    event,
                );
                let _ = conn.flush();
            }
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Runs on the dedicated capture thread: owns the X connection for the
/// whole capture session, from setup through the blocking event loop.
fn run(
    sink: std::sync::mpsc::Sender<InputMessage>,
    setup_tx: &std::sync::mpsc::Sender<Result<u32, String>>,
) -> Result<(), String> {
    let (conn, screen_num) =
        x11rb::connect(None).map_err(|e| format!("failed to connect to the X server: {e}"))?;
    let root = conn.setup().roots[screen_num].root;

    // XInput2 must be negotiated before any xi_* request is valid.
    conn.xinput_xi_query_version(2, 2)
        .map_err(|e| format!("XIQueryVersion request failed: {e}"))?
        .reply()
        .map_err(|e| format!("XInput2 extension unavailable: {e}"))?;

    let stop_window = conn
        .generate_id()
        .map_err(|e| format!("failed to allocate a window id: {e}"))?;
    x11rb::protocol::xproto::create_window(
        &conn,
        x11rb::COPY_DEPTH_FROM_PARENT,
        stop_window,
        root,
        0,
        0,
        1,
        1,
        0,
        WindowClass::INPUT_ONLY,
        x11rb::COPY_FROM_PARENT,
        &CreateWindowAux::default(),
    )
    .map_err(|e| format!("failed to create the stop-signal window: {e}"))?;

    conn.xinput_xi_select_events(
        root,
        &[EventMask {
            deviceid: XI_ALL_DEVICES.into(),
            mask: vec![raw_event_mask()],
        }],
    )
    .map_err(|e| format!("XISelectEvents request failed: {e}"))?;

    let keymap = KeyMap::query(&conn)?;

    conn.flush()
        .map_err(|e| format!("failed to flush setup requests: {e}"))?;

    if setup_tx.send(Ok(stop_window)).is_err() {
        return Ok(()); // Caller already gave up waiting.
    }

    loop {
        let event = conn
            .wait_for_event()
            .map_err(|e| format!("X11 connection error: {e}"))?;
        match event {
            Event::XinputRawKeyPress(e) => {
                let _ = sink.send(key_event_to_message(&keymap, &e, true));
            }
            Event::XinputRawKeyRelease(e) => {
                let _ = sink.send(key_event_to_message(&keymap, &e, false));
            }
            Event::XinputRawButtonPress(e) => {
                if let Some(message) = scroll_delta_for_button(&e) {
                    let _ = sink.send(message);
                } else if let Some(message) = button_event_to_message(&e, true) {
                    let _ = sink.send(message);
                }
            }
            Event::XinputRawButtonRelease(e) => {
                if let Some(message) = button_event_to_message(&e, false) {
                    let _ = sink.send(message);
                }
            }
            Event::XinputRawMotion(e) => {
                if let Some(message) = motion_event_to_message(&e) {
                    let _ = sink.send(message);
                }
            }
            Event::ClientMessage(e) if e.window == stop_window => break,
            _ => {}
        }
    }

    Ok(())
}
