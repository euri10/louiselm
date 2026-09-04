use std::sync::{Arc, Mutex};

use louiselm_skills::{
    canonical::Digest,
    launch_receipt::{
        Authorization, ChainAnchor, Completion, LaunchEvidence, RECEIPT_SCHEMA, ReceiptAppender,
        ReceiptAuthority, ReceiptCause, ReceiptError, ReceiptOutcome, ReceiptPayload,
        ReceiptSigner, SIGNED_RECEIPT_SCHEMA, SessionState, SignedReceipt,
    },
};

fn digest(label: &str) -> String {
    Digest::of(label.as_bytes()).to_string()
}

fn authorization(request_id: &str) -> Authorization {
    Authorization {
        authorization_id: "authorization-1".to_owned(),
        request_id: request_id.to_owned(),
        request_digest: digest(request_id),
    }
}

fn launch_authorization(request_id: &str) -> Authorization {
    Authorization {
        request_digest: digest("launch-request"),
        ..authorization(request_id)
    }
}

fn launch_evidence() -> LaunchEvidence {
    LaunchEvidence {
        launch_request_digest: digest("launch-request"),
        runtime_measurement_digest: digest("runtime"),
        skill_generation_id: digest("generation"),
        session_input_manifest_id: digest("input"),
        isolation_contract: "louiselm.isolation/1".to_owned(),
        isolation_backend_id: "bubblewrap-0_12".to_owned(),
        kernel_identity: "linux-6_18".to_owned(),
        isolation_evidence_digest: digest("isolation"),
        capability_channel_ids: vec!["acp".to_owned(), "broker".to_owned()],
    }
}

fn payload(
    sequence: u64,
    previous_receipt_digest: Option<String>,
    envelope_revision: u64,
    request_id: &str,
    outcome: ReceiptOutcome,
    resulting_state: SessionState,
) -> ReceiptPayload {
    ReceiptPayload {
        schema: RECEIPT_SCHEMA.to_owned(),
        session_id: "session-1".to_owned(),
        run_id: "run-1".to_owned(),
        request_id: request_id.to_owned(),
        envelope_revision,
        sequence,
        previous_receipt_digest,
        release_id: digest("release"),
        signing_key_id: digest("launcher-key"),
        outcome,
        resulting_state,
    }
}

fn signed(payload: ReceiptPayload) -> SignedReceipt {
    let signature = Digest::of(&payload.canonical_bytes()).to_string();
    SignedReceipt {
        schema: SIGNED_RECEIPT_SCHEMA.to_owned(),
        payload,
        signature,
    }
}

fn chain() -> Vec<SignedReceipt> {
    let launch = signed(payload(
        0,
        None,
        3,
        "request-0",
        ReceiptOutcome::Launch {
            authorization: launch_authorization("request-0"),
            evidence: Box::new(launch_evidence()),
        },
        SessionState::Starting,
    ));
    let start = signed(payload(
        1,
        Some(launch.digest().to_string()),
        3,
        "request-start",
        ReceiptOutcome::Start {
            authority: ReceiptAuthority::Cause {
                cause: ReceiptCause::LaunchAcknowledged,
            },
        },
        SessionState::Running,
    ));
    let park = signed(payload(
        2,
        Some(start.digest().to_string()),
        3,
        "request-1",
        ReceiptOutcome::Park {
            authority: ReceiptAuthority::Cause {
                cause: ReceiptCause::BrokerLost,
            },
        },
        SessionState::Parked,
    ));
    let resume = signed(payload(
        3,
        Some(park.digest().to_string()),
        4,
        "request-2",
        ReceiptOutcome::Resume {
            authorization: authorization("request-2"),
        },
        SessionState::Running,
    ));
    let interrupt = signed(payload(
        4,
        Some(resume.digest().to_string()),
        4,
        "request-3",
        ReceiptOutcome::Interrupt {
            authorization: authorization("request-3"),
        },
        SessionState::Running,
    ));
    let dispose = signed(payload(
        5,
        Some(interrupt.digest().to_string()),
        4,
        "request-4",
        ReceiptOutcome::Disposal {
            authority: ReceiptAuthority::Cause {
                cause: ReceiptCause::ProcessExited,
            },
        },
        SessionState::Terminal,
    ));
    vec![launch, start, park, resume, interrupt, dispose]
}

