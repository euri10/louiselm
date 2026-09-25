//! Production load, sender and owner-loss proof with disposable unprivileged peers.

mod peer;
mod process;
use crate::{
    conformance::{Check, Cleanup, Outcome, SENDER_GUARD_CHECK},
    launch_protocol::GuardScope,
    launch_supervisor::sender_guard::{GuardError, GuardedSocket, SenderGuard},
    launch_transport::{
        CredentialPin, KernelCredentials, KernelProcess, SeqpacketConnector, TransportError,
    },
};
pub use peer::serve as serve_peer;
use peer::{Reply, Request};
use process::Peer;

#[cfg(test)]
std::thread_local! {
    pub(crate) static REFUSE_GUARD_LOAD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
use std::{
    fs::{self, File},
    io::{self, IoSlice, Read},
    net::{SocketAddr, TcpListener, TcpStream},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::mpsc,
    time::{Duration, Instant},
};

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error("guard probe I/O unavailable")]
    Io(#[from] io::Error),
    #[error(transparent)]
    Guard(#[from] GuardError),
    #[error("guard probe authenticated channel unavailable")]
    Transport(#[from] TransportError),
    #[error("guard probe {0}")]
    Observation(&'static str),
}

struct Probe {
    peers: Vec<Peer>,
    guard: Option<SenderGuard>,
    endpoint: Option<GuardedSocket<TcpListener>>,
    upstream: Option<GuardedSocket<TcpStream>>,
    runtime: Option<KernelProcess>,
    maps: Vec<u32>,
}

/// The dedicated thread's exit releases its private pin namespace. No tested
/// endpoint or process survives that exit; map IDs prove final resource release.
pub(super) fn run(
    executable: PathBuf,
    root: PathBuf,
    runtime_uid: u32,
    broker_uid: u32,
    deadline: Instant,
) -> (Check, Cleanup) {
    #[cfg(test)]
    let refuse_load = REFUSE_GUARD_LOAD.with(|refuse| refuse.replace(false));
    let worker = std::thread::Builder::new()
        .name("conformance-guard".into())
        .spawn(move || {
            let mut probe = Probe {
                peers: Vec::new(),
                guard: None,
                endpoint: None,
                upstream: None,
                runtime: None,
                maps: Vec::new(),
            };
            #[cfg(test)]
            let privilege = if refuse_load {
                restrict_loader()
            } else {
                Ok(())
            };
            #[cfg(not(test))]
            let privilege = Ok::<(), Error>(());
            let observed = privilege.and_then(|()| {
                probe.execute(&executable, &root, runtime_uid, broker_uid, deadline)
            });
            let clean = probe.cleanup();
            (observed, clean, probe.maps)
        });
    let (observed, clean, maps) = match worker {
        Ok(worker) => match worker.join() {
            Ok(result) => result,
            Err(_) => (
                Err(Error::Observation("worker interrupted")),
                false,
                Vec::new(),
            ),
        },
        Err(error) => (Err(Error::Io(error)), true, Vec::new()),
    };
    let clean = clean && maps_released(&maps, deadline);
    let (control, confined) = observed.unwrap_or_else(|error| {
        (
            Outcome::Error(error.to_string()),
            Outcome::Error(error.to_string()),
        )
    });
    (
        Check {
            name: SENDER_GUARD_CHECK.into(),
            control,
            confined,
        },
        if clean {
            Cleanup::Confirmed
        } else {
            Cleanup::Unconfirmed
        },
    )
}

#[cfg(test)]
fn restrict_loader() -> Result<(), Error> {
    // The real loader lacks required privilege on this disposable thread only.
    use rustix::thread::{CapabilitySet, capabilities, set_capabilities};
    let mut sets = capabilities(None).map_err(io::Error::from)?;
    sets.effective
        .remove(CapabilitySet::SYS_ADMIN | CapabilitySet::BPF | CapabilitySet::PERFMON);
    set_capabilities(None, sets).map_err(io::Error::from)?;
    Ok(())
}

fn ready(reply: &Reply) -> Result<(), Error> {
    if matches!(reply, Reply::Ready) {
        Ok(())
    } else {
        Err(Error::Observation("unexpected response"))
    }
}
fn observed(reply: Reply) -> Result<Outcome, Error> {
    if let Reply::Observed(outcome) = reply {
        Ok(outcome)
    } else {
        Err(Error::Observation("missing write observation"))
    }
}

impl Probe {
    fn enroll(
        &mut self,
        executable: &Path,
        root: &Path,
        runtime_uid: u32,
        broker_uid: u32,
        deadline: Instant,
    ) -> Result<(GuardScope, SocketAddr), Error> {
        use rustix::process::{Gid, PidfdFlags, Uid, pidfd_open};
        let directory = root.join("guard-peer");
        fs::create_dir(&directory)?;
        rustix::fs::chown(
            &directory,
            Some(Uid::from_raw(broker_uid)),
            Some(Gid::from_raw(broker_uid)),
        )
        .map_err(io::Error::from)?;
        let channel = directory.join("control");
        let handoff = directory.join("handoff");
        self.peers.push(Peer::spawn(executable, broker_uid)?);
        ready(&self.peers[0].request(
            &Request::Broker {
                channel: channel.clone(),
                handoff: handoff.clone(),
                owner: std::process::id(),
            },
            deadline,
        )?)?;
        let connector = SeqpacketConnector::new()?;
        let (send, receive) = mpsc::sync_channel(1);
        connector.connect(
            &channel,
            CredentialPin::Process(KernelCredentials {
                pid: self.peers[0].pid().as_raw_nonzero().get().cast_unsigned(),
                uid: broker_uid,
                gid: broker_uid,
            }),
            Box::new(move |result| {
                // A timed-out probe already refuses certification.
                let _ = send.send(result);
            }),
        )?;
        let broker = receive
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| Error::Observation("connection deadline"))??;
        let now = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
        let scope = GuardScope {
            session_id: format!("conformance-{}", std::process::id()),
            run_id: "conformance".into(),
            revision: 1,
            deadline_ns: u64::try_from(now.tv_sec)
                .map_err(|_| Error::Observation("clock unavailable"))?
                * 1_000_000_000
                + 180_000_000_000,
        };
        self.guard = Some(SenderGuard::load(scope.clone(), broker)?);
        let guard = self
            .guard
            .as_mut()
            .ok_or(Error::Observation("loader absent"))?;
        self.maps = guard.conformance_map_ids()?;
        self.endpoint = Some(
            guard.bind_endpoint(
                "127.0.0.1:0"
                    .parse()
                    .map_err(|_| Error::Observation("invalid endpoint"))?,
            )?,
        );
        let address = self
            .endpoint
            .as_ref()
            .ok_or(Error::Observation("endpoint absent"))?
            .local_addr()?;
        self.peers.push(Peer::spawn(executable, runtime_uid)?);
        ready(&self.peers[1].request(&Request::Runtime, deadline)?)?;
        self.peers[1].freeze(deadline)?;
        self.runtime = Some(KernelProcess::from_exec_stop(
            KernelCredentials {
                pid: self.peers[1].pid().as_raw_nonzero().get().cast_unsigned(),
                uid: runtime_uid,
                gid: runtime_uid,
            },
            pidfd_open(self.peers[1].pid(), PidfdFlags::empty()).map_err(io::Error::from)?,
            &File::open(executable)?,
        )?);
        let runtime = self
            .runtime
            .as_ref()
            .ok_or(Error::Observation("runtime absent"))?;
        guard.enroll_at_exec_stop(runtime)?;
        self.peers[1].resume()?;
        self.peers[0].send(&Request::Enrollment)?;
        guard.announce_enrollment(
            "conformance",
            deadline.saturating_duration_since(Instant::now()),
        )?;
        ready(&self.peers[0].receive(deadline)?)?;
        Ok((scope, address))
    }

    fn execute(
        &mut self,
        executable: &Path,
        root: &Path,
        runtime_uid: u32,
        broker_uid: u32,
        deadline: Instant,
    ) -> Result<(Outcome, Outcome), Error> {
        let (scope, address) = self.enroll(executable, root, runtime_uid, broker_uid, deadline)?;
        let guard = self
            .guard
            .as_mut()
            .ok_or(Error::Observation("loader absent"))?;
        let runtime = self
            .runtime
            .as_ref()
            .ok_or(Error::Observation("runtime absent"))?;
        let inactive = observed(self.peers[1].request(&Request::Send(address), deadline)?)?;
        guard.activate(&scope)?;
        let control = observed(self.peers[1].request(&Request::Send(address), deadline)?)?;
        if control != Outcome::Allowed || !matches!(inactive, Outcome::Denied(_)) {
            return Ok((control, inactive));
        }
        let unauthorized = observed(self.peers[0].request(&Request::Send(address), deadline)?)?;
        if !matches!(unauthorized, Outcome::Denied(_)) {
            return Ok((control, unauthorized));
        }
        let listener = TcpListener::bind("127.0.0.1:0")?;
        self.upstream =
            Some(guard.connect_upstream(&scope, listener.local_addr()?, Duration::from_secs(2))?);
        let (mut accepted, _) = listener.accept()?;
        accepted.set_read_timeout(Some(Duration::from_secs(2)))?;
        self.peers[0].send(&Request::Take)?;
        send_socket(
            &root.join("guard-peer/handoff"),
            self.upstream
                .as_ref()
                .ok_or(Error::Observation("upstream absent"))?,
        )?;
        ready(&self.peers[0].receive(deadline)?)?;
        let upstream_control = observed(self.peers[0].request(&Request::Write, deadline)?)?;
        if upstream_control != Outcome::Allowed {
            return Ok((
                upstream_control,
                Outcome::Error("upstream positive control failed".into()),
            ));
        }
        let mut bytes = [0; 9];
        accepted.read_exact(&mut bytes)?;
        if &bytes != b"sentinel\n" {
            return Err(Error::Observation("upstream sentinel mismatch"));
        }
        // Keep the broker and its exact socket alive across runtime loss. The
        // second write must be refused by the kernel, without userspace preflight.
        self.peers[1].dispose()?;
        let lost = observed(self.peers[0].request(&Request::Write, deadline)?)?;
        if guard.activate(&scope) != Err(GuardError::Lost) || runtime.valid()? {
            return Ok((control, Outcome::Allowed));
        }
        Ok((control, lost))
    }

    fn cleanup(&mut self) -> bool {
        let mut clean = true;
        for peer in &mut self.peers {
            clean &= peer.dispose().is_ok();
        }
        if let Some(guard) = &mut self.guard {
            clean &= guard.dispose().is_ok();
        }
        self.upstream.take();
        self.endpoint.take();
        self.runtime.take();
        self.guard.take();
        clean
    }
}

fn send_socket(path: &Path, socket: &GuardedSocket<TcpStream>) -> io::Result<()> {
    let channel = UnixStream::connect(path)?;
    let fds = socket.descriptors();
    let mut space = [std::mem::MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(3))];
    let mut ancillary = rustix::net::SendAncillaryBuffer::new(&mut space);
    if !ancillary.push(rustix::net::SendAncillaryMessage::ScmRights(&fds)) {
        return Err(io::ErrorKind::InvalidData.into());
    }
    if rustix::net::sendmsg(
        &channel,
        &[IoSlice::new(b"G")],
        &mut ancillary,
        rustix::net::SendFlags::NOSIGNAL,
    )? != 1
    {
        return Err(io::ErrorKind::WriteZero.into());
    }
    Ok(())
}

fn maps_released(ids: &[u32], deadline: Instant) -> bool {
    loop {
        let mut remaining = false;
        for id in ids {
            match libbpf_rs::MapHandle::from_map_id(*id) {
                Ok(_) => remaining = true,
                Err(error) if error.kind() == libbpf_rs::ErrorKind::NotFound => (),
                Err(_) => return false,
            }
        }
        if !remaining {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
