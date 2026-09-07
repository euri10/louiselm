use std::{
    fs,
    io::{self, BufRead, BufReader, Read},
    os::{
        fd::OwnedFd,
        unix::net::{UnixDatagram, UnixStream},
    },
    time::{Duration, Instant},
};

use rustix::process::{Pid, PidfdFlags, pidfd_open};
use serde::Deserialize;

use super::Cgroup;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_STATUS_BYTES: usize = 16 * 1024;
const MAX_STATUS_LINE_BYTES: usize = 4 * 1024;
const MAX_STATUS_OBJECTS: usize = 16;

#[derive(Debug)]
pub(super) struct Gate {
    status: UnixStream,
    status_child: Option<OwnedFd>,
    unblock: Option<UnixDatagram>,
    block_child: Option<OwnedFd>,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Observation {
    pub sandbox_leader_pid: u32,
    pub uid: u32,
    pub gid: u32,
    pub initial_user_namespace: bool,
}

#[derive(Deserialize)]
struct BubblewrapStatus {
    #[serde(rename = "child-pid")]
    child_pid: Option<u32>,
}

struct ProcessStatus {
    parent_pid: u32,
    uids: [u32; 4],
    gids: [u32; 4],
    groups: Vec<u32>,
    namespace_pids: Vec<u32>,
}

impl Gate {
    pub fn new() -> io::Result<Self> {
        let (status, status_child) = UnixStream::pair()?;
        // A datagram socket has no EOF read. If the supervisor disappears
        // before sending the byte, closing its endpoint cannot accidentally
        // release Bubblewrap's `--block-fd` as a pipe or stream socket would.
        let (unblock, block_child) = UnixDatagram::pair()?;
        Ok(Self {
            status,
            status_child: Some(status_child.into()),
            unblock: Some(unblock),
            block_child: Some(block_child.into()),
        })
    }

    /// Transfers the CLOEXEC endpoints once to the blocked bootstrap.
    #[expect(
        clippy::expect_used,
        reason = "The private prepare path transfers both owned descriptors exactly once."
    )]
    pub fn take_child_fds(&mut self) -> [OwnedFd; 2] {
        [
            self.status_child
                .take()
                .expect("status fd not yet transferred"),
            self.block_child
                .take()
                .expect("block fd not yet transferred"),
        ]
    }

    pub fn verify(
        &mut self,
        monitor_pid: u32,
        cgroup: &Cgroup,
        uid: u32,
        gid: u32,
    ) -> io::Result<Observation> {
        let deadline = Instant::now() + STARTUP_TIMEOUT;
        let pid = read_child_pid(&self.status, deadline)?;
        wait_for_cgroup(cgroup, monitor_pid, pid, deadline)?;

        let monitor = read_process_status(monitor_pid)?;
        verify_credentials("Bubblewrap monitor process", &monitor, uid, gid)?;
        let child = read_process_status(pid)?;
        verify_credentials("Bubblewrap sandbox leader", &child, uid, gid)?;
        verify_leader(&child, monitor_pid, pid)?;
        verify_id_map(pid, "uid_map", uid, deadline)?;
        verify_id_map(pid, "gid_map", gid, deadline)?;

        Ok(Observation {
            sandbox_leader_pid: pid,
            uid,
            gid,
            initial_user_namespace: read_id_map(std::process::id(), "uid_map")?
                == [[0, 0, u32::MAX]],
        })
    }

    pub fn pin_namespace_leader(&self, monitor_pid: u32) -> io::Result<(u32, OwnedFd)> {
        let pid = read_child_pid(&self.status, Instant::now() + STARTUP_TIMEOUT)?;
        let process = i32::try_from(pid)
            .ok()
            .and_then(Pid::from_raw)
            .ok_or_else(|| invalid("Bubblewrap reported an invalid child-pid"))?;
        let fd = pidfd_open(process, PidfdFlags::empty())?;
        verify_leader(&read_process_status(pid)?, monitor_pid, pid)?;
        // Pin before reading /proc, then require that pinned process to still
        // be alive. A recycled numeric PID cannot validate a dead pidfd.
        if !super::pidfd_events(&fd)?.is_empty() {
            return Err(invalid(
                "Bubblewrap namespace leader exited during observation",
            ));
        }
        Ok((pid, fd))
    }

    pub fn release(&mut self) -> io::Result<()> {
        self.unblock
            .as_ref()
            .ok_or_else(|| invalid("startup gate was already released"))?
            .send(b"1")?;
        self.unblock = None;
        Ok(())
    }

    pub fn into_status(self) -> UnixStream {
        self.status
    }
}

