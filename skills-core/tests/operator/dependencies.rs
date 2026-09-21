use super::*;
use louiselm_skills::{
    Digest,
    broker::{DependencyInspection, operator::dependencies},
};

#[test]
fn dependency_operator_requires_exact_durable_batch_acknowledgement() {
    for confirm in [false, true] {
        let root = private_root();
        let path = root.path().join("operator.sock");
        let uid = rustix::process::geteuid().as_raw();
        let server = OperatorServer::bind(&path, uid).unwrap();
        let ids = vec![Digest::of(b"candidate").to_string()];
        let expected = ids.clone();
        let worker = thread::spawn(move || {
            server
                .serve_once(
                    |session_id, approve| {
                        assert_eq!(session_id, "session");
                        assert_eq!(approve, Some(expected.as_slice()));
                        Ok(DependencyInspection {
                            session_id: session_id.into(),
                            envelope_revision: 1,
                            pending: vec![],
                            has_more: false,
                            approved: if confirm { expected } else { vec![] },
                            expires_at_ms: 1000,
                        })
                    },
                    |_, _| panic!("not Session lookup"),
                    |_| panic!("not conformance lookup"),
                    |_, _| panic!("not Skill control"),
                    |_, _| panic!("not Beads control"),
                    |_, _| panic!("not retention control"),
                    |_, _, _| panic!("not waiver control"),
                )
                .unwrap();
        });
        assert_eq!(
            dependencies(&path, uid, "session", Some(ids), Duration::from_secs(2)).is_ok(),
            confirm
        );
        worker.join().unwrap();
    }
}

#[test]
fn dependency_operator_rejects_empty_duplicate_and_unbounded_batches_before_connecting() {
    let id = Digest::of(b"candidate").to_string();
    for batch in [
        vec![],
        vec![id.clone(), id.clone()],
        vec![id; 33],
        vec!["*".into()],
    ] {
        assert!(matches!(
            dependencies(
                std::path::Path::new("/does-not-exist"),
                1,
                "session",
                Some(batch),
                Duration::from_secs(1)
            ),
            Err(InspectError::InvalidRequest)
        ));
    }
}