fn anchor() -> ChainAnchor {
    ChainAnchor {
        session_id: "session-1".to_owned(),
        run_id: "run-1".to_owned(),
        release_id: digest("release"),
        signing_key_id: digest("launcher-key"),
    }
}

fn verifies(key_id: &str, bytes: &[u8], signature: &str) -> bool {
    key_id == digest("launcher-key") && Digest::of(bytes).to_string() == signature
}

#[test]
fn payload_and_signed_envelope_have_one_canonical_encoding() {
    let receipt = chain().remove(0);
    let payload_bytes = receipt.payload.canonical_bytes();
    let envelope_bytes = receipt.canonical_bytes();

    assert_eq!(
        String::from_utf8(payload_bytes.clone()).unwrap(),
        format!(
            concat!(
                "{{\"schema\":\"louiselm.launch.receipt/2\",",
                "\"session_id\":\"session-1\",\"run_id\":\"run-1\",",
                "\"request_id\":\"request-0\",\"envelope_revision\":3,",
                "\"sequence\":0,\"previous_receipt_digest\":null,",
                "\"release_id\":\"{}\",\"signing_key_id\":\"{}\",",
                "\"outcome\":{{\"action\":\"launch\",\"authorization\":{{",
                "\"authorization_id\":\"authorization-1\",\"request_id\":\"request-0\",",
                "\"request_digest\":\"{}\"}},\"evidence\":{{",
                "\"launch_request_digest\":\"{}\",",
                "\"runtime_measurement_digest\":\"{}\",",
                "\"skill_generation_id\":\"{}\",",
                "\"session_input_manifest_id\":\"{}\",",
                "\"isolation_contract\":\"louiselm.isolation/1\",",
                "\"isolation_backend_id\":\"bubblewrap-0_12\",",
                "\"kernel_identity\":\"linux-6_18\",",
                "\"isolation_evidence_digest\":\"{}\",",
                "\"capability_channel_ids\":[\"acp\",\"broker\"]}}}},",
                "\"resulting_state\":\"starting\"}}"
            ),
            digest("release"),
            digest("launcher-key"),
            digest("launch-request"),
            digest("launch-request"),
            digest("runtime"),
            digest("generation"),
            digest("input"),
            digest("isolation"),
        ),
    );
    assert_eq!(
        String::from_utf8(envelope_bytes.clone()).unwrap(),
        format!(
            "{{\"schema\":\"louiselm.launch.signed-receipt/2\",\"payload\":{},\"signature\":\"{}\"}}",
            String::from_utf8(payload_bytes.clone()).unwrap(),
            receipt.signature,
        ),
    );
    assert_eq!(
        ReceiptPayload::parse_canonical(&payload_bytes).unwrap(),
        receipt.payload,
    );
    assert_eq!(
        SignedReceipt::parse_canonical(&envelope_bytes).unwrap(),
        receipt,
    );

    let start = chain().remove(1);
    assert_eq!(
        String::from_utf8(start.payload.canonical_bytes()).unwrap(),
        format!(
            concat!(
                "{{\"schema\":\"louiselm.launch.receipt/2\",",
                "\"session_id\":\"session-1\",\"run_id\":\"run-1\",",
                "\"request_id\":\"request-start\",\"envelope_revision\":3,",
                "\"sequence\":1,\"previous_receipt_digest\":\"{}\",",
                "\"release_id\":\"{}\",\"signing_key_id\":\"{}\",",
                "\"outcome\":{{\"action\":\"start\",\"authority\":{{",
                "\"kind\":\"cause\",\"cause\":\"launch_acknowledged\"}}}},",
                "\"resulting_state\":\"running\"}}"
            ),
            chain()[0].digest(),
            digest("release"),
            digest("launcher-key"),
        ),
    );

    let mut alternate_signature = receipt.clone();
    alternate_signature.signature = digest("alternate-signature");
    assert_ne!(alternate_signature.digest(), receipt.digest());

    let mut padded = payload_bytes;
    padded.push(b'\n');
    assert_eq!(
        ReceiptPayload::parse_canonical(&padded),
        Err(ReceiptError::NonCanonical)
    );

    let mut json: serde_json::Value = serde_json::from_slice(&envelope_bytes).unwrap();
    json.as_object_mut()
        .unwrap()
        .insert("command".to_owned(), serde_json::json!("whoami"));
    assert!(matches!(
        SignedReceipt::parse_canonical(&serde_json::to_vec(&json).unwrap()),
        Err(ReceiptError::Malformed(_))
    ));

    let nested_unknown = String::from_utf8(envelope_bytes).unwrap().replace(
        r#""action":"launch","#,
        r#""action":"launch","command":"whoami","#,
    );
    assert!(matches!(
        SignedReceipt::parse_canonical(nested_unknown.as_bytes()),
        Err(ReceiptError::Malformed(_))
    ));

    assert!(
        serde_json::from_value::<ReceiptOutcome>(serde_json::json!({
            "action": "resume",
            "authority": { "kind": "cause", "cause": "broker_lost" }
        }))
        .is_err()
    );
}

