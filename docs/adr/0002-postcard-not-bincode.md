# ADR-0002: Use `postcard` Instead of `bincode` for Wire Serialization

## Status

Accepted (2026-09-09) — supersedes the serialization choice in [ADR-0001](0001-tech-stack.md)

## Context

ADR-0001 chose `bincode` for `protocol` crate serialization. While adding it as
a dependency during Phase 1a implementation, `cargo add`/docs.rs surfaced that
`bincode` 3.0.0 is marked unmaintained by its author, and RustSec has since
published **RUSTSEC-2025-0141** ("Bincode is unmaintained", 2026-01-07). Our
own Phase 0 CI gate (`cargo deny check` advisories) would flag this the moment
it landed in `Cargo.lock`, so this is exactly the kind of thing that process
was built to catch early rather than after later phases depend on it.

## Decision

Use [`postcard`](https://docs.rs/postcard) instead. It is:

- Actively maintained (regular releases, stable 1.0+ wire format).
- A near drop-in replacement: same serde-derive workflow
  (`#[derive(Serialize, Deserialize)]` on message types), just
  `postcard::to_allocvec(&msg)` / `postcard::from_bytes::<Msg>(&bytes)`
  instead of `bincode::serde::encode_to_vec`/`decode_from_slice`.
- A compact varint-based binary format, comparable in size/performance to
  bincode for our purposes (small, frequent messages: input events, control,
  clipboard, transfer chunks).
- Widely used in production (embedded/IoT ecosystem in particular), so it is
  battle-tested for exactly the "compact wire format for a custom protocol"
  use case `protocol` exists for.

## Consequences

- `crates/protocol` depends on `postcard` (with the `alloc` feature) instead
  of `bincode`.
- No change to the framing/versioning design in `protocol` — only the
  encode/decode calls underneath change.
- General lesson for the rest of the build: dependency choices get a final
  sanity check (`cargo add` + `cargo deny check`) at the point they're
  actually introduced, not just trusted from the original ADR, since a
  crate's maintenance status can change between planning and implementation.
