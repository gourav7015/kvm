//! X11 capture via XInput2 raw events. This file is the "thin shim"
//! ADR-0007 describes: it converts XI2 events to [`InputMessage`]s and
//! runs the connection's event loop — no cross-platform translation
//! logic lives here. See ADR-0008 for why XInput2 raw events (not
//! `XGrabKeyboard`/`XGrabPointer`, which exclusively steal input, or
//! the older XRecord extension) were chosen for this.
//!
//! **Local suppression while forwarding (ADR-0009 decision 24).** Raw
//! events can't be discarded one by one the way a macOS/Windows hook
//! can, so while this machine forwards its input elsewhere the capture
//! thread takes the exclusive keyboard and pointer grabs ADR-0008 ruled
//! out for an *always-on* session — only for the duration of
//! `Forwarding`, released on return, with an invisible cursor so this
//! screen's own arrow disappears too. Raw events keep arriving while
//! grabbed (and this client holds the grab anyway), so forwarding, the
//! emergency chord and edge detection are unaffected. If this process
//! dies, the X server drops the grabs with its connection.

use std::collections::HashSet;
use std::thread;

use kvm_protocol::InputMessage;
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xinput::{ConnectionExt as _, EventMask, XIEventMask};
use x11rb::protocol::xproto::{
    ClientMessageEvent, ConnectionExt as _, CreateGCAux, CreateWindowAux, Cursor,
    EventMask as CoreEventMask, GrabMode, GrabStatus, KeyButMask, Rectangle, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;

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

/// `XIAllMasterDevices`, per the XInput2 protocol spec — selects raw
/// events from every *logical* (master) keyboard/pointer pair.
///
/// Deliberately not `XIAllDevices` (value `0`): a real hardware run
/// found that selecting raw events on `XIAllDevices` delivers every
/// event twice on this machine — a known XInput2 pitfall, since
/// `XIAllDevices` additionally matches each underlying physical/slave
/// device a master is paired with, not just the master's own merged
/// stream. `XIAllMasterDevices` receives exactly one raw event per
/// logical device pair (normally "Virtual core keyboard" +
/// "Virtual core pointer"), which is what a capture tool actually
/// wants — one event per real keypress/click, not one per device layer
/// it passed through.
const XI_ALL_MASTER_DEVICES: u8 = 1;

/// `XIAllDevices`, per the XInput2 protocol spec — used only to *list*
/// devices (`XIQueryDevice`), never to select events (see above).
const XI_ALL_DEVICES: u16 = 0;

/// A request from another thread to the capture thread, delivered as the
/// first 32-bit word of a `ClientMessage` on the capture thread's own
/// window — the only way into its blocking event loop without sharing
/// its X connection across threads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureCommand {
    /// Word 0, so the original all-zero stop message keeps its meaning.
    Stop,
    /// Take the keyboard and pointer grabs (entering `Forwarding`).
    Suppress,
    /// Release them (back to `Local`).
    Release,
}

impl CaptureCommand {
    fn to_word(self) -> u32 {
        match self {
            CaptureCommand::Stop => 0,
            CaptureCommand::Suppress => 1,
            CaptureCommand::Release => 2,
        }
    }

    fn from_word(word: u32) -> Option<Self> {
        match word {
            0 => Some(CaptureCommand::Stop),
            1 => Some(CaptureCommand::Suppress),
            2 => Some(CaptureCommand::Release),
            _ => None,
        }
    }
}

/// Whether an input device is one of the X server's XTEST devices
/// ("Virtual core XTEST pointer"/"... keyboard"): everything injected via
/// XTEST — including this process's own cursor warps on switching
/// (`RecenterLocal`, `LandLocal`) — arrives as raw events from them. Those
/// are ignored, so a warp can never be captured back as user motion (the
/// macOS equivalent is `SYNTHETIC_EVENT_MARKER`; ADR-0009 decision 24).
/// Input from other XTEST users (e.g. `xdotool`) is ignored too.
fn is_xtest_device_name(name: &[u8]) -> bool {
    name.windows(b"XTEST".len())
        .any(|window| window == b"XTEST")
}