#[test]
fn canonical_parsers_reject_oversize_and_noncanonical_digests() {
    let oversized = vec![b'x'; louiselm_skills::launch_receipt::MAX_RECEIPT_BYTES + 1];
    assert_eq!(
        ReceiptPayload::parse_canonical(&oversized),
        Err(ReceiptError::Oversized)
    );
    assert_eq!(
        SignedReceipt::parse_canonical(&oversized),
        Err(ReceiptError::Oversized)
    );

    let mut receipt = chain().remove(0);
    receipt.payload.release_id = Digest::parse(&receipt.payload.release_id)
        .unwrap()
        .hex()
        .to_owned();
    assert!(matches!(
        receipt.payload.validate(),
        Err(ReceiptError::InvalidDigest {
            field: "release_id"
        })
    ));

    let mut receipt = chain().remove(0);
    receipt.payload.signing_key_id = Digest::parse(&receipt.payload.signing_key_id)
        .unwrap()
        .hex()
        .to_owned();
    assert!(matches!(
        receipt.payload.validate(),
        Err(ReceiptError::InvalidDigest {
            field: "signing_key_id"
        })
    ));

    let mut receipt = chain().remove(0);
    if let ReceiptOutcome::Launch { evidence, .. } = &mut receipt.payload.outcome {
        evidence.capability_channel_ids.swap(0, 1);
    }
    assert_eq!(
        receipt.payload.validate(),
        Err(ReceiptError::UnsortedChannels)
    );
}

