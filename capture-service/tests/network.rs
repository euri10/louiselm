//! Behavioral coverage for network.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]

use std::net::SocketAddr;

use louiselm_capture::{NetworkProfile, NetworkProfileKind};

#[test]
fn private_network_profiles_are_validated_and_persisted() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let path = temporary.path().join("network.json");
    let profile = NetworkProfile::new(
        NetworkProfileKind::Lan,
        "192.168.1.20:7391".parse::<SocketAddr>().expect("bind"),
        "https://192.168.1.20:7391",
    )
    .expect("LAN profile");

    profile.save(&path).expect("save profile");
    assert_eq!(NetworkProfile::load(&path).expect("load profile"), profile);

    assert!(
        NetworkProfile::new(
            NetworkProfileKind::Lan,
            "0.0.0.0:7391".parse().expect("wildcard"),
            "https://192.168.1.20:7391",
        )
        .is_err()
    );
    assert!(
        NetworkProfile::new(
            NetworkProfileKind::Lan,
            "203.0.113.20:7391".parse().expect("public"),
            "https://203.0.113.20:7391",
        )
        .is_err()
    );
    assert!(
        NetworkProfile::new(
            NetworkProfileKind::Overlay,
            "100.100.20.30:7391".parse().expect("overlay"),
            "https://203.0.113.20:7391",
        )
        .is_err()
    );
    assert!(
        NetworkProfile::new(
            NetworkProfileKind::Overlay,
            "100.100.20.30:7391".parse().expect("overlay"),
            "https://desktop.example.ts.net:7391",
        )
        .is_ok()
    );
}

#[test]
fn missing_profile_is_an_explicit_loopback_only_state() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let profile = NetworkProfile::load_or_default(&temporary.path().join("missing.json"))
        .expect("default profile");

    assert_eq!(profile.kind(), None);
    assert_eq!(profile.bind(), "127.0.0.1:7391".parse().expect("bind"));
    assert_eq!(profile.receiver_url(), None);
    assert!(!profile.phone_reachable());
}
