//! Platform-independent device/layout model for edge-based switching —
//! see ADR-0009. Pure data: no I/O, no OS coordinates, no networking.
//! [`Layout::neighbor`] is the only query the rest of `core` needs.

use std::collections::HashMap;

use kvm_protocol::DeviceId;
use thiserror::Error;

/// Which screen edge a pointer crossed. Deliberately just four values —
/// a corner is not a separate case (see ADR-0009): resolving which of
/// the two adjoining edges a corner-crossing pointer actually meant is
/// the caller's job (based on its motion vector), not this model's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

/// One device's entry in a [`Layout`] — display metadata only; routing
/// itself is keyed entirely by `device_id`, never by IP address or
/// label (see ADR-0009: IP addresses can change, `DeviceId` doesn't).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutDevice {
    pub device_id: DeviceId,
    pub label: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LayoutError {
    #[error("device {0:?} is not part of this layout")]
    UnknownDevice(DeviceId),
    #[error("device {0:?} already has an edge assignment for {1:?}")]
    DuplicateEdge(DeviceId, Edge),
    #[error("a device cannot be its own neighbor ({0:?})")]
    SelfNeighbor(DeviceId),
}

/// A platform-independent map of which device sits across which edge of
/// which other device. No OS coordinates, no IP addresses — routing is
/// keyed entirely by [`DeviceId`].
#[derive(Debug, Clone, Default)]
pub struct Layout {
    devices: HashMap<DeviceId, LayoutDevice>,
    edges: HashMap<(DeviceId, Edge), DeviceId>,
}

impl Layout {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds (or replaces) a device's entry. Replacing an existing
    /// device's entry does not touch any edge assignments already made
    /// for it.
    pub fn add_device(&mut self, device: LayoutDevice) {
        self.devices.insert(device.device_id, device);
    }

    pub fn device(&self, device_id: &DeviceId) -> Option<&LayoutDevice> {
        self.devices.get(device_id)
    }

    pub fn devices(&self) -> impl Iterator<Item = &LayoutDevice> {
        self.devices.values()
    }

    /// Assigns `to` as `from`'s neighbor across `edge`. Both devices
    /// must already have been added via [`Self::add_device`]. At most
    /// one neighbor per `(from, edge)` pair — calling this twice for
    /// the same pair is a [`LayoutError::DuplicateEdge`], not a silent
    /// overwrite, so a config mistake (two devices both claiming a
    /// hub's right edge) surfaces immediately instead of quietly
    /// keeping whichever assignment happened to be added first.
    pub fn set_neighbor(
        &mut self,
        from: DeviceId,
        edge: Edge,
        to: DeviceId,
    ) -> Result<(), LayoutError> {
        if from == to {
            return Err(LayoutError::SelfNeighbor(from));
        }
        if !self.devices.contains_key(&from) {
            return Err(LayoutError::UnknownDevice(from));
        }
        if !self.devices.contains_key(&to) {
            return Err(LayoutError::UnknownDevice(to));
        }
        if self.edges.contains_key(&(from, edge)) {
            return Err(LayoutError::DuplicateEdge(from, edge));
        }
        self.edges.insert((from, edge), to);
        Ok(())
    }

