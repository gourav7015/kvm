# ADR-0006: mDNS Host Names Must Respect the 63-Byte DNS Label Limit

## Status

Accepted (2026-09-10)

## Context

While writing `discovery`'s loopback mDNS test (advertise a device, then
browse and expect to find it), browsing consistently timed out: the
`ServiceFound` event fired (so the PTR/browse record was visible), but it
never progressed to `ServiceResolved` (SRV/TXT/A record resolution never
completed).

Root-caused by checking macOS's own `dns-sd` command-line tool
independently of this crate's code: `dns-sd -B` (bare browse) found the
service fine, but `dns-sd -L` (full lookup/resolve) also silently failed
to resolve it — proving the bug was in what we were *publishing*, not in
our own browse-side code.

The cause: `MdnsDiscovery::advertise` built the mDNS host name from the
device's full 64-character hex-encoded `DeviceId` (`"<64 hex
chars>.local."`). DNS labels are capped at 63 bytes (RFC 1035) — a
64-byte label is invalid. `mdns-sd`/the underlying resolver didn't error
loudly on this; it just meant the address (A) record never
published/resolved correctly, so nothing depending on it ever completed.

## Decision

Use a truncated (16-hex-char / 8-byte) prefix of the device_id as the
mDNS host name instead of the full identifier. This is well within the
63-byte limit and still effectively unique for grouping one device's
address records — it is not the device's actual identity, which
continues to travel intact and separately in the TXT record
(`device_id`, still the full 64-char hex value) that `browse` actually
reads to build a `DiscoveredDevice`.

## Consequences

- `mdns::host_name_for` is a small pure function specifically so this
  constraint has a direct, fast unit test
  (`host_name_labels_never_exceed_the_dns_limit`) independent of the
  slower real-network loopback test that originally caught the bug —
  matching the project's standing rule that a bug's regression test
  belongs at the lowest crate level that can express it.
- General lesson, same shape as ADR-0005: a real end-to-end check (not
  just a compiling unit test) caught something a purely logical review
  would very plausibly have missed — nothing in `ServiceInfo::new`'s
  compile-time types stops you from handing it an oversized label, and
  the failure mode is silent (a timeout, not an error) rather than loud.
- The same 63-byte-label constraint would bite any other DNS label this
  crate constructs from arbitrary-length input in the future (e.g. if a
  device's human-chosen label were ever used to build a label rather
  than just a TXT/instance-name value) — worth remembering as a category
  of mistake, not just this one instance of it.
