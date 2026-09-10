//! Manual cross-machine discovery test for Phase 2's DoD ("mDNS-only,
//! UDP-fallback-only, and manual-IP-only paths each manually verified").
//! Not run by `cargo test`; run by hand on two separate machines on the
//! same LAN.
//!
//! Run `advertise` and `browse` on the *same* machine at once and the
//! UDP broadcast path will fail with "address already in use" — both
//! sides bind [`kvm_discovery::DEFAULT_ANNOUNCE_PORT`], which is correct
//! for the real deployment (one process per machine, advertising and
//! browsing concurrently through one shared socket) but means two
//! separate example processes on one box collide on that port. This is
//! not a bug to fix here; it's exactly why this check needs two real
//! machines. (mDNS doesn't have this problem, since `mdns-sd` uses
//! ephemeral ports for its own multicast socket internally.)
//!
//! Uses a fixed device seed so the printed device_id is stable and
//! recognizable across runs — fine for this manual check, never do this
//! for a real device identity.
//!
//! ## Usage
//!
//! On machine 1 (advertises over both mDNS and UDP broadcast):
//! ```text
//! cargo run -p kvm-discovery --example discover -- advertise
//! ```
//!
//! On machine 2 (browses both paths, prints what it finds):
//! ```text
//! cargo run -p kvm-discovery --example discover -- browse
//! ```
//!
//! To specifically exercise only one path, disable the other on the
//! advertising side by passing `--mdns-only` or `--udp-only`. To check
//! the manual-IP path, skip discovery entirely and connect directly
//! with `kvm-net`'s `examples/lan_peer.rs` using the advertised address
//! printed here.
//!
//! Success looks like the browsing machine printing the advertising
//! machine's device_id and address within a few seconds.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::env;
use std::time::Duration;

use kvm_discovery::{MdnsDiscovery, UdpBroadcastDiscovery};
use kvm_identity::DeviceKeypair;

const SEED: [u8; 32] = [0xCC; 32];
const FAKE_QUIC_PORT: u16 = 51820;

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();
    let mdns_only = args.iter().any(|a| a == "--mdns-only");
    let udp_only = args.iter().any(|a| a == "--udp-only");

    match args.get(1).map(String::as_str) {
        Some("advertise") => advertise(mdns_only, udp_only).await,
        Some("browse") => browse(mdns_only, udp_only).await,
        _ => {
            eprintln!(
                "usage: discover advertise [--mdns-only|--udp-only] | discover browse [--mdns-only|--udp-only]"
            );
            std::process::exit(2);
        }
    }
}

async fn advertise(mdns_only: bool, udp_only: bool) {
    let device_id = DeviceKeypair::from_secret_bytes(&SEED).device_id();
    println!("Advertising device_id={}", hex::encode(device_id));

    let mdns = if udp_only {
        None
    } else {
        let d = MdnsDiscovery::new().expect("start mDNS daemon");
        d.advertise(&device_id, FAKE_QUIC_PORT, Some("discover-example"))
            .expect("advertise via mDNS");
        println!("mDNS: advertising as \"discover-example\"");
        Some(d)
    };

    let udp = if mdns_only {
        None
    } else {
        let d = UdpBroadcastDiscovery::bind(kvm_discovery::DEFAULT_ANNOUNCE_PORT)
            .await
            .expect("bind UDP broadcast socket");
        println!(
            "UDP broadcast: announcing on port {}",
            kvm_discovery::DEFAULT_ANNOUNCE_PORT
        );
        Some(d)
    };

    loop {
        if let Some(udp) = &udp {
            let _ = udp
                .announce(&device_id, FAKE_QUIC_PORT, Some("discover-example"))
                .await;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        if mdns.is_none() && udp.is_none() {
            eprintln!("both paths disabled, nothing to do");
            return;
        }
    }
}

async fn browse(mdns_only: bool, udp_only: bool) {
    let mut tasks = Vec::new();

    if !udp_only {
        tasks.push(tokio::spawn(async move {
            let mdns = MdnsDiscovery::new().expect("start mDNS daemon");
            let rx = mdns.browse().expect("browse via mDNS");
            println!("mDNS: browsing...");
            while let Ok(device) = rx.recv_async().await {
                println!(
                    "[mDNS] found device_id={:?} addr={} label={:?}",
                    device.device_id.map(hex::encode),
                    device.addr,
                    device.label
                );
            }
        }));
    }

    if !mdns_only {
        tasks.push(tokio::spawn(async move {
            let udp = UdpBroadcastDiscovery::bind(kvm_discovery::DEFAULT_ANNOUNCE_PORT)
                .await
                .expect("bind UDP broadcast socket");
            println!("UDP broadcast: listening...");
            loop {
                match udp.recv_announcement().await {
                    Ok(device) => println!(
                        "[UDP] found device_id={:?} addr={} label={:?}",
                        device.device_id.map(hex::encode),
                        device.addr,
                        device.label
                    ),
                    Err(e) => {
                        eprintln!("UDP broadcast error: {e}");
                        return;
                    }
                }
            }
        }));
    }

    for task in tasks {
        let _ = task.await;
    }
}