    /// The device configured across `edge` from `from`, if any — the
    /// only query the rest of `core` needs. `None` covers both an
    /// unmapped edge (no neighbor configured — completely ordinary,
    /// most devices don't have all four edges wired up) and a
    /// currently-disabled neighbor; either way it means "nothing to
    /// switch to," not an error.
    pub fn neighbor(&self, from: DeviceId, edge: Edge) -> Option<DeviceId> {
        let to = *self.edges.get(&(from, edge))?;
        let device = self.devices.get(&to)?;
        device.enabled.then_some(to)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const MAC: DeviceId = [1u8; 32];
    const WINDOWS: DeviceId = [2u8; 32];
    const LINUX: DeviceId = [3u8; 32];
    const LAPTOP: DeviceId = [4u8; 32];
    const UNKNOWN: DeviceId = [9u8; 32];

    fn device(id: DeviceId, label: &str) -> LayoutDevice {
        LayoutDevice {
            device_id: id,
            label: label.to_string(),
            enabled: true,
        }
    }

    #[test]
    fn exact_edge_lookup() {
        let mut layout = Layout::new();
        layout.add_device(device(MAC, "Mac"));
        layout.add_device(device(WINDOWS, "Windows"));
        layout.set_neighbor(MAC, Edge::Right, WINDOWS).unwrap();

        assert_eq!(layout.neighbor(MAC, Edge::Right), Some(WINDOWS));
        assert_eq!(layout.neighbor(MAC, Edge::Left), None);
        assert_eq!(layout.neighbor(MAC, Edge::Top), None);
        assert_eq!(layout.neighbor(MAC, Edge::Bottom), None);
    }

    #[test]
    fn a_hub_device_can_have_up_to_four_distinct_neighbors() {
        let mut layout = Layout::new();
        for d in [MAC, WINDOWS, LINUX, LAPTOP] {
            layout.add_device(device(d, "device"));
        }
        layout.set_neighbor(MAC, Edge::Right, WINDOWS).unwrap();
        layout.set_neighbor(MAC, Edge::Top, LINUX).unwrap();
        layout.set_neighbor(MAC, Edge::Bottom, LAPTOP).unwrap();

        assert_eq!(layout.neighbor(MAC, Edge::Right), Some(WINDOWS));
        assert_eq!(layout.neighbor(MAC, Edge::Top), Some(LINUX));
        assert_eq!(layout.neighbor(MAC, Edge::Bottom), Some(LAPTOP));
        assert_eq!(layout.neighbor(MAC, Edge::Left), None);
    }

    #[test]
    fn unmapped_edge_is_none_not_an_error() {
        let mut layout = Layout::new();
        layout.add_device(device(MAC, "Mac"));
        assert_eq!(layout.neighbor(MAC, Edge::Left), None);
    }

    #[test]
    fn unknown_source_device_has_no_neighbors() {
        let layout = Layout::new();
        assert_eq!(layout.neighbor(UNKNOWN, Edge::Right), None);
    }

    #[test]
    fn disabled_neighbor_is_not_returned() {
        let mut layout = Layout::new();
        layout.add_device(device(MAC, "Mac"));
        layout.add_device(LayoutDevice {
            device_id: WINDOWS,
            label: "Windows".to_string(),
            enabled: false,
        });
        layout.set_neighbor(MAC, Edge::Right, WINDOWS).unwrap();

        assert_eq!(layout.neighbor(MAC, Edge::Right), None);
    }

    #[test]
    fn setting_a_neighbor_for_an_unknown_device_is_rejected() {
        let mut layout = Layout::new();
        layout.add_device(device(MAC, "Mac"));
        assert_eq!(
            layout.set_neighbor(MAC, Edge::Right, UNKNOWN),
            Err(LayoutError::UnknownDevice(UNKNOWN))
        );
        assert_eq!(
            layout.set_neighbor(UNKNOWN, Edge::Right, MAC),
            Err(LayoutError::UnknownDevice(UNKNOWN))
        );
    }

    #[test]
    fn duplicate_edge_assignment_is_rejected_not_silently_overwritten() {
        let mut layout = Layout::new();
        layout.add_device(device(MAC, "Mac"));
        layout.add_device(device(WINDOWS, "Windows"));
        layout.add_device(device(LINUX, "Linux"));
        layout.set_neighbor(MAC, Edge::Right, WINDOWS).unwrap();

        assert_eq!(
            layout.set_neighbor(MAC, Edge::Right, LINUX),
            Err(LayoutError::DuplicateEdge(MAC, Edge::Right))
        );
        // The original assignment must be untouched by the rejected call.
        assert_eq!(layout.neighbor(MAC, Edge::Right), Some(WINDOWS));
    }

    #[test]
    fn a_device_cannot_be_its_own_neighbor() {
        let mut layout = Layout::new();
        layout.add_device(device(MAC, "Mac"));
        assert_eq!(
            layout.set_neighbor(MAC, Edge::Right, MAC),
            Err(LayoutError::SelfNeighbor(MAC))
        );
    }

    #[test]
    fn edges_are_directional_not_automatically_reciprocal() {
        // Configuring Mac -> Windows on the right does not imply
        // Windows -> Mac on the left; that's a separate, deliberate
        // call, matching real layouts where a device might only ever
        // switch outward (or the reverse edge might point elsewhere).
        let mut layout = Layout::new();
        layout.add_device(device(MAC, "Mac"));
        layout.add_device(device(WINDOWS, "Windows"));
        layout.set_neighbor(MAC, Edge::Right, WINDOWS).unwrap();

        assert_eq!(layout.neighbor(WINDOWS, Edge::Left), None);
    }
}
