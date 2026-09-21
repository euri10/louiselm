use super::*;
use louiselm_skills::{
    broker::{BrokerSession, lifecycle::LifecycleCaller},
    launch_protocol::{BrokerConnection, ChannelState, SupervisorStatus},
};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

pub(super) enum Reply {
    Chunk(Vec<u8>),
    Complete,
    Unknown,
    Redirect,
}

struct Effect {
    output: Vec<u8>,
    pending: bool,
    unknown: bool,
}

pub(super) struct Proof {
    frames: framing::Frames,
    service: BrokerService,
    session: BrokerSession,
    peer: SeqpacketChannel,
    pub binding: LaunchAuthorization,
    pub current: SupervisorStatus,
    // Trusted fixture output of .3.6's enrollment, never parsed from HTTP.
    pub sender_enrolled: Arc<AtomicBool>,
    pub clock: Arc<AtomicU64>,
    pub before_status_reply: Option<Box<dyn FnOnce() + Send>>,
    remaining: u32,
    effects: Vec<Effect>,
    replies: mpsc::Receiver<(usize, Reply)>,
    upstream: mpsc::Sender<(usize, Reply)>,
    _root: TempDir,
}

impl Proof {
    pub(super) fn new(reservations: u32) -> Self {
        let root = TempDir::new().unwrap();
        let socket = root.path().join("broker.sock");
        let request = request("request-proof");
        let service = lifecycle::bound_service(root.path(), &socket, &request);
        let peer = thread::spawn(move || fake_supervisor(&socket, &request, 2000));
        let session = service
            .serve_launch(2000, verify_fixture_signature)
            .unwrap();
        let (binding, peer) = peer.join().unwrap();
        let current = lifecycle::status(&binding);
        let (upstream, replies) = mpsc::channel();
        Self {
            frames: framing::Frames::default(),
            service,
            session,
            peer,
            binding,
            current,
            sender_enrolled: Arc::new(AtomicBool::new(true)),
            clock: Arc::new(AtomicU64::new(2000)),
            before_status_reply: None,
            remaining: reservations,
            effects: Vec::new(),
            replies,
            upstream,
            _root: root,
        }
    }
    pub(super) fn feed(&mut self, bytes: &[u8]) -> Result<(), ProtocolError> {
        let result = self.frames.feed(bytes);
        if result.is_err() {
            self.stop();
        }
        result
    }
    pub(super) fn admit(&mut self) -> Result<Option<usize>, ProtocolError> {
        let result = self.admit_one();
        if result.is_err() {
            self.stop();
        }
        result
    }

    fn admit_one(&mut self) -> Result<Option<usize>, ProtocolError> {
        let Some(model) = self.frames.next()? else {
            return Ok(None);
        };
        self.check()?;
        if model != "fixture-model" || self.remaining == 0 {
            return Err(denied());
        }
        // First fake upstream effect is this append, in the same exclusive
        // owner turn as authorization/reservation. No unchecked permit leaves
        // the owner. Production .3.2 must replace this reservation double with
        // its durable shared Run transaction and recheck after blocking I/O.
        self.remaining -= 1;
        let id = self.effects.len();
        self.effects.push(Effect {
            output: Vec::new(),
            pending: true,
            unknown: false,
        });
        Ok(Some(id))
    }

    fn check_local(&self) -> Result<(), ProtocolError> {
        if !self.sender_enrolled.load(Ordering::SeqCst)
            || self.session.channel().is_closed()
            || self.binding != *self.session.authorization()
            || self.clock.load(Ordering::SeqCst) >= self.binding.expires_at_ms
        {
            return Err(denied());
        }
        Ok(())
    }

    fn check(&mut self) -> Result<(), ProtocolError> {
        self.check_local()?;
        // The existing worker owns &mut BrokerSession for both controls and
        // admissions. The real authenticated status exchange is exercised;
        // supervisor mechanics and its sender-proof input remain fixture data.
        let status = thread::scope(|scope| {
            let peer = &self.peer;
            let current = &self.current;
            let hook = self.before_status_reply.take();
            let observed_at = self.clock.load(Ordering::SeqCst);
            let answer = scope.spawn(move || {
                if let Some(hook) = hook {
                    hook();
                }
                lifecycle::answer_one_status_query(peer, current);
            });
            let status = self.service.session_status(
                &mut self.session,
                &LifecycleCaller::Agent,
                observed_at,
                verify_fixture_signature,
            );
            answer.join().unwrap();
            status
        })
        .map_err(|_| denied())?;
        if status.state != SessionState::Running
            || status.channel_state != ChannelState::Enabled
            || status.broker_connection != BrokerConnection::Connected
            || status.pending_operation.is_some()
        {
            return Err(denied());
        }
        // Status I/O cannot extend expiry or retain a lost sender's authority.
        self.check_local()
    }

    pub(super) fn effects(&self) -> usize {
        self.effects.len()
    }
    pub(super) fn expire(&self) {
        self.clock
            .store(self.binding.expires_at_ms, Ordering::SeqCst);
    }
    pub(super) fn invalidate_sender(&self) {
        self.sender_enrolled.store(false, Ordering::SeqCst);
    }
    pub(super) fn remaining(&self) -> u32 {
        self.remaining
    }
    pub(super) fn output(&self, id: usize) -> &[u8] {
        &self.effects[id].output
    }
    pub(super) fn unknown(&self, id: usize) -> bool {
        self.effects[id].unknown
    }

    pub(super) fn stop(&mut self) {
        // Existing ownership boundary: dropping/closing this Session closes
        // the retained supervisor channel. Cancellation cannot undo upstream
        // effects. Every started but unfinished operation becomes unknown.
        self.session.close();
        for effect in &mut self.effects {
            if effect.pending {
                effect.unknown = true;
                effect.pending = false;
            }
        }
    }

    pub(super) fn tick(&mut self) {
        if self.check().is_err() {
            self.stop();
        }
    }

    pub(super) fn reply(&self, id: usize, reply: Reply) {
        // Actual asynchronous completion context; callbacks only enqueue data.
        let sender = self.upstream.clone();
        thread::spawn(move || sender.send((id, reply)).unwrap())
            .join()
            .unwrap();
    }

    pub(super) fn poll(&mut self) {
        self.tick();
        while let Ok((id, reply)) = self.replies.try_recv() {
            let effect = &mut self.effects[id];
            if !effect.pending {
                continue;
            }
            match reply {
                Reply::Chunk(bytes) if bytes.len() <= 4096 - effect.output.len() => {
                    effect.output.extend(bytes);
                }
                Reply::Complete => effect.pending = false,
                Reply::Chunk(_) | Reply::Unknown | Reply::Redirect => {
                    effect.pending = false;
                    effect.unknown = true;
                }
            }
        }
    }
}

fn denied() -> ProtocolError {
    ProtocolError::new(ErrorCode::CapabilityDenied, None, None)
}
