use louiselm_capture::{PairingRegistry, PairingStatus};

#[test]
fn one_time_token_creates_a_persistent_hashed_device_credential() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let registry = PairingRegistry::open(temporary.path()).expect("registry");
    let offer = registry
        .issue(
            "https://192.0.2.1:7391",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            1_000,
            60_000,
        )
        .expect("offer");

    let paired = registry
        .consume(&offer.token, "garden-phone", 2_000)
        .expect("consume");
    assert_eq!(paired.device_name, "garden-phone");
    assert!(registry.authenticate(&paired.credential).expect("auth"));
    assert!(registry.consume(&offer.token, "second", 2_001).is_err());
    assert_eq!(offer.version, 2);
    assert_eq!(
        offer.receiver_identity_sha256,
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    );

    let state =
        std::fs::read_to_string(temporary.path().join("pairing.json")).expect("persisted state");
    assert!(!state.contains(&offer.token));
    assert!(!state.contains(&paired.credential));

    let restarted = PairingRegistry::open(temporary.path()).expect("restart");
    assert!(
        restarted
            .authenticate(&paired.credential)
            .expect("restart auth")
    );
    restarted.revoke(&paired.device_id).expect("revoke");
    assert!(!restarted.authenticate(&paired.credential).expect("revoked"));
}

#[test]
fn expired_pairing_offer_is_rejected_and_device_listing_exposes_no_secret() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let registry = PairingRegistry::open(temporary.path()).expect("registry");
    let offer = registry
        .issue(
            "https://192.0.2.1:7391",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            1_000,
            5_000,
        )
        .expect("offer");

    assert!(registry.consume(&offer.token, "late-phone", 6_001).is_err());
    assert_eq!(
        registry.status().expect("status"),
        PairingStatus { devices: vec![] }
    );
}

#[test]
fn pairing_offer_requires_a_clean_https_base_url_and_full_receiver_identity() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let registry = PairingRegistry::open(temporary.path()).expect("registry");
    let fingerprint = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    for invalid in [
        "https://",
        "http://192.0.2.1:7391",
        "https://user@192.0.2.1:7391",
        "https://192.0.2.1:7391/path",
    ] {
        assert!(registry.issue(invalid, fingerprint, 1_000, 5_000).is_err());
    }
    assert!(
        registry
            .issue("https://192.0.2.1:7391", "aabbcc", 1_000, 5_000)
            .is_err()
    );
}

#[test]
fn separate_cli_and_server_registries_observe_pairing_and_revocation() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let server = PairingRegistry::open(temporary.path()).expect("server registry");
    let cli = PairingRegistry::open(temporary.path()).expect("CLI registry");
    let offer = cli
        .issue(
            "https://192.0.2.1:7391",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            1_000,
            5_000,
        )
        .expect("offer");

    let paired = server
        .consume(&offer.token, "garden phone", 2_000)
        .expect("server observes offer");
    assert!(
        cli.authenticate(&paired.credential)
            .expect("CLI observes device")
    );
    cli.revoke(&paired.device_id).expect("CLI revokes device");
    assert!(
        !server
            .authenticate(&paired.credential)
            .expect("server observes revocation")
    );
}
