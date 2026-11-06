//! Read-only diagnostics for explicitly declared native instruction sources.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, Metadata},
    io::{self, Read},
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    path::{Path, PathBuf},
};

use rustix::fs::{Dir, Mode, OFlags};
use serde::Serialize;

use super::{ViewError, profile};
use crate::{Digest, registry::Registry};

const MAX_ENTRIES: usize = 4096;
const MAX_BYTES: u64 = 32 * 1024 * 1024;
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_DEPTH: usize = 32;

#[derive(Serialize)]
pub(crate) struct Drift {
    pub agent: String,
    pub path: PathBuf,
    pub sha256: Option<String>,
    pub reason: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct Status {
    pub drift: Vec<Drift>,
    pub undeclared: Vec<String>,
    pub complete: bool,
}

struct Scan {
    status: Status,
    seen: BTreeSet<(String, PathBuf)>,
    entries: usize,
    bytes: u64,
}

pub(crate) fn status(
    registry: &Registry,
    expected: Option<(&str, &[String])>,
) -> Result<Status, ViewError> {
    let mut scan = Scan {
        status: Status {
            drift: Vec::new(),
            undeclared: Vec::new(),
            complete: true,
        },
        seen: BTreeSet::new(),
        entries: 0,
        bytes: 0,
    };
    let mut ids = registry.agent_ids();
    ids.sort();
    for id in &ids {
        let agent = registry
            .agent(id)
            .map_err(|_| ViewError::Refused("registered_agent_missing"))?;
        if agent.native_instruction_paths.is_empty() {
            scan.status.undeclared.push(id.clone());
        }
        let expected = expected
            .filter(|(_, agents)| agents.contains(id))
            .map(|(digest, _)| digest);
        let mut paths: BTreeMap<_, _> = agent
            .native_instruction_paths
            .into_iter()
            .map(|path| (path, true))
            .collect();
        if let Some(path) = agent.always_on_path {
            paths.entry(path).or_insert(false);
        }
        for (path, recursive) in paths {
            if scan.exhausted() {
                scan.problem(id, &path, "scan_limit", None);
                break;
            }
            match profile::parent(&path, false) {
                Ok(parent) => scan.visit(&parent, &path, id, expected, 0, recursive),
                Err(ViewError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => (),
                Err(error) => scan.problem(id, &path, "unreadable", Some(error.to_string())),
            }
        }
    }
    scan.status
        .drift
        .sort_by(|a, b| (&a.agent, &a.path).cmp(&(&b.agent, &b.path)));
    Ok(scan.status)
}

pub(crate) fn human_status(status: &Status) -> String {
    use std::fmt::Write;
    let mut output = String::new();
    // Formatting into a String is infallible.
    for drift in &status.drift {
        // Debug quoting escapes controls in native filenames; never print bytes.
        let _ = writeln!(
            output,
            "native instructions {} \"{}\": {} {}",
            drift.agent,
            drift.path.to_string_lossy().escape_debug(),
            drift.reason,
            drift.sha256.as_deref().unwrap_or("digest unavailable")
        );
    }
    if !status.complete {
        output
            .push_str("native instruction scan incomplete; inspect unavailable paths and limits\n");
    }
    if !status.undeclared.is_empty() {
        let _ = writeln!(
            output,
            "native instruction locations undeclared: {}",
            status.undeclared.join(", ")
        );
    }
    output
}

impl Scan {
    fn exhausted(&self) -> bool {
        self.entries >= MAX_ENTRIES || self.bytes >= MAX_BYTES
    }

    fn problem(&mut self, agent: &str, path: &Path, reason: &'static str, error: Option<String>) {
        self.status.complete = false;
        self.status.drift.push(Drift {
            agent: agent.to_owned(),
            path: path.to_owned(),
            sha256: None,
            reason,
            error,
        });
    }

    fn visit(
        &mut self,
        directory: &File,
        path: &Path,
        agent: &str,
        expected: Option<&str>,
        depth: usize,
        recursive: bool,
    ) {
        self.entries += 1;
        if !self.seen.insert((agent.to_owned(), path.to_owned())) {
            return;
        }
        if depth > MAX_DEPTH {
            self.problem(agent, path, "depth_limit", None);
            return;
        }
        // Pin the inode without opening devices/FIFOs. A symlink stays a
        // symlink; neither it nor a concurrently substituted target is read.
        let result = (|| -> io::Result<()> {
            let name = path
                .file_name()
                .ok_or_else(|| io::Error::other("missing file name"))?;
            let handle = File::from(rustix::fs::openat(
                directory,
                name,
                OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )?);
            let before = handle.metadata()?;
            if before.is_dir() && recursive {
                let child = File::from(rustix::fs::openat(
                    &handle,
                    ".",
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )?);
                self.directory(&child, path, agent, expected, depth)?;
            } else if before.is_file() {
                self.file(&handle, &before, path, agent, expected)?;
            } else {
                self.problem(agent, path, "not_regular", None);
            }
            Ok(())
        })();
        if let Err(error) = result
            && error.kind() != io::ErrorKind::NotFound
        {
            self.problem(agent, path, "unreadable", Some(error.to_string()));
        }
    }

    fn directory(
        &mut self,
        directory: &File,
        path: &Path,
        agent: &str,
        expected: Option<&str>,
        depth: usize,
    ) -> io::Result<()> {
        // Bound enumeration too: do not collect/sort an unbounded directory.
        for entry in Dir::read_from(directory)? {
            let entry = entry?;
            let name = entry
                .file_name()
                .to_str()
                .map_err(|_| io::Error::other("non-UTF-8 native instruction name"))?;
            if matches!(name, "." | "..") {
                continue;
            }
            if self.exhausted() {
                self.problem(agent, path, "scan_limit", None);
                break;
            }
            self.visit(
                directory,
                &path.join(name),
                agent,
                expected,
                depth + 1,
                true,
            );
        }
        Ok(())
    }

    fn file(
        &mut self,
        handle: &File,
        before: &Metadata,
        path: &Path,
        agent: &str,
        expected: Option<&str>,
    ) -> io::Result<()> {
        if before.len() > MAX_FILE_BYTES {
            self.problem(agent, path, "file_limit", None);
            return Ok(());
        }
        let limit = MAX_FILE_BYTES.min(MAX_BYTES - self.bytes);
        if before.len() > limit {
            self.problem(agent, path, "scan_limit", None);
            self.bytes = MAX_BYTES;
            return Ok(());
        }
        // Reopen the validated regular inode, never the mutable source path.
        let mut file = File::open(format!("/proc/self/fd/{}", handle.as_raw_fd()))?;
        let mut bytes = Vec::new();
        let read = (&mut file).take(limit + 1).read_to_end(&mut bytes);
        self.bytes += bytes.len() as u64;
        read?;
        if bytes.len() as u64 > limit || !unchanged(before, &file.metadata()?) {
            self.problem(agent, path, "changed_during_read", None);
            return Ok(());
        }
        let digest = Digest::of(&bytes);
        if expected != Some(digest.hex()) {
            self.status.drift.push(Drift {
                agent: agent.to_owned(),
                path: path.to_owned(),
                sha256: Some(digest.to_string()),
                reason: "outside_profile",
                error: None,
            });
        }
        Ok(())
    }
}

fn unchanged(before: &Metadata, after: &Metadata) -> bool {
    before.len() == after.len()
        && before.mtime() == after.mtime()
        && before.mtime_nsec() == after.mtime_nsec()
        && before.ctime() == after.ctime()
        && before.ctime_nsec() == after.ctime_nsec()
}
