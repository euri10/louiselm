//! Capture creation and exec directly; never select an Agent by socket arrival.
//!
//! Attach only to our unreaped bootstrap child, whose PID cannot be reused.
//! Kernel fork events then supply stopped, lifetime-pinned descendants. The
//! tracer remains on the preparing thread until all tracees are detached.

use std::{
    ffi::{c_int, c_long, c_uint, c_ulong, c_void},
    fs, io,
    marker::PhantomData,
    os::fd::{AsFd, OwnedFd},
    rc::Rc,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use rustix::process::{
    Pid, PidfdFlags, Signal, WaitId, WaitIdOptions, kill_process, pidfd_open, pidfd_send_signal,
    waitid,
};

use crate::launch_transport::{KernelCredentials, KernelProcess};

const SEIZE: c_uint = 0x4206;
const SETOPTIONS: c_uint = 0x4200;
const GETEVENTMSG: c_uint = 0x4201;
const CONT: c_uint = 7;
const DETACH: c_uint = 17;
const TRACEFORK: usize = 1 << 1;
const TRACEEXEC: usize = 1 << 4;
const EXITKILL: usize = 1 << 20;
const EVENT_FORK: i32 = 1;
const EVENT_EXEC: i32 = 4;
const EVENT_STOP: i32 = 128;
const WAIT_ALL: u32 = 0x4000_0000;
const TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
struct Tracee {
    pid: Pid,
    pin: Option<OwnedFd>,
}

#[derive(Debug)]
pub(super) struct Trace {
    tracees: Vec<Tracee>,
    current: Pid,
    bootstrap: Pid,
    // Linux assigns tracing ownership to a thread, not merely its process.
    _same_thread: PhantomData<Rc<()>>,
}

impl Trace {
    /// The caller owns an unreaped Child for this PID throughout attachment.
    pub fn attach_child(pid: u32) -> io::Result<Self> {
        let pid = checked_pid(pid)?;
        let pin = pidfd_open(pid, PidfdFlags::empty())?;
        operation(SEIZE, pid, TRACEFORK | EXITKILL)?;
        Ok(Self {
            tracees: vec![Tracee {
                pid,
                pin: Some(pin),
            }],
            current: pid,
            bootstrap: pid,
            _same_thread: PhantomData,
        })
    }

    /// Lets the kernel-identified namespace reaper reach the startup gate.
    pub fn prepare_reaper(&mut self) -> io::Result<u32> {
        let monitor = self.current;
        let reaper = self.fork_child(monitor, Instant::now() + TIMEOUT)?;
        self.detach(monitor)?;
        operation(CONT, reaper, 0)?;
        self.current = reaper;
        Ok(reaper.as_raw_nonzero().get().unsigned_abs())
    }

    /// Called after the sequence-zero ACK and release of Bubblewrap's gate.
    pub fn authenticate(
        mut self,
        expected: &fs::File,
        uid: u32,
        gid: u32,
    ) -> io::Result<Arc<KernelProcess>> {
        let agent = self.stop_at_exec()?;
        verify_credentials(agent.as_raw_nonzero().get().unsigned_abs(), uid, gid)?;
        self.pin_executable(agent, expected, uid, gid)
    }

    fn stop_at_exec(&mut self) -> io::Result<Pid> {
        let deadline = Instant::now() + TIMEOUT;
        let reaper = self.current;
        let agent = self.fork_child(reaper, deadline)?;
        self.detach(reaper)?;
        operation(SETOPTIONS, agent, TRACEFORK | TRACEEXEC | EXITKILL)?;
        operation(CONT, agent, 0)?;
        let event = wait_event(agent, deadline)?;
        if event == EVENT_FORK {
            self.capture_child(agent, deadline)?;
        }
        if event != EVENT_EXEC {
            return Err(io::Error::other("unsupported Agent startup process event"));
        }
        Ok(agent)
    }

    fn pin_executable(
        mut self,
        agent: Pid,
        expected: &fs::File,
        uid: u32,
        gid: u32,
    ) -> io::Result<Arc<KernelProcess>> {
        let host_pid = agent.as_raw_nonzero().get().unsigned_abs();
        // The trace stop prevents exec/exit/reuse while the kernel pin and
        // expected executable are associated. No Agent instructions ran yet.
        let index = self.index(agent)?;
        let pin = self.tracees[index]
            .pin
            .as_ref()
            .ok_or_else(|| io::Error::other("missing Agent lifetime pin"))?;
        let duplicate = rustix::io::fcntl_dupfd_cloexec(pin, 0)?;
        let identity = KernelProcess::from_exec_stop(
            KernelCredentials {
                pid: host_pid,
                uid,
                gid,
            },
            duplicate,
            expected,
        )?;
        self.detach(agent)?;
        Ok(Arc::new(identity))
    }

    fn fork_child(&mut self, parent: Pid, deadline: Instant) -> io::Result<Pid> {
        expect_event(parent, EVENT_FORK, deadline)?;
        self.capture_child(parent, deadline)
    }

    fn capture_child(&mut self, parent: Pid, deadline: Instant) -> io::Result<Pid> {
        let child = event_pid(parent)?;
        // Record ownership before any fallible pin operation: the child is
        // kernel-stopped and has not been waited/reaped, so this PID is stable.
        self.tracees.push(Tracee {
            pid: child,
            pin: None,
        });
        let index = self.index(child)?;
        self.tracees[index].pin = Some(pidfd_open(child, PidfdFlags::empty())?);
        expect_event(child, EVENT_STOP, deadline)?;
        Ok(child)
    }

    fn index(&self, pid: Pid) -> io::Result<usize> {
        self.tracees
            .iter()
            .position(|tracee| tracee.pid == pid)
            .ok_or_else(|| io::Error::other("unowned startup tracee"))
    }

    fn detach(&mut self, pid: Pid) -> io::Result<()> {
        let index = self.index(pid)?;
        operation(DETACH, pid, 0)?;
        self.tracees.swap_remove(index);
        Ok(())
    }
}

impl Drop for Trace {
    fn drop(&mut self) {
        for tracee in &self.tracees {
            // This is not cleanup proof: the enclosing sandbox owner must
            // still prove an empty tree before releasing its identity lease.
            if let Some(pin) = &tracee.pin {
                let _ = pidfd_send_signal(pin, Signal::KILL);
            } else {
                // Only the just-created, never-waited kernel tracee can lack
                // a pin. Its stopped/unreaped lifetime prevents PID reuse.
                let _ = kill_process(tracee.pid, Signal::KILL);
            }
        }
        let deadline = Instant::now() + TIMEOUT;
        for tracee in &self.tracees {
            if tracee.pid == self.bootstrap {
                // The surrounding std::process::Child owns this exit status
                // and needs it to prove cleanup; never reap it on its behalf.
                continue;
            }
            while Instant::now() < deadline {
                let target = tracee
                    .pin
                    .as_ref()
                    .map_or(WaitId::Pid(tracee.pid), |pin| WaitId::PidFd(pin.as_fd()));
                match waitid(
                    target,
                    WaitIdOptions::EXITED
                        | WaitIdOptions::NOHANG
                        | WaitIdOptions::from_bits_retain(WAIT_ALL),
                ) {
                    Ok(Some(_)) | Err(_) => break,
                    _ => thread::sleep(Duration::from_millis(1)),
                }
            }
        }
    }
}

fn expect_event(pid: Pid, event: i32, deadline: Instant) -> io::Result<()> {
    if wait_event(pid, deadline)? != event {
        return Err(io::Error::other("unexpected startup process event"));
    }
    Ok(())
}

fn wait_event(pid: Pid, deadline: Instant) -> io::Result<i32> {
    loop {
        let options = WaitIdOptions::NOHANG | WaitIdOptions::from_bits_retain(WAIT_ALL);
        match waitid(
            WaitId::Pid(pid),
            options | WaitIdOptions::STOPPED | WaitIdOptions::EXITED | WaitIdOptions::NOWAIT,
        ) {
            Ok(Some(status)) if status.trapped() => {
                // Consume only a stop, not an exit that races this observation.
                if let Some(stop) = waitid(WaitId::Pid(pid), options | WaitIdOptions::STOPPED)? {
                    return stop
                        .trapping_signal()
                        .map(|signal| signal >> 8)
                        .ok_or_else(|| io::Error::other("unexpected startup process stop"));
                }
            }
            Ok(Some(_)) => return Err(io::Error::other("unexpected startup process event")),
            Ok(None) | Err(rustix::io::Errno::INTR) => {}
            Err(error) => return Err(error.into()),
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Agent identity observation timed out",
            ));
        }
        thread::sleep(Duration::from_millis(1));
    }
}