/// Global capture of this machine's keyboard and mouse via XInput2 raw
/// events, selected on the root window: listen-only while `Local`,
/// grabbed while `Forwarding` (see the module doc). Requires an X11
/// session reachable via the standard `DISPLAY`/`XAUTHORITY` environment
/// — no elevated privileges beyond ordinary X client access (see
/// ADR-0008; this is a real, if permissive, difference from Wayland's
/// security model).
#[derive(Default)]
pub struct X11Capture {
    thread: Option<thread::JoinHandle<()>>,
    /// The dummy window the capture thread's connection listens for
    /// `ClientMessage` commands on — senders open their *own*, separate X
    /// connection to send one, rather than sharing the capture thread's
    /// connection across threads.
    stop_window: Option<u32>,
    /// Whether the capture thread has been told to hold its grabs.
    suppressed: bool,
}

impl X11Capture {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sends `command` to the capture thread. Returns whether it was sent.
    fn send_command(&self, command: CaptureCommand) -> bool {
        let Some(window) = self.stop_window else {
            return false;
        };
        // A fresh, separate connection purely to deliver the message —
        // deliberately not sharing the capture thread's connection.
        let Ok((conn, _)) = x11rb::connect(None) else {
            return false;
        };
        let event = ClientMessageEvent::new(32, window, 0u32, [command.to_word(), 0, 0, 0, 0]);
        let sent = x11rb::protocol::xproto::send_event(
            &conn,
            false,
            window,
            CoreEventMask::NO_EVENT,
            event,
        )
        .is_ok();
        sent && conn.flush().is_ok()
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
        // The capture thread releases any grab on its way out; its
        // connection closing would drop them anyway.
        self.send_command(CaptureCommand::Stop);
        self.stop_window = None;
        self.suppressed = false;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }

    fn set_local_suppression(&mut self, suppress: bool) {
        tracing::info!(suppress, "set_local_suppression called");
        if suppress == self.suppressed {
            return;
        }
        let command = if suppress {
            CaptureCommand::Suppress
        } else {
            CaptureCommand::Release
        };
        if self.send_command(command) {
            self.suppressed = suppress;
        } else {
            tracing::warn!(
                suppress,
                "could not reach the X11 capture thread to change local suppression"
            );
        }
    }

    fn caps_lock_state(&self) -> Option<bool> {
        // Once per switch, so a short-lived connection is fine; the Lock
        // modifier in the pointer's modifier mask is the X server's own
        // Caps Lock state.
        let (conn, screen_num) = x11rb::connect(None).ok()?;
        let root = conn.setup().roots.get(screen_num)?.root;
        let reply = conn.query_pointer(root).ok()?.reply().ok()?;
        Some(u16::from(reply.mask) & u16::from(KeyButMask::LOCK) != 0)
    }
}

/// Which of the two exclusive grabs the capture thread currently holds,
/// and whether it should be holding them.
#[derive(Debug, Default)]
struct GrabState {
    wanted: bool,
    keyboard: bool,
    pointer: bool,
}

impl GrabState {
    fn held(&self) -> bool {
        self.keyboard && self.pointer
    }
}

/// Takes whichever grab isn't held yet. A grab can fail — most often
/// because another client holds an active grab, e.g. a button still down
/// in the middle of a drag — so the capture loop retries on later input
/// until both are held; `report_failure` keeps those retries quiet.
fn acquire_grabs(
    conn: &RustConnection,
    root: Window,
    cursor: Cursor,
    state: &mut GrabState,
    report_failure: bool,
) {
    let was_held = state.held();
    if !state.keyboard {
        state.keyboard = conn
            .grab_keyboard(
                false,
                root,
                x11rb::CURRENT_TIME,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
            )
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .is_some_and(|reply| reply.status == GrabStatus::SUCCESS);
    }
    if !state.pointer {
        state.pointer = conn
            .grab_pointer(
                false,
                root,
                CoreEventMask::NO_EVENT,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
                x11rb::NONE,
                cursor,
                x11rb::CURRENT_TIME,
            )
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .is_some_and(|reply| reply.status == GrabStatus::SUCCESS);
    }
    if state.held() && !was_held {
        tracing::info!("local input grabbed -- this screen no longer acts on it while forwarding");
    } else if !state.held() && report_failure {
        tracing::warn!(
            keyboard = state.keyboard,
            pointer = state.pointer,
            "could not take every local input grab yet (another client holds one?) -- \
             retrying on the next input"
        );
    }
}

