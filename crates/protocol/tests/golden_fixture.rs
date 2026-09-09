//! Golden-fixture backward-compatibility check.
//!
//! These bytes are the *actual* v1 wire encoding of a `Hello` message,
//! captured once and frozen here. The point of this test isn't what it
//! asserts today — round_trip.rs already covers that — it's what happens
//! when someone changes the framing or message shape later: if that change
//! makes this exact fixture stop decoding (or decode to something
//! different), that's a real backward-compatibility break with already
//! paired peers, and this test is what catches it. Do not "fix" this test
//! by regenerating the fixture unless the break is intentional and the
//! protocol major version is bumped to match.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use kvm_protocol::{DecodeStatus, HandshakeMessage, Message, decode_frame};

/// v1 encoding of `Message::Handshake(HandshakeMessage::Hello { device_id: [0x00..=0x1F] })`.
const V1_HELLO_FRAME: [u8; 40] = [
    0x22, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07,
    0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17,
    0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E, 0x1F,
];

#[test]
fn v1_hello_fixture_still_decodes_as_expected() {
    let expected = Message::Handshake(HandshakeMessage::Hello {
        device_id: [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D,
            0x0E, 0x0F, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1B,
            0x1C, 0x1D, 0x1E, 0x1F,
        ],
    });

    match decode_frame(&V1_HELLO_FRAME).expect("the v1 golden fixture must still decode") {
        DecodeStatus::Ready { message, consumed } => {
            assert_eq!(message, expected);
            assert_eq!(consumed, V1_HELLO_FRAME.len());
        }
        DecodeStatus::Incomplete => panic!("fixture should be a complete frame"),
    }
}