fn verify_leader(child: &ProcessStatus, monitor_pid: u32, pid: u32) -> io::Result<()> {
    if child.parent_pid != monitor_pid {
        return Err(invalid(
            "Bubblewrap sandbox leader has an unexpected parent",
        ));
    }
    if child.namespace_pids.len() < 2
        || child.namespace_pids.first() != Some(&pid)
        || child.namespace_pids.last() != Some(&1)
    {
        return Err(invalid(
            "Bubblewrap child-pid is not the host-view PID-namespace leader",
        ));
    }
    Ok(())
}

fn wait_for_cgroup(
    cgroup: &Cgroup,
    monitor_pid: u32,
    child_pid: u32,
    deadline: Instant,
) -> io::Result<()> {
    loop {
        let processes = cgroup.try_processes()?;
        if processes.contains(&monitor_pid) && processes.contains(&child_pid) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(invalid(format!(
                "Bubblewrap processes {monitor_pid}/{child_pid} were not both in the Session cgroup {processes:?}",
            )));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn read_child_pid(stream: &UnixStream, deadline: Instant) -> io::Result<u32> {
    let mut reader = BufReader::new(stream);
    let mut total = 0;
    for _ in 0..MAX_STATUS_OBJECTS {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::TimedOut, "Bubblewrap status timed out")
            })?;
        stream.set_read_timeout(Some(remaining))?;

        let remaining_bytes = MAX_STATUS_BYTES.saturating_sub(total);
        if remaining_bytes == 0 {
            return Err(invalid("Bubblewrap status exceeded its byte limit"));
        }
        let limit = remaining_bytes.min(MAX_STATUS_LINE_BYTES) + 1;
        let mut line = Vec::new();
        let read = (&mut reader)
            .take(limit as u64)
            .read_until(b'\n', &mut line)?;
        if read == 0 {
            return Err(invalid(
                "Bubblewrap closed status without reporting child-pid",
            ));
        }
        total += read;
        if read > MAX_STATUS_LINE_BYTES || total > MAX_STATUS_BYTES || !line.ends_with(b"\n") {
            return Err(invalid("Bubblewrap emitted an oversized status object"));
        }

        let status: BubblewrapStatus = serde_json::from_slice(&line)
            .map_err(|_| invalid("Bubblewrap emitted malformed JSON status"))?;
        if let Some(pid) = status.child_pid {
            if pid == 0 {
                return Err(invalid("Bubblewrap reported child-pid 0"));
            }
            stream.set_read_timeout(None)?;
            return Ok(pid);
        }
    }
    Err(invalid(
        "Bubblewrap did not report child-pid within the status object limit",
    ))
}

fn read_process_status(pid: u32) -> io::Result<ProcessStatus> {
    let path = format!("/proc/{pid}/status");
    let status = fs::read_to_string(&path)?;
    Ok(ProcessStatus {
        parent_pid: one_value(&status, "PPid:", pid)?,
        uids: four_values(&status, "Uid:", pid)?,
        gids: four_values(&status, "Gid:", pid)?,
        groups: values(&status, "Groups:", pid)?,
        namespace_pids: values(&status, "NSpid:", pid)?,
    })
}

fn verify_credentials(subject: &str, status: &ProcessStatus, uid: u32, gid: u32) -> io::Result<()> {
    if !status.uids.iter().all(|&found| found == uid)
        || !status.gids.iter().all(|&found| found == gid)
        || !status.groups.is_empty()
    {
        return Err(invalid(format!(
            "{subject} did not adopt the assigned host uid/gid with empty supplementary groups",
        )));
    }
    Ok(())
}