fn release_grabs(conn: &RustConnection, state: &mut GrabState) {
    let had_any = state.keyboard || state.pointer;
    if state.keyboard {
        let _ = conn.ungrab_keyboard(x11rb::CURRENT_TIME);
    }
    if state.pointer {
        let _ = conn.ungrab_pointer(x11rb::CURRENT_TIME);
    }
    let _ = conn.flush();
    *state = GrabState::default();
    if had_any {
        tracing::info!("local input grabs released");
    }
}

/// A fully transparent 1x1 cursor, shown while the pointer is grabbed so
/// this screen's own arrow disappears while forwarding (the macOS
/// equivalent is decision 20's cursor hiding).
fn create_blank_cursor(conn: &RustConnection, root: Window) -> Result<Cursor, String> {
    let id = |what: &str| {
        conn.generate_id()
            .map_err(|e| format!("failed to allocate an id for the blank cursor's {what}: {e}"))
    };
    let pixmap = id("pixmap")?;
    let gc = id("graphics context")?;
    let cursor = id("cursor")?;
    let fail = |e: x11rb::errors::ConnectionError| format!("blank cursor setup failed: {e}");
    conn.create_pixmap(1, pixmap, root, 1, 1).map_err(fail)?;
    conn.create_gc(gc, pixmap, &CreateGCAux::new().foreground(0))
        .map_err(fail)?;
    conn.poly_fill_rectangle(
        pixmap,
        gc,
        &[Rectangle {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        }],
    )
    .map_err(fail)?;
    // Source and mask both the cleared pixmap: every pixel transparent.
    conn.create_cursor(cursor, pixmap, pixmap, 0, 0, 0, 0, 0, 0, 0, 0)
        .map_err(fail)?;
    conn.free_gc(gc).map_err(fail)?;
    conn.free_pixmap(pixmap).map_err(fail)?;
    Ok(cursor)
}

/// The device ids of the X server's XTEST devices — see
/// [`is_xtest_device_name`].
fn xtest_device_ids(conn: &RustConnection) -> Result<HashSet<u16>, String> {
    let reply = conn
        .xinput_xi_query_device(XI_ALL_DEVICES)
        .map_err(|e| format!("XIQueryDevice request failed: {e}"))?
        .reply()
        .map_err(|e| format!("XIQueryDevice reply failed: {e}"))?;
    let ids: HashSet<u16> = reply
        .infos
        .iter()
        .filter(|info| is_xtest_device_name(&info.name))
        .map(|info| info.deviceid)
        .collect();
    tracing::info!(
        ?ids,
        "X11 capture: ignoring raw events from XTEST devices (this process's own cursor warps)"
    );
    Ok(ids)
}

