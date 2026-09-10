//! Wire format for UDP broadcast discovery announcements — the fallback
//! path for networks that block mDNS. Deliberately its own tiny format
//! rather than reusing `kvm_protocol::Message`/framing: this is a single
//! connectionless datagram, not a stream, and has nothing to do with the
//! authenticated peer protocol.
//!
//! Layout: `[4-byte magic "KVM1"][32-byte device_id][2-byte port, BE]
//! [1-byte label_len][label_len bytes, UTF-8]`.

const MAGIC: [u8; 4] = *b"KVM1";
const MAX_LABEL_LEN: usize = 63;
const HEADER_LEN: usize = 4 + 32 + 2 + 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Announcement {
    pub device_id: [u8; 32],
    pub port: u16,
    pub label: Option<String>,
}

/// Encodes an announcement. `label` is truncated at a UTF-8 char boundary
/// to [`MAX_LABEL_LEN`] bytes if longer — this is a best-effort broadcast
/// hint, not a place to fail loudly over a long device name.
pub fn encode(device_id: &[u8; 32], port: u16, label: Option<&str>) -> Vec<u8> {
    let label = label.unwrap_or("");
    let truncated = truncate_to_boundary(label, MAX_LABEL_LEN);

    let mut buf = Vec::with_capacity(HEADER_LEN + truncated.len());
    buf.extend_from_slice(&MAGIC);
    buf.extend_from_slice(device_id);
    buf.extend_from_slice(&port.to_be_bytes());
    buf.push(truncated.len() as u8);
    buf.extend_from_slice(truncated.as_bytes());
    buf
}

fn truncate_to_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Decodes an announcement. Returns `None` for anything malformed —
/// wrong magic, truncated buffer, a declared label length that overruns
/// the buffer, or non-UTF-8 label bytes — never panics, since this reads
/// datagrams from an untrusted, unauthenticated broadcast channel.
pub fn decode(bytes: &[u8]) -> Option<Announcement> {
    if bytes.len() < HEADER_LEN {
        return None;
    }
    if bytes[0..4] != MAGIC {
        return None;
    }

    let mut device_id = [0u8; 32];
    device_id.copy_from_slice(&bytes[4..36]);

    let mut port_bytes = [0u8; 2];
    port_bytes.copy_from_slice(&bytes[36..38]);
    let port = u16::from_be_bytes(port_bytes);

    let label_len = bytes[38] as usize;
    let label_start = HEADER_LEN;
    let label_end = label_start.checked_add(label_len)?;
    if bytes.len() < label_end {
        return None;
    }

    let label = if label_len == 0 {
        None
    } else {
        Some(
            std::str::from_utf8(&bytes[label_start..label_end])
                .ok()?
                .to_string(),
        )
    };

    Some(Announcement {
        device_id,
        port,
        label,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_with_and_without_a_label() {
        let device_id = [7u8; 32];
        let encoded = encode(&device_id, 51820, Some("Gourav's MacBook"));
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded.device_id, device_id);
        assert_eq!(decoded.port, 51820);
        assert_eq!(decoded.label.as_deref(), Some("Gourav's MacBook"));

        let encoded_no_label = encode(&device_id, 51820, None);
        let decoded_no_label = decode(&encoded_no_label).unwrap();
        assert_eq!(decoded_no_label.label, None);
    }

    #[test]
    fn empty_buffer_decodes_to_none() {
        assert_eq!(decode(&[]), None);
    }

    #[test]
    fn truncated_header_decodes_to_none() {
        let encoded = encode(&[1u8; 32], 1234, Some("x"));
        for len in 0..HEADER_LEN {
            assert_eq!(decode(&encoded[..len]), None, "len={len}");
        }
    }

    #[test]
    fn wrong_magic_decodes_to_none() {
        let mut encoded = encode(&[1u8; 32], 1234, None);
        encoded[0] = b'X';
        assert_eq!(decode(&encoded), None);
    }

    #[test]
    fn declared_label_length_overrunning_the_buffer_decodes_to_none() {
        let mut encoded = encode(&[1u8; 32], 1234, Some("hi"));
        // Claim a much longer label than is actually present.
        let label_len_index = 4 + 32 + 2;
        encoded[label_len_index] = 200;
        assert_eq!(decode(&encoded), None);
    }

    #[test]
    fn non_utf8_label_bytes_decode_to_none() {
        let mut encoded = encode(&[1u8; 32], 1234, Some("hi"));
        let label_start = HEADER_LEN;
        encoded[label_start] = 0xFF; // invalid UTF-8 start byte
        encoded[label_start + 1] = 0xFF;
        assert_eq!(decode(&encoded), None);
    }

    #[test]
    fn overlong_label_is_truncated_not_rejected() {
        let long_label = "x".repeat(200);
        let encoded = encode(&[1u8; 32], 1234, Some(&long_label));
        let decoded = decode(&encoded).unwrap();
        assert!(decoded.label.unwrap().len() <= MAX_LABEL_LEN);
    }

    #[test]
    fn truncation_respects_utf8_char_boundaries() {
        // Each "é" is 2 bytes; a naive byte-count truncation at an odd
        // boundary would split a character and produce invalid UTF-8.
        let label: String = "é".repeat(40); // 80 bytes
        let encoded = encode(&[1u8; 32], 1234, Some(&label));
        // Must not panic (invalid UTF-8 would panic in `&str[..end]`
        // slicing if the boundary were wrong), and must decode cleanly.
        let decoded = decode(&encoded).unwrap();
        assert!(decoded.label.is_some());
    }

    #[test]
    fn decode_never_panics_on_arbitrary_short_buffers() {
        // Not a full proptest crate dependency for one small format —
        // a deterministic sweep over small buffers covers the same
        // "never panics" property this format's tiny state space allows.
        for len in 0..80 {
            let buf = vec![0xABu8; len];
            let _ = decode(&buf);
        }
    }
}