fn verify_id_map(pid: u32, name: &str, id: u32, deadline: Instant) -> io::Result<()> {
    loop {
        let rows = read_id_map(pid, name)?;
        if rows == [[id, id, 1]] {
            return Ok(());
        }
        let transitional = rows.is_empty() || rows == [[0, id, 1]];
        if !transitional || Instant::now() >= deadline {
            return Err(invalid(format!(
                "process {pid} did not reach the assigned namespace-to-host mapping in {name}: {rows:?}",
            )));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn read_id_map(pid: u32, name: &str) -> io::Result<Vec<[u32; 3]>> {
    fs::read_to_string(format!("/proc/{pid}/{name}"))?
        .lines()
        .map(|line| {
            let values = line
                .split_whitespace()
                .map(|value| {
                    value
                        .parse::<u32>()
                        .map_err(|_| invalid(format!("process {pid} has malformed {name}")))
                })
                .collect::<io::Result<Vec<_>>>()?;
            values
                .try_into()
                .map_err(|_| invalid(format!("process {pid} has malformed {name}")))
        })
        .collect()
}

fn one_value(status: &str, field: &str, pid: u32) -> io::Result<u32> {
    let values = values(status, field, pid)?;
    match values.as_slice() {
        [value] => Ok(*value),
        _ => Err(invalid(format!("process {pid} has malformed {field}"))),
    }
}

fn four_values(status: &str, field: &str, pid: u32) -> io::Result<[u32; 4]> {
    values(status, field, pid)?
        .try_into()
        .map_err(|_| invalid(format!("process {pid} has malformed {field}")))
}

fn values(status: &str, field: &str, pid: u32) -> io::Result<Vec<u32>> {
    status
        .lines()
        .find_map(|line| line.strip_prefix(field))
        .ok_or_else(|| invalid(format!("process {pid} status omits {field}")))?
        .split_whitespace()
        .map(|value| {
            value
                .parse()
                .map_err(|_| invalid(format!("process {pid} has malformed {field}")))
        })
        .collect()
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn transferred_descriptors_outlive_gate() {
        use std::os::fd::AsRawFd;

        let mut gate = Gate::new().expect("startup gate opens");
        let [status, block] = gate.take_child_fds();
        let status_path = format!("/proc/self/fd/{}", status.as_raw_fd());
        let block_path = format!("/proc/self/fd/{}", block.as_raw_fd());
        let status_target = fs::read_link(&status_path).expect("status descriptor is open");
        let block_target = fs::read_link(&block_path).expect("block descriptor is open");
        drop(gate);

        assert_eq!(fs::read_link(status_path).ok(), Some(status_target));
        assert_eq!(fs::read_link(block_path).ok(), Some(block_target));
    }

    #[test]
    fn dropping_transferred_descriptors_closes_status_endpoint() {
        let mut gate = Gate::new().expect("startup gate opens");
        gate.status
            .set_read_timeout(Some(Duration::from_secs(1)))
            .expect("timeout set");
        drop(gate.take_child_fds());
        let mut byte = [0];
        assert_eq!(gate.status.read(&mut byte).expect("status reaches EOF"), 0);
    }

    #[test]
    fn status_reader_ignores_unknown_objects_and_fields() {
        let (reader, mut writer) = UnixStream::pair().expect("status socket pair opens");
        writer
            .write_all(b"{\"future-object\":true}\n{\"child-pid\":42,\"future-field\":true}\n")
            .expect("status writes");

        let pid = read_child_pid(&reader, Instant::now() + Duration::from_secs(1))
            .expect("known child-pid is accepted");

        assert_eq!(pid, 42);
    }

    #[test]
    fn status_reader_rejects_malformed_json() {
        let (reader, mut writer) = UnixStream::pair().expect("status socket pair opens");
        writer.write_all(b"not-json\n").expect("status writes");

        let error = read_child_pid(&reader, Instant::now() + Duration::from_secs(1))
            .expect_err("malformed status is refused");

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn pinning_rejects_an_ordinary_child_without_signalling_it() {
        let mut gate = Gate::new().expect("startup gate opens");
        let [status, _block] = gate.take_child_fds();
        let mut writer = UnixStream::from(status);
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("ordinary child starts");
        writeln!(writer, "{{\"child-pid\":{}}}", child.id()).expect("status writes");

        let result = gate.pin_namespace_leader(std::process::id());
        let still_running = child.try_wait().expect("child status reads").is_none();
        child.kill().expect("fixture child is killed");
        child.wait().expect("fixture child is reaped");

        let error = result.expect_err("a child must also be PID 1 in a nested namespace");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(still_running, "invalid status must never authorize a kill");
    }

    #[test]
    fn pinning_rejects_a_pid_outside_the_kernel_range() {
        let mut gate = Gate::new().expect("startup gate opens");
        let [status, _block] = gate.take_child_fds();
        let mut writer = UnixStream::from(status);
        writeln!(writer, "{{\"child-pid\":{}}}", u32::MAX).expect("status writes");

        let error = gate
            .pin_namespace_leader(std::process::id())
            .expect_err("unsigned overflow must not select another process");

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn closing_the_datagram_gate_does_not_release_its_reader() {
        let (sender, receiver) = UnixDatagram::pair().expect("gate socket pair opens");
        receiver
            .set_read_timeout(Some(Duration::from_millis(50)))
            .expect("gate timeout is set");
        drop(sender);
        let mut byte = [0];

        let error = receiver
            .recv(&mut byte)
            .expect_err("a closed datagram peer produces no EOF byte");

        assert!(matches!(
            error.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
        ));
    }
}