/// Stage 1 of the pipeline trace (same shape as the macOS capture's),
/// with the pointer's real on-screen position beside the captured delta
/// so a hub-side drift between the two is measurable. Costs one
/// round-trip, so only when debug logging is on.
fn log_motion(conn: &RustConnection, root: Window, message: &InputMessage) {
    let InputMessage::MouseMove { dx, dy } = *message else {
        return;
    };
    if let Some(reply) = conn
        .query_pointer(root)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
    {
        tracing::debug!(
            stage = "1-capture",
            loc_x = reply.root_x,
            loc_y = reply.root_y,
            dx,
            dy,
            "X11 capture: mouse motion"
        );
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
            deviceid: XI_ALL_MASTER_DEVICES.into(),
            mask: vec![raw_event_mask()],
        }],
    )
    .map_err(|e| format!("XISelectEvents request failed: {e}"))?;

    let keymap = KeyMap::query(&conn)?;
    let xtest_devices = xtest_device_ids(&conn)?;
    let blank_cursor = create_blank_cursor(&conn, root)?;

    conn.flush()
        .map_err(|e| format!("failed to flush setup requests: {e}"))?;

    if setup_tx.send(Ok(stop_window)).is_err() {
        return Ok(()); // Caller already gave up waiting.
    }

    let mut grab = GrabState::default();
    loop {
        let event = conn
            .wait_for_event()
            .map_err(|e| format!("X11 connection error: {e}"))?;
        match event {
            Event::XinputRawKeyPress(e) if !xtest_devices.contains(&e.sourceid) => {
                let _ = sink.send(key_event_to_message(&keymap, &e, true));
            }
            Event::XinputRawKeyRelease(e) if !xtest_devices.contains(&e.sourceid) => {
                let _ = sink.send(key_event_to_message(&keymap, &e, false));
            }
            Event::XinputRawButtonPress(e) if !xtest_devices.contains(&e.sourceid) => {
                if let Some(message) = scroll_delta_for_button(&e) {
                    let _ = sink.send(message);
                } else if let Some(message) = button_event_to_message(&e, true) {
                    let _ = sink.send(message);
                }
            }
            Event::XinputRawButtonRelease(e) if !xtest_devices.contains(&e.sourceid) => {
                if let Some(message) = button_event_to_message(&e, false) {
                    let _ = sink.send(message);
                }
                // A released button is the usual moment a blocked grab
                // (another client's drag) becomes possible.
                if grab.wanted && !grab.held() {
                    acquire_grabs(&conn, root, blank_cursor, &mut grab, false);
                }
            }
            Event::XinputRawMotion(e) if !xtest_devices.contains(&e.sourceid) => {
                if let Some(message) = motion_event_to_message(&e) {
                    if tracing::enabled!(tracing::Level::DEBUG) {
                        log_motion(&conn, root, &message);
                    }
                    let _ = sink.send(message);
                }
                if grab.wanted && !grab.held() {
                    acquire_grabs(&conn, root, blank_cursor, &mut grab, false);
                }
            }
            Event::ClientMessage(e) if e.window == stop_window => {
                match CaptureCommand::from_word(e.data.as_data32()[0]) {
                    Some(CaptureCommand::Stop) => break,
                    Some(CaptureCommand::Suppress) => {
                        grab.wanted = true;
                        acquire_grabs(&conn, root, blank_cursor, &mut grab, true);
                    }
                    Some(CaptureCommand::Release) => release_grabs(&conn, &mut grab),
                    None => tracing::warn!(
                        word = e.data.as_data32()[0],
                        "unknown X11 capture command -- ignored"
                    ),
                }
            }
            _ => {}
        }
    }

    release_grabs(&conn, &mut grab);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_commands_round_trip_through_their_message_word() {
        for command in [
            CaptureCommand::Stop,
            CaptureCommand::Suppress,
            CaptureCommand::Release,
        ] {
            assert_eq!(CaptureCommand::from_word(command.to_word()), Some(command));
        }
        assert_eq!(CaptureCommand::from_word(99), None);
    }

    /// `stop()` used to send an all-zero message; word 0 must still mean
    /// stop, so a capture thread can always be shut down.
    #[test]
    fn an_all_zero_message_still_means_stop() {
        assert_eq!(CaptureCommand::from_word(0), Some(CaptureCommand::Stop));
    }

    /// Regression guard for the macOS decision-18 failure class: this
    /// process's own warps must never be captured back as user motion.
    #[test]
    fn the_x_servers_xtest_devices_are_recognised_and_real_ones_are_not() {
        assert!(is_xtest_device_name(b"Virtual core XTEST pointer"));
        assert!(is_xtest_device_name(b"Virtual core XTEST keyboard"));
        assert!(!is_xtest_device_name(b"Virtual core pointer"));
        assert!(!is_xtest_device_name(b"Virtual core keyboard"));
        assert!(!is_xtest_device_name(b"SynPS/2 Synaptics TouchPad"));
        assert!(!is_xtest_device_name(b"Logitech USB Receiver"));
    }

    #[test]
    fn both_grabs_are_needed_to_count_as_held() {
        let mut state = GrabState::default();
        assert!(!state.held());
        state.keyboard = true;
        assert!(!state.held());
        state.pointer = true;
        assert!(state.held());
    }
}
