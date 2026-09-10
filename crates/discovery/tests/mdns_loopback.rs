//! Automated same-machine check that mDNS advertise/browse actually
//! finds a real registered service through the real `mdns-sd` daemon and
//! OS multicast stack — not a substitute for the Phase 2 DoD's manual
//! verification across real separate devices on a real LAN (multicast
//! behaves differently across real routers/Wi-Fi/VLANs than on one
//! machine's loopback/local interfaces), but real evidence the
//! advertise/browse wiring itself is correct.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use kvm_discovery::MdnsDiscovery;

#[tokio::test]
async fn browse_finds_an_advertised_device_on_this_machine() {
    let device_id = [77u8; 32];

    let advertiser = MdnsDiscovery::new().unwrap();
    advertiser
        .advertise(&device_id, 51820, Some("mdns-loopback-test-device"))
        .unwrap();

    let browser = MdnsDiscovery::new().unwrap();
    let rx = browser.browse().unwrap();

    let found = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let device = rx.recv_async().await.expect("browse channel closed");
            if device.device_id == Some(device_id) {
                return device;
            }
            // Any other service resolving first (e.g. a leftover from
            // another test process) is fine to skip past.
        }
    })
    .await
    .expect("did not discover the advertised device within 10s");

    assert_eq!(found.device_id, Some(device_id));
    assert_eq!(found.addr.port(), 51820);

    advertiser.shutdown().unwrap();
    browser.shutdown().unwrap();
}
