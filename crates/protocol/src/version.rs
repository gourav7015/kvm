//! Protocol version and compatibility rules.

/// Major component of the current protocol version.
///
/// Peers with a different major version are incompatible and must reject
/// the connection during handshake — a major bump means existing wire
/// semantics changed.
pub const PROTOCOL_MAJOR: u8 = 1;

/// Minor component of the current protocol version.
///
/// Peers with the same major version but a different minor version are
/// forward/backward compatible: newer minor versions only add optional
/// behavior, never change or remove existing wire semantics.
pub const PROTOCOL_MINOR: u8 = 0;

/// Checks whether a peer-reported major version is compatible with ours.
///
/// Compatibility is major-version equality; minor version differences are
/// always accepted, in either direction.
pub fn is_compatible(peer_major: u8) -> bool {
    peer_major == PROTOCOL_MAJOR
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn same_version_is_compatible() {
        assert!(is_compatible(PROTOCOL_MAJOR));
    }

    #[test]
    fn different_major_is_incompatible() {
        assert!(!is_compatible(PROTOCOL_MAJOR + 1));
        assert!(!is_compatible(PROTOCOL_MAJOR.wrapping_sub(1)));
    }
}
