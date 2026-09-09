//! Version-negotiation behavior: same version, forward-compatible minor
//! bump, and incompatible major bump each produce the documented outcome.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use kvm_protocol::{
    DecodeStatus, HandshakeMessage, Message, PROTOCOL_MAJOR, PROTOCOL_MINOR, ProtocolError,
    decode_frame, encode_frame, is_compatible,
};

fn sample_frame() -> Vec<u8> {
    encode_frame(&Message::Handshake(HandshakeMessage::Hello {
        device_id: [9u8; 32],
    }))
    .expect("encoding a well-formed Message must not fail")
}

#[test]
fn exact_same_version_matches() {
    assert!(is_compatible(PROTOCOL_MAJOR));

    let frame = sample_frame();
    assert!(matches!(
        decode_frame(&frame).expect("same-version frame must decode"),
        DecodeStatus::Ready { .. }
    ));
}

#[test]
fn forward_compatible_minor_bump_is_accepted() {
    // A peer one minor version ahead (or behind) is still compatible — only
    // the major version gates compatibility.
    assert!(is_compatible(PROTOCOL_MAJOR));

    let mut frame = sample_frame();
    let minor_byte_index = 5; // payload_len(4) + major(1) + [minor]
    frame[minor_byte_index] = PROTOCOL_MINOR.wrapping_add(7);

    match decode_frame(&frame).expect("minor-version drift must not be rejected") {
        DecodeStatus::Ready { message, .. } => {
            assert_eq!(
                message,
                Message::Handshake(HandshakeMessage::Hello {
                    device_id: [9u8; 32]
                })
            );
        }
        DecodeStatus::Incomplete => panic!("expected a complete frame"),
    }
}

#[test]
fn incompatible_major_bump_is_cleanly_rejected() {
    assert!(!is_compatible(PROTOCOL_MAJOR + 1));

    let mut frame = sample_frame();
    let major_byte_index = 4; // payload_len(4) + [major]
    let peer_major = PROTOCOL_MAJOR + 1;
    frame[major_byte_index] = peer_major;

    let err = decode_frame(&frame).expect_err("a major-version mismatch must be rejected");
    assert_eq!(
        err,
        ProtocolError::IncompatibleVersion {
            peer_major,
            peer_minor: PROTOCOL_MINOR,
            our_major: PROTOCOL_MAJOR,
            our_minor: PROTOCOL_MINOR,
        }
    );
}