#[test]
fn payload_validation_rejects_each_contradictory_shape() {
    let launch = chain().remove(0);

    let mut wrong_schema = launch.payload.clone();
    wrong_schema.schema = "louiselm.launch.receipt/1".to_owned();
    assert!(matches!(
        wrong_schema.validate(),
        Err(ReceiptError::UnsupportedSchema(_))
    ));

    let mut bad_subject = launch.payload.clone();
    bad_subject.session_id = "../session".to_owned();
    assert_eq!(
        bad_subject.validate(),
        Err(ReceiptError::InvalidIdentifier {
            field: "session_id"
        })
    );

    let mut genesis_with_predecessor = launch.payload.clone();
    genesis_with_predecessor.previous_receipt_digest = Some(digest("predecessor"));
    assert_eq!(
        genesis_with_predecessor.validate(),
        Err(ReceiptError::GenesisHasPredecessor)
    );

    let mut later_without_predecessor = chain().remove(1).payload;
    later_without_predecessor.previous_receipt_digest = None;
    assert_eq!(
        later_without_predecessor.validate(),
        Err(ReceiptError::MissingPredecessor)
    );

    let mut later_launch = launch.payload.clone();
    later_launch.sequence = 1;
    later_launch.previous_receipt_digest = Some(digest("predecessor"));
    assert_eq!(later_launch.validate(), Err(ReceiptError::LaunchNotGenesis));

    let mut non_launch_genesis = chain().remove(1).payload;
    non_launch_genesis.sequence = 0;
    non_launch_genesis.previous_receipt_digest = None;
    assert_eq!(
        non_launch_genesis.validate(),
        Err(ReceiptError::GenesisNotLaunch)
    );

    let mut launch_claiming_running = launch.payload.clone();
    launch_claiming_running.resulting_state = SessionState::Running;
    assert_eq!(
        launch_claiming_running.validate(),
        Err(ReceiptError::ContradictoryResult)
    );

    let mut authorized_start = chain().remove(1).payload;
    authorized_start.outcome = ReceiptOutcome::Start {
        authority: ReceiptAuthority::Authorized(authorization("request-start")),
    };
    assert_eq!(
        authorized_start.validate(),
        Err(ReceiptError::ContradictoryCause)
    );

    let mut wrongly_caused_start = chain().remove(1).payload;
    wrongly_caused_start.outcome = ReceiptOutcome::Start {
        authority: ReceiptAuthority::Cause {
            cause: ReceiptCause::BrokerLost,
        },
    };
    assert_eq!(
        wrongly_caused_start.validate(),
        Err(ReceiptError::ContradictoryCause)
    );

    let mut wrong_contract = launch.payload.clone();
    if let ReceiptOutcome::Launch { evidence, .. } = &mut wrong_contract.outcome {
        evidence.isolation_contract = "louiselm.isolation/2".to_owned();
    }
    assert!(matches!(
        wrong_contract.validate(),
        Err(ReceiptError::UnsupportedIsolationContract(_))
    ));

    let mut missing_channels = launch.payload.clone();
    if let ReceiptOutcome::Launch { evidence, .. } = &mut missing_channels.outcome {
        evidence.capability_channel_ids.clear();
    }
    assert_eq!(
        missing_channels.validate(),
        Err(ReceiptError::MissingChannel)
    );

    let mut duplicate_channels = launch.payload.clone();
    if let ReceiptOutcome::Launch { evidence, .. } = &mut duplicate_channels.outcome {
        evidence.capability_channel_ids = vec!["acp".to_owned(), "acp".to_owned()];
    }
    assert_eq!(
        duplicate_channels.validate(),
        Err(ReceiptError::DuplicateChannel("acp".to_owned()))
    );

    let mut empty_signature = launch;
    empty_signature.signature.clear();
    assert_eq!(
        empty_signature.validate(),
        Err(ReceiptError::InvalidSignatureEncoding)
    );

    let mut oversized_payload = chain().remove(0).payload;
    if let ReceiptOutcome::Launch { evidence, .. } = &mut oversized_payload.outcome {
        evidence.capability_channel_ids = vec!["a".repeat(128); 600];
    }
    assert_eq!(oversized_payload.validate(), Err(ReceiptError::Oversized));

    let mut impossible_cause = chain().remove(2).payload;
    impossible_cause.outcome = ReceiptOutcome::Park {
        authority: ReceiptAuthority::Cause {
            cause: ReceiptCause::ProcessExited,
        },
    };
    assert_eq!(
        impossible_cause.validate(),
        Err(ReceiptError::ContradictoryCause)
    );

    let mut failed_launch_cleanup = chain().remove(5).payload;
    failed_launch_cleanup.outcome = ReceiptOutcome::Disposal {
        authority: ReceiptAuthority::Cause {
            cause: ReceiptCause::AcknowledgementFailed,
        },
    };
    failed_launch_cleanup
        .validate()
        .expect("failed receipt acknowledgement may require terminal cleanup");

    let mut mismatched_launch_request = chain().remove(0).payload;
    if let ReceiptOutcome::Launch { authorization, .. } = &mut mismatched_launch_request.outcome {
        authorization.request_digest = digest("another-launch-request");
    }
    assert_eq!(
        mismatched_launch_request.validate(),
        Err(ReceiptError::LaunchRequestMismatch)
    );
}

