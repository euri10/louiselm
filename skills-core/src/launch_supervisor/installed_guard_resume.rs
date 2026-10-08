//! Warm Resume through the installed broker, retained runtime and real signer.
use super::*;
use crate::launch_receipt::{ReceiptOutcome, ReceiptPayload};
use std::sync::{Condvar, Mutex};

type HeldSignature = (Vec<u8>, SupervisorCompletion<String>);

pub(super) struct ResumeSigner {
    signer: InstalledLaunchSigner,
    hold: bool,
    pending: Mutex<Option<HeldSignature>>,
    changed: Condvar,
}

impl ResumeSigner {
    pub(super) fn new(signer: InstalledLaunchSigner, hold: bool) -> Self {
        Self {
            signer,
            hold,
            pending: Mutex::new(None),
            changed: Condvar::new(),
        }
    }

    fn wait_for_resume(&self) {
        let (pending, timeout) = self
            .changed
            .wait_timeout_while(
                self.pending.lock().unwrap(),
                Duration::from_secs(5),
                |pending| pending.is_none(),
            )
            .unwrap();
        assert!(
            !timeout.timed_out(),
            "retained runtime never reached Resume signing"
        );
        assert!(pending.is_some());
    }

    fn release(&self) {
        let (bytes, complete) = self.pending.lock().unwrap().take().unwrap();
        self.signer.sign(bytes, complete).unwrap();
    }
}

impl LaunchSigner for ResumeSigner {
    fn release_id(&self) -> &str {
        self.signer.release_id()
    }
    fn signing_key_id(&self) -> &str {
        self.signer.signing_key_id()
    }
    fn check_authority(&self, complete: SupervisorCompletion<()>) -> Result<(), SupervisorError> {
        self.signer.check_authority(complete)
    }
    fn record_containment(
        &self,
        session: String,
        containment: crate::launcher_install::KeyContainment,
        complete: SupervisorCompletion<()>,
    ) -> Result<(), SupervisorError> {
        self.signer
            .record_containment(session, containment, complete)
    }
    fn complete_session(
        &self,
        terminal: ReceiptPayload,
        complete: SupervisorCompletion<()>,
    ) -> Result<(), SupervisorError> {
        let pending = self.pending.lock().unwrap().take();
        if let Some((_, cancelled)) = pending {
            cancelled(Err(SupervisorError::SigningUnavailable));
        }
        self.signer.complete_session(terminal, complete)
    }
    fn sign(
        &self,
        bytes: Vec<u8>,
        complete: SupervisorCompletion<String>,
    ) -> Result<(), SupervisorError> {
        let payload = ReceiptPayload::parse_canonical(&bytes).unwrap();
        if self.hold && matches!(payload.outcome, ReceiptOutcome::Resume { .. }) {
            assert!(
                self.pending
                    .lock()
                    .unwrap()
                    .replace((bytes, complete))
                    .is_none()
            );
            self.changed.notify_all();
            Ok(())
        } else {
            self.signer.sign(bytes, complete)
        }
    }
}

fn request(
    id: &str,
    action: LifecycleAction,
    state: SessionState,
    sequence: u64,
) -> LifecycleRequest {
    LifecycleRequest {
        schema: LIFECYCLE_REQUEST_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: id.into(),
        session_id: "session".into(),
        run_id: "run".into(),
        authorization_id: format!("operator-{id}"),
        action,
        expected_state: state,
        expected_receipt_sequence: Some(sequence),
        envelope_revision: 1,
    }
}

