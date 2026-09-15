//! Existing admitted chains retain their identity when current authority changes.

use super::*;

#[test]
fn admitted_chain_survives_rotation_and_release_upgrade() {
    for upgrade in [false, true] {
        let root = TempDir::new().unwrap();
        let authorization = consumed_authorization(root.path(), &request("original"));
        let path = root.path().join("receipts");
        let original = trusted_release();
        let receipts = ReceiptStore::open(&path, original.clone()).unwrap();
        let launch = launch_receipt(&authorization);
        receipts
            .append(
                &authorization,
                &launch.canonical_bytes(),
                verify_fixture_signature,
            )
            .unwrap();
        drop(receipts);

        let current = TrustedRelease {
            release_id: if upgrade {
                Digest::of(b"next release").to_string()
            } else {
                original.release_id
            },
            signing_key_id: Digest::of(b"next key").to_string(),
        };
        let receipts = ReceiptStore::open(&path, current).unwrap();
        let start = start_receipt(&authorization, &launch);
        receipts
            .append(
                &authorization,
                &start.canonical_bytes(),
                verify_fixture_signature,
            )
            .expect("admitted history continues under its original authority");
        assert_eq!(
            receipts.stored_bytes(&authorization.session_id).unwrap(),
            [launch.canonical_bytes(), start.canonical_bytes()]
        );

        let fresh = consumed_authorization(root.path(), &request("new-under-retired-key"));
        assert!(
            receipts
                .append(
                    &fresh,
                    &launch_receipt(&fresh).canonical_bytes(),
                    verify_fixture_signature
                )
                .is_err(),
            "retained history does not authorize a new old-key chain"
        );
    }
}