#[test]
fn a_complete_chain_and_a_suffix_from_a_trusted_head_verify() {
    let receipts = chain();
    let head = louiselm_skills::launch_receipt::verify_chain(&receipts, &anchor(), verifies)
        .expect("the complete receipt chain verifies");
    assert_eq!(head.sequence(), 5);
    assert_eq!(head.state(), SessionState::Terminal);
    assert_eq!(head.receipt_digest(), receipts[5].digest().to_string());

    let starting =
        louiselm_skills::launch_receipt::verify_chain(&receipts[..1], &anchor(), verifies).unwrap();
    assert_eq!(starting.sequence(), 0);
    assert_eq!(starting.state(), SessionState::Starting);
    let running =
        louiselm_skills::launch_receipt::verify_chain(&receipts[..2], &anchor(), verifies).unwrap();
    assert_eq!(running.sequence(), 1);
    assert_eq!(running.state(), SessionState::Running);

    let trusted =
        louiselm_skills::launch_receipt::verify_chain(&receipts[..3], &anchor(), verifies).unwrap();
    let suffix_head =
        louiselm_skills::launch_receipt::verify_suffix(&receipts[3..], &trusted, verifies)
            .expect("a continuous launcher-ahead suffix verifies");
    assert_eq!(suffix_head, head);
    assert_eq!(
        louiselm_skills::launch_receipt::verify_suffix(&[], &trusted, verifies).unwrap(),
        trusted,
    );
}

#[test]
fn verification_rejects_mutation_order_gaps_duplicates_foreign_prefixes_and_splices() {
    let base = chain();

    let mut cases = Vec::new();

    let mut mutated = base.clone();
    mutated[2].payload.envelope_revision = 5;
    cases.push(mutated);

    let mut reordered = base.clone();
    reordered.swap(1, 2);
    cases.push(reordered);

    let mut gap = base.clone();
    gap.remove(1);
    cases.push(gap);

    let mut duplicate = base.clone();
    duplicate.insert(2, duplicate[1].clone());
    cases.push(duplicate);

    let mut foreign = base.clone();
    foreign[0].payload.session_id = "session-2".to_owned();
    cases.push(foreign);

    let mut splice = base.clone();
    splice[2].payload.previous_receipt_digest = Some(digest("other-chain"));
    cases.push(splice);

    let mut wrong_release = base.clone();
    wrong_release[2].payload.release_id = digest("other-release");
    cases.push(wrong_release);

    let mut wrong_key = base.clone();
    wrong_key[2].payload.signing_key_id = digest("other-key");
    cases.push(wrong_key);

    let mut invalid_signature = base.clone();
    invalid_signature[2].signature = digest("forged");
    cases.push(invalid_signature);

    for receipts in cases {
        assert!(
            louiselm_skills::launch_receipt::verify_chain(&receipts, &anchor(), verifies).is_err(),
            "the attacked chain must be rejected"
        );
    }

    let trusted =
        louiselm_skills::launch_receipt::verify_chain(&base[..2], &anchor(), verifies).unwrap();
    assert!(
        louiselm_skills::launch_receipt::verify_suffix(&base[2..3], &trusted, verifies).is_ok()
    );
    let mut spliced_suffix = base[2..].to_vec();
    spliced_suffix[0].payload.previous_receipt_digest = Some(digest("foreign-head"));
    assert!(matches!(
        louiselm_skills::launch_receipt::verify_suffix(&spliced_suffix, &trusted, verifies),
        Err(ReceiptError::WrongPredecessor { .. })
    ));

    let mut reused_request = base[2..3].to_vec();
    reused_request[0].payload.request_id = "request-0".to_owned();
    reused_request[0].signature =
        Digest::of(&reused_request[0].payload.canonical_bytes()).to_string();
    assert!(matches!(
        louiselm_skills::launch_receipt::verify_suffix(&reused_request, &trusted, verifies),
        Err(ReceiptError::DuplicateRequestId { .. })
    ));
}