fn checked_pid(pid: u32) -> io::Result<Pid> {
    i32::try_from(pid)
        .ok()
        .and_then(Pid::from_raw)
        .ok_or_else(|| io::Error::other("invalid startup process ID"))
}

fn verify_credentials(pid: u32, uid: u32, gid: u32) -> io::Result<()> {
    let status = fs::read_to_string(format!("/proc/{pid}/status"))?;
    for (key, expected) in [("Uid:", uid), ("Gid:", gid)] {
        let line = status
            .lines()
            .find_map(|line| line.strip_prefix(key))
            .ok_or_else(|| io::Error::other("missing Agent credentials"))?;
        let actual = line
            .split_whitespace()
            .map(str::parse::<u32>)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| io::Error::other("invalid Agent credentials"))?;
        if actual != [expected; 4] {
            return Err(io::Error::other(
                "Agent credentials do not match assignment",
            ));
        }
    }
    if status
        .lines()
        .find_map(|line| line.strip_prefix("Groups:"))
        .is_none_or(|groups| !groups.trim().is_empty())
    {
        return Err(io::Error::other("Agent inherited supplementary groups"));
    }
    Ok(())
}

#[allow(
    unsafe_code,
    reason = "Linux ptrace has no stdlib/rustix binding; only owned startup tracees and scalar control operations are accepted (qbr.5.1.1.2 comment 1446)."
)]
unsafe extern "C" {
    fn ptrace(request: c_uint, pid: c_int, address: *mut c_void, ...) -> c_long;
}

