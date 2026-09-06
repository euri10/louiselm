//! Behavioral coverage for identity.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]

use louiselm_capture::TlsIdentity;
use rcgen::{CertificateParams, KeyPair};

#[test]
fn tls_identity_is_private_persistent_and_fingerprinted() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let identity = TlsIdentity::load_or_create(temporary.path()).expect("identity");

    assert!(identity.certificate_path().is_file());
    assert!(identity.private_key_path().is_file());
    assert_eq!(identity.public_key_sha256().len(), 64);
    assert!(
        identity
            .public_key_sha256()
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    );

    let restarted = TlsIdentity::load_or_create(temporary.path()).expect("restart");
    assert_eq!(restarted.public_key_sha256(), identity.public_key_sha256());

    let original_certificate =
        std::fs::read_to_string(identity.certificate_path()).expect("original certificate");
    let private_key = std::fs::read_to_string(identity.private_key_path()).expect("private key");
    let key_pair = KeyPair::from_pem(&private_key).expect("key pair");
    let renewed = CertificateParams::new(vec!["renewed.example".to_owned()])
        .expect("certificate parameters")
        .self_signed(&key_pair)
        .expect("renewed certificate");
    std::fs::write(identity.certificate_path(), renewed.pem()).expect("replace certificate");
    assert_ne!(
        std::fs::read_to_string(identity.certificate_path()).expect("renewed PEM"),
        original_certificate
    );

    let renewed_identity = TlsIdentity::load_or_create(temporary.path()).expect("renewed identity");
    assert_eq!(
        renewed_identity.public_key_sha256(),
        identity.public_key_sha256()
    );
}