#[test]
fn verification_enforces_state_and_envelope_transitions() {
    let mut receipts = chain();
    receipts[2].payload.outcome = ReceiptOutcome::Start {
        authority: ReceiptAuthority::Cause {
            cause: ReceiptCause::LaunchAcknowledged,
        },
    };
    receipts[2].payload.resulting_state = SessionState::Running;
    receipts[2].signature = Digest::of(&receipts[2].payload.canonical_bytes()).to_string();
    receipts[3].payload.previous_receipt_digest = Some(receipts[2].digest().to_string());
    receipts[3].signature = Digest::of(&receipts[3].payload.canonical_bytes()).to_string();
    assert!(matches!(
        louiselm_skills::launch_receipt::verify_chain(&receipts, &anchor(), verifies),
        Err(ReceiptError::InvalidTransition { sequence: 2 })
    ));

    let mut receipts = chain();
    receipts[2].payload.envelope_revision = 2;
    receipts[2].signature = Digest::of(&receipts[2].payload.canonical_bytes()).to_string();
    assert!(matches!(
        louiselm_skills::launch_receipt::verify_chain(&receipts, &anchor(), verifies),
        Err(ReceiptError::EnvelopeRevisionRegressed {
            previous: 3,
            found: 2,
            sequence: 2
        })
    ));

    let mut receipt = chain().remove(2);
    if let ReceiptOutcome::Park { authority } = &mut receipt.payload.outcome {
        *authority = ReceiptAuthority::Authorized(authorization("different-request"));
    }
    assert!(matches!(
        receipt.payload.validate(),
        Err(ReceiptError::AuthorizationRequestMismatch)
    ));
}

type PendingSign = Option<(Vec<u8>, Completion<String, &'static str>)>;

#[derive(Default)]
struct DeferredSigner {
    pending: Mutex<PendingSign>,
}

impl ReceiptSigner for DeferredSigner {
    type Error = &'static str;

    fn sign(&self, payload_bytes: Vec<u8>, complete: Completion<String, Self::Error>) {
        *self.pending.lock().unwrap() = Some((payload_bytes, complete));
    }
}

impl DeferredSigner {
    fn complete(&self) {
        let (payload_bytes, complete) = self.pending.lock().unwrap().take().unwrap();
        complete(Ok(Digest::of(&payload_bytes).to_string()));
    }
}

type PendingAppend = Option<(Vec<u8>, Completion<(), &'static str>)>;

#[derive(Default)]
struct DeferredAppender {
    pending: Mutex<PendingAppend>,
}

impl ReceiptAppender for DeferredAppender {
    type Error = &'static str;

    fn append(&self, receipt_bytes: Vec<u8>, complete: Completion<(), Self::Error>) {
        *self.pending.lock().unwrap() = Some((receipt_bytes, complete));
    }
}

impl DeferredAppender {
    fn complete(&self) {
        let (_bytes, complete) = self.pending.lock().unwrap().take().unwrap();
        complete(Ok(()));
    }
}

#[test]
fn signing_and_persistence_ports_can_complete_after_the_call_returns() {
    let signer = DeferredSigner::default();
    let signed_result = Arc::new(Mutex::new(None));
    let capture = Arc::clone(&signed_result);
    let payload = chain().remove(0).payload;
    let payload_bytes = payload.canonical_bytes();
    signer.sign(
        payload_bytes.clone(),
        Box::new(move |result| *capture.lock().unwrap() = Some(result.unwrap())),
    );
    assert!(signed_result.lock().unwrap().is_none());
    assert_eq!(
        signer.pending.lock().unwrap().as_ref().unwrap().0,
        payload_bytes,
        "the signer receives exact canonical payload bytes",
    );
    signer.complete();
    let receipt = SignedReceipt {
        schema: SIGNED_RECEIPT_SCHEMA.to_owned(),
        payload,
        signature: signed_result.lock().unwrap().take().unwrap(),
    };

    let appender = DeferredAppender::default();
    let appended = Arc::new(Mutex::new(false));
    let capture = Arc::clone(&appended);
    let receipt_bytes = receipt.canonical_bytes();
    appender.append(
        receipt_bytes.clone(),
        Box::new(move |result| {
            result.unwrap();
            *capture.lock().unwrap() = true;
        }),
    );
    assert!(!*appended.lock().unwrap());
    assert_eq!(
        appender.pending.lock().unwrap().as_ref().unwrap().0,
        receipt_bytes,
        "the appender receives exact canonical signed-envelope bytes",
    );
    appender.complete();
    assert!(*appended.lock().unwrap());
}