#[allow(
    unsafe_code,
    reason = "Narrow Linux scalar ptrace controls; no tracee memory or register access (qbr.5.1.1.2 comment 1446)."
)]
fn operation(request: c_uint, pid: Pid, value: usize) -> io::Result<()> {
    // SAFETY: request is one of the private scalar-only control constants;
    // pid belongs to this thread's trace transaction. The null address is
    // required by these requests, and data is an integer, never dereferenced.
    let result = unsafe { ptrace(request, pid.as_raw_pid(), std::ptr::null_mut(), value) };
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[allow(
    unsafe_code,
    reason = "PTRACE_GETEVENTMSG writes one c_ulong to a live local variable; no tracee memory access (qbr.5.1.1.2 comment 1446)."
)]
fn event_pid(parent: Pid) -> io::Result<Pid> {
    let mut child: c_ulong = 0;
    // SAFETY: parent is stopped at the fork event on this tracing thread.
    // GETEVENTMSG writes exactly one c_ulong to the valid exclusive pointer,
    // synchronously without retaining it; address must be null.
    let result = unsafe {
        ptrace(
            GETEVENTMSG,
            parent.as_raw_pid(),
            std::ptr::null_mut(),
            &raw mut child,
        )
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    checked_pid(
        u32::try_from(child).map_err(|_| io::Error::other("invalid kernel fork process ID"))?,
    )
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "Process fixtures fail immediately on unavailable kernel prerequisites or incorrect observations."
)]
mod tests {
    use super::*;
    use std::{
        io::Write,
        os::unix::fs::MetadataExt,
        process::{Child, Command, Stdio},
    };

    struct Fixture(Child);

    impl Fixture {
        fn prepare() -> (Self, Trace, u32) {
            // This test-only shell holds our direct Child before execing the
            // real Bubblewrap. Production uses the descriptor bootstrap.
            let mut fixture = Self(Command::new("/bin/sh")
                .args(["-c", "read -r ready; exec /usr/bin/bwrap --unshare-all --die-with-parent --new-session --ro-bind / / --proc /proc --dev /dev --tmpfs /tmp --block-fd 0 -- /bin/sleep 30"])
                .stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::inherit())
                .spawn().expect("controlled bootstrap starts"));
            let mut trace = Trace::attach_child(fixture.0.id()).expect("owned Child attaches");
            fixture
                .0
                .stdin
                .as_mut()
                .unwrap()
                .write_all(b"initialize\n")
                .unwrap();
            let reaper = trace
                .prepare_reaper()
                .expect("kernel supplies stopped reaper");
            (fixture, trace, reaper)
        }

        fn release(&mut self) {
            self.0.stdin.as_mut().unwrap().write_all(b"x").unwrap();
        }

        fn wait(&mut self) {
            let deadline = Instant::now() + TIMEOUT;
            while self.0.try_wait().unwrap().is_none() {
                assert!(
                    Instant::now() < deadline,
                    "original monitor/reaper failed to collect workload"
                );
                thread::sleep(Duration::from_millis(1));
            }
        }
    }

    fn observe_unprivileged(
        mut trace: Trace,
        expected: &fs::File,
    ) -> io::Result<Arc<KernelProcess>> {
        let agent = trace.stop_at_exec()?;
        // An unprivileged test cannot clear host supplementary groups. Exercise
        // kernel creation/exec/lifetime proof here; production authenticate()
        // additionally requires all assigned IDs and an empty Groups list.
        trace.pin_executable(
            agent,
            expected,
            rustix::process::getuid().as_raw(),
            rustix::process::getgid().as_raw(),
        )
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn real_bubblewrap_authenticates_workload_and_preserves_reaper() {
        let (mut fixture, trace, reaper) = Fixture::prepare();
        fixture.release();
        let identity = observe_unprivileged(trace, &fs::File::open("/bin/sleep").unwrap())
            .expect("actual executable authenticates");
        assert_ne!(identity.credentials().pid, reaper);
        assert_ne!(identity.credentials().pid, fixture.0.id());
        assert!(identity.valid().unwrap());
        let pid = checked_pid(identity.credentials().pid).unwrap();
        let pin = pidfd_open(pid, PidfdFlags::empty()).unwrap();
        pidfd_send_signal(&pin, Signal::KILL).unwrap();
        fixture.wait();
        assert!(
            !identity.valid().unwrap(),
            "exit irreversibly revokes the original pin"
        );
        let error = KernelProcess::from_exec_stop(
            identity.credentials(),
            pin,
            &fs::File::open("/bin/sleep").unwrap(),
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "Agent process {} exited before executable observation",
                identity.credentials().pid
            )
        );
    }

    #[test]
    fn wrong_executable_is_denied_before_workload_execution_and_reaped() {
        let (mut fixture, mut trace, _) = Fixture::prepare();
        fixture.release();
        let agent = trace.stop_at_exec().unwrap();
        let expected_file = fs::File::open("/bin/cat").unwrap();
        let expected = expected_file.metadata().unwrap();
        let observed = fs::metadata("/bin/sleep").unwrap();
        let error = trace
            .pin_executable(
                agent,
                &expected_file,
                rustix::process::getuid().as_raw(),
                rustix::process::getgid().as_raw(),
            )
            .unwrap_err();
        fixture.wait();
        assert_eq!(
            error.to_string(),
            format!(
                "Agent executable changed for process {}: expected device {} inode {}, observed device {} inode {}",
                agent.as_raw_nonzero().get(),
                expected.dev(),
                expected.ino(),
                observed.dev(),
                observed.ino()
            )
        );
    }

    #[test]
    fn dropping_preparation_kills_and_reaps_the_owned_tracees() {
        let (mut fixture, trace, _) = Fixture::prepare();
        drop(trace);
        fixture.wait();
    }

    #[test]
    fn failed_startup_leaves_the_direct_child_exit_for_its_owner() {
        let mut fixture = Fixture(
            Command::new("/bin/sh")
                .args(["-c", "read -r ready; exit 0"])
                .stdin(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let trace = Trace::attach_child(fixture.0.id()).unwrap();
        fixture
            .0
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"exit\n")
            .unwrap();
        assert!(
            expect_event(
                trace.current,
                EVENT_FORK,
                Instant::now() + Duration::from_millis(50)
            )
            .is_err()
        );
        drop(trace);
        assert!(
            fixture.0.wait().is_ok(),
            "tracing must not steal Child's cleanup proof"
        );
    }

    #[test]
    fn missing_workload_fork_times_out_and_still_disposes_the_reaper() {
        let (mut fixture, trace, _) = Fixture::prepare();
        let error = expect_event(
            trace.current,
            EVENT_FORK,
            Instant::now() + Duration::from_millis(20),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        drop(trace);
        fixture.wait();
    }

    #[test]
    fn kernel_credential_check_rejects_the_wrong_assigned_uid_or_gid() {
        let uid = rustix::process::getuid().as_raw();
        let gid = rustix::process::getgid().as_raw();
        for (expected_uid, expected_gid) in [(uid + 1, gid), (uid, gid + 1)] {
            let error =
                verify_credentials(std::process::id(), expected_uid, expected_gid).unwrap_err();
            assert_eq!(
                error.to_string(),
                "Agent credentials do not match assignment"
            );
        }
    }
}
