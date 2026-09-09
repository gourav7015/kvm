//! Property-based round-trip coverage and the "never panics on arbitrary
//! bytes" guarantee — the mechanism that catches wire-format drift and
//! decoder panics that hand-picked unit tests would miss.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use kvm_protocol::{
    ButtonState, ClipboardContent, ClipboardMessage, ControlMessage, DecodeStatus,
    HandshakeMessage, InputMessage, Message, MouseButton, TransferMessage, decode_frame,
    encode_frame,
};
use proptest::prelude::*;

fn arb_device_id() -> impl Strategy<Value = [u8; 32]> {
    proptest::array::uniform32(any::<u8>())
}

fn arb_button_state() -> impl Strategy<Value = ButtonState> {
    prop_oneof![Just(ButtonState::Pressed), Just(ButtonState::Released)]
}

fn arb_mouse_button() -> impl Strategy<Value = MouseButton> {
    prop_oneof![
        Just(MouseButton::Left),
        Just(MouseButton::Right),
        Just(MouseButton::Middle),
        any::<u8>().prop_map(MouseButton::Other),
    ]
}

fn arb_handshake() -> impl Strategy<Value = Message> {
    prop_oneof![
        arb_device_id()
            .prop_map(|device_id| Message::Handshake(HandshakeMessage::Hello { device_id })),
        (arb_device_id(), any::<bool>()).prop_map(|(device_id, trusted)| {
            Message::Handshake(HandshakeMessage::HelloAck { device_id, trusted })
        }),
    ]
}

fn arb_control() -> impl Strategy<Value = Message> {
    prop_oneof![
        any::<u64>().prop_map(|nonce| Message::Control(ControlMessage::Ping { nonce })),
        any::<u64>().prop_map(|nonce| Message::Control(ControlMessage::Pong { nonce })),
        arb_device_id()
            .prop_map(|device_id| Message::Control(ControlMessage::SwitchActive { device_id })),
        ".*".prop_map(|reason| Message::Control(ControlMessage::Disconnect { reason })),
    ]
}

fn arb_input() -> impl Strategy<Value = Message> {
    prop_oneof![
        (any::<u32>(), arb_button_state())
            .prop_map(|(keycode, state)| Message::Input(InputMessage::Key { keycode, state })),
        (any::<i32>(), any::<i32>())
            .prop_map(|(dx, dy)| Message::Input(InputMessage::MouseMove { dx, dy })),
        (arb_mouse_button(), arb_button_state()).prop_map(|(button, state)| {
            Message::Input(InputMessage::MouseButton { button, state })
        }),
        (any::<i32>(), any::<i32>())
            .prop_map(|(dx, dy)| Message::Input(InputMessage::MouseScroll { dx, dy })),
    ]
}

fn arb_clipboard() -> impl Strategy<Value = Message> {
    prop_oneof![
        ".*".prop_map(|text| Message::Clipboard(ClipboardMessage::Update {
            content: ClipboardContent::Text(text)
        })),
        ".*".prop_map(|url| Message::Clipboard(ClipboardMessage::Update {
            content: ClipboardContent::Url(url)
        })),
        (".{0,8}", proptest::collection::vec(any::<u8>(), 0..64)).prop_map(|(format, bytes)| {
            Message::Clipboard(ClipboardMessage::Update {
                content: ClipboardContent::Image { format, bytes },
            })
        }),
    ]
}

fn arb_transfer() -> impl Strategy<Value = Message> {
    prop_oneof![
        (any::<u64>(), ".*", any::<u64>()).prop_map(|(id, file_name, size)| {
            Message::Transfer(TransferMessage::Offer {
                id,
                file_name,
                size,
            })
        }),
        any::<u64>().prop_map(|id| Message::Transfer(TransferMessage::Accept { id })),
        any::<u64>().prop_map(|id| Message::Transfer(TransferMessage::Reject { id })),
        (
            any::<u64>(),
            any::<u64>(),
            proptest::collection::vec(any::<u8>(), 0..256)
        )
            .prop_map(
                |(id, offset, data)| Message::Transfer(TransferMessage::Chunk { id, offset, data })
            ),
        any::<u64>().prop_map(|id| Message::Transfer(TransferMessage::Complete { id })),
        any::<u64>().prop_map(|id| Message::Transfer(TransferMessage::Cancel { id })),
    ]
}

/// Any valid `Message`, across every concern.
fn arb_message() -> impl Strategy<Value = Message> {
    prop_oneof![
        arb_handshake(),
        arb_control(),
        arb_input(),
        arb_clipboard(),
        arb_transfer(),
    ]
}

proptest! {
    /// Every generated message survives encode -> decode unchanged, and
    /// `consumed` always equals the full frame length for a single frame.
    #[test]
    fn arbitrary_message_round_trips(message in arb_message()) {
        let frame = encode_frame(&message).expect("encoding a well-formed Message must not fail");
        match decode_frame(&frame).expect("decoding a just-encoded frame must not fail") {
            DecodeStatus::Ready { message: decoded, consumed } => {
                prop_assert_eq!(decoded, message);
                prop_assert_eq!(consumed, frame.len());
            }
            DecodeStatus::Incomplete => prop_assert!(false, "a fully-buffered frame must not be Incomplete"),
        }
    }

    /// `decode_frame` must never panic on arbitrary bytes — malformed,
    /// truncated, or adversarial input always produces `Incomplete` or
    /// `Err`, both of which are fine; only a panic is a bug.
    #[test]
    fn decode_never_panics_on_arbitrary_bytes(bytes in proptest::collection::vec(any::<u8>(), 0..4096)) {
        let _ = decode_frame(&bytes);
    }

    /// Same guarantee, but biased toward buffers that at least start with a
    /// plausible header, to exercise the payload-decoding path (not just
    /// the "too short to have a header" early return) more often.
    #[test]
    fn decode_never_panics_with_plausible_header(
        payload_len in 0u32..64,
        major in any::<u8>(),
        minor in any::<u8>(),
        payload in proptest::collection::vec(any::<u8>(), 0..128),
    ) {
        let mut buf = Vec::new();
        buf.extend_from_slice(&payload_len.to_le_bytes());
        buf.push(major);
        buf.push(minor);
        buf.extend_from_slice(&payload);
        let _ = decode_frame(&buf);
    }
}
