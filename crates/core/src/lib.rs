//! Orchestrator: device state, active-device switching, screen layout, and
//! pairing state machines. Wires `input`, `clipboard`, `transfer`, and `net`
//! together via channels; owns no sockets or OS input APIs directly.
