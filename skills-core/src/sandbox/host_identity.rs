use std::{
    fs,
    io::{self, BufRead, BufReader, Read},
    os::{
        fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd},
        unix::{
            net::{UnixDatagram, UnixStream},
            process::CommandExt,
        },
    },
    process::Command,
    time::{Duration, Instant},
};

use rustix::io::{FdFlags, fcntl_dupfd_cloexec, fcntl_getfd, fcntl_setfd};
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
        let status_child = duplicate_for_child(&status_child)?;
        // A datagram socket has no EOF read. If the supervisor disappears
        // before sending the byte, closing its endpoint cannot accidentally
        // release Bubblewrap's `--block-fd` as a pipe or stream socket would.
        let (unblock, block_child) = UnixDatagram::pair()?;
        let block_child = duplicate_for_child(&block_child)?;
        Ok(Self {
            status,
            status_child: Some(status_child),
            unblock: Some(unblock),
            block_child: Some(block_child),
        })
    }

    pub fn status_fd(&self) -> RawFd {
        self.status_child
            .as_ref()
            .expect("the child status fd is present before spawn")
            .as_raw_fd()
    }

    pub fn block_fd(&self) -> RawFd {
        self.block_child
            .as_ref()
            .expect("the child block fd is present before spawn")
            .as_raw_fd()
    }

    /// Keeps only the two explicitly declared descriptors across `exec`.
    ///
    /// Changing the flags in the child avoids a parent-side window in which an
    /// unrelated concurrent spawn could inherit either descriptor.
    pub fn inherit_fds(&self, command: &mut Command) {
        inherit_fd(command, self.status_fd());
        inherit_fd(command, self.block_fd());
    }

    pub fn child_spawned(&mut self) {
        self.status_child = None;
        self.block_child = None;
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
        if child.parent_pid != monitor_pid {
            return Err(invalid(
                "Bubblewrap sandbox leader has an unexpected parent",
            ));
        }
        if child.namespace_pids.first() != Some(&pid) || child.namespace_pids.last() != Some(&1) {
            return Err(invalid(
                "Bubblewrap child-pid is not the host-view PID-namespace leader",
            ));
        }
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

    pub fn release(&mut self) -> io::Result<()> {
        self.unblock
            .as_ref()
            .expect("the startup gate is released once")
            .send(b"1")?;
        self.unblock = None;
        Ok(())
    }

    pub fn into_status(self) -> UnixStream {
        self.status
    }
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

fn duplicate_for_child(fd: &impl AsFd) -> io::Result<OwnedFd> {
    fcntl_dupfd_cloexec(fd, 3).map_err(io::Error::from)
}

fn inherit_fd(command: &mut Command, raw_fd: RawFd) {
    // SAFETY: `fcntl(F_GETFD/F_SETFD)` is async-signal-safe, the borrowed fd
    // remains open through `spawn`, and the hook performs no allocation.
    unsafe {
        command.pre_exec(move || {
            let fd = BorrowedFd::borrow_raw(raw_fd);
            let mut flags = fcntl_getfd(fd).map_err(io::Error::from)?;
            flags.remove(FdFlags::CLOEXEC);
            fcntl_setfd(fd, flags).map_err(io::Error::from)
        });
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
mod tests {
    use super::*;
    use std::io::Write;

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