pub(super) fn park_resume(broker: &InstalledBroker, session: &mut BrokerSession, uid: u32) {
    let caller = LifecycleCaller::Operator { uid };
    let activated = network_posture(broker, session, &caller);
    assert_eq!(activated.state, crate::posture::DimensionState::Verified);
    let parked = broker
        .request_lifecycle(
            session,
            &caller,
            &request("warm-park", LifecycleAction::Park, SessionState::Running, 1),
        )
        .unwrap();
    assert_eq!(parked.payload.resulting_state, SessionState::Parked);
    let parked = network_posture(broker, session, &caller);
    assert_eq!(parked.state, crate::posture::DimensionState::Failed);
    assert_eq!(
        parked.freshness.last_verified_at_ms,
        activated.freshness.last_verified_at_ms
    );
    println!("BROKER_WARM_PARKED");
    let resume = request(
        "warm-resume",
        LifecycleAction::Resume,
        SessionState::Parked,
        2,
    );
    let running = broker.request_lifecycle(session, &caller, &resume).unwrap();
    assert_eq!(running.payload.sequence, 3);
    assert_eq!(running.payload.resulting_state, SessionState::Running);
    let resumed = network_posture(broker, session, &caller);
    assert_eq!(resumed.state, crate::posture::DimensionState::Verified);
    assert!(resumed.freshness.last_verified_at_ms > activated.freshness.last_verified_at_ms);
    // Exact replay cannot allocate or thaw again.
    assert_eq!(
        broker.request_lifecycle(session, &caller, &resume).unwrap(),
        running
    );
    println!("BROKER_WARM_RESUMED");
}

pub(super) fn finish(broker: &InstalledBroker, session: &mut BrokerSession, uid: u32) {
    let caller = LifecycleCaller::Operator { uid };
    broker
        .request_lifecycle(
            session,
            &caller,
            &request(
                "warm-final-park",
                LifecycleAction::Park,
                SessionState::Running,
                3,
            ),
        )
        .unwrap();
    println!("BROKER_PARKED");
    broker
        .request_lifecycle(
            session,
            &caller,
            &request(
                "warm-final-dispose",
                LifecycleAction::Disposal,
                SessionState::Parked,
                4,
            ),
        )
        .unwrap();
    assert_eq!(
        broker.inspect("session").unwrap().unwrap().state,
        SessionState::Terminal
    );
    println!("BROKER_TERMINAL");
}

pub(super) fn drive(
    root: &Path,
    lines: &mpsc::Receiver<String>,
    signer: &ResumeSigner,
    runtime: u32,
    address: SocketAddr,
    (input, output): (
        &mut std::os::unix::net::UnixStream,
        &mut std::os::unix::net::UnixStream,
    ),
) {
    output
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let before = provider_exchange(input, output, address, "valid-once");
    assert!(before.contains("HTTP/1.1 200 OK"));
    assert_eq!(live_spent(root), 1);
    assert_eq!(
        provider_exchange(input, output, address, "retain-socket"),
        "RETAINED"
    );
    fs::write(root.join("guard-warm-request"), b"").unwrap();
    marker(lines, "BROKER_WARM_PARKED");
    signer.wait_for_resume();
    // The same process has thawed after authenticated handoff, but no Running
    // receipt is signed/stored yet. Probe must be a kernel denial, not an HTTP refusal.
    assert!(Path::new(&format!("/proc/{runtime}")).exists());
    assert_eq!(
        provider_exchange(input, output, address, "guard-probe"),
        "DENIED"
    );
    assert_eq!(live_spent(root), 1);
    signer.release();
    marker(lines, "BROKER_WARM_RESUMED");
    let retired = provider_exchange(input, output, address, "probe-retained");
    assert!(
        retired == "DENIED" || retired == "CLOSED",
        "retired socket regained authority: {retired}"
    );
    let after = provider_exchange(input, output, address, "valid-once");
    assert!(after.contains("HTTP/1.1 200 OK"));
    assert!(after.contains("event: first") && after.contains("event: last"));
    assert_eq!(live_spent(root), 2);
    let denied = provider_exchange(input, output, address, "valid-once");
    assert!(denied.contains("HTTP/1.1 429 Too Many Requests"));
    assert_eq!(live_spent(root), 2, "Resume must not reset Run spending");
    fs::write(root.join("guard-provider-done"), b"").unwrap();
    println!("WARM_RESUME_PRE_ACK_DENIED POST_ACK_PROVIDER_OK SPENT_RETAINED=2");
}
