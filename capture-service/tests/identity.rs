use louiselm_capture::TlsIdentity;

#[test]
fn tls_identity_is_private_persistent_and_fingerprinted() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let identity = TlsIdentity::load_or_create(temporary.path()).expect("identity");

    assert!(identity.certificate_path().is_file());
    assert!(identity.private_key_path().is_file());
    assert_eq!(identity.certificate_sha256().len(), 64);
    assert!(
        identity
            .certificate_sha256()
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    );

    let restarted = TlsIdentity::load_or_create(temporary.path()).expect("restart");
    assert_eq!(
        restarted.certificate_sha256(),
        identity.certificate_sha256()
    );
}
