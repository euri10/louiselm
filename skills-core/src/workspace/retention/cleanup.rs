//! Fixed-root cleanup mechanics live here, separate from broker policy.

use super::{InputReferences, MAX_RECORD_BYTES, PrimaryEvidence, Store, invalid};
use crate::{
    launch::LaunchRequest,
    session_manifest::SessionInputManifest,
    workspace::{WorkspaceError, filesystem},
};
use rustix::fs::{AtFlags, Dir, Mode, OFlags, StatxFlags};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::Write,
    os::unix::fs::MetadataExt,
    path::Path,
    time::{Duration, Instant},
};

const MARKER: &str = "workspace-retention.json";
const MARKER_SCHEMA: &str = "louiselm.workspace.sealed/1";

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SealedStorage {
    schema: String,
    launch: LaunchRequest,
    inputs: InputReferences,
    pub(crate) disposed: bool,
}

impl SealedStorage {
    pub(crate) fn new(
        launch: &LaunchRequest,
        inputs: &SessionInputManifest,
    ) -> Result<Self, WorkspaceError> {
        let marker = Self {
            schema: MARKER_SCHEMA.into(),
            launch: launch.clone(),
            inputs: InputReferences::from_manifest(inputs)?,
            disposed: false,
        };
        marker.inputs.validate(launch)?;
        Ok(marker)
    }

    pub(crate) fn persist(&self, directory: &Path) -> Result<(), WorkspaceError> {
        let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
        temporary.write_all(&serde_json::to_vec(self)?)?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(directory.join(MARKER))
            .map_err(|e| e.error)?;
        File::open(directory)?.sync_all()?;
        Ok(())
    }
}

/// Payload-free bounded cleanup summary. Refusals retain uncertain storage.
#[derive(Debug, Default, Serialize)]
pub struct CleanupReport {
    /// Session trees whose primary content was removed (not securely erased).
    pub removed: usize,
    /// Live, undisposed, unexpired or explicitly pinned trees preserved.
    pub retained: usize,
    /// Missing/corrupt state or incomplete cleanup; a nonzero count is failure.
    pub failed: usize,
}

pub(super) fn system(owner: (u32, u32), now_ms: u64) -> Result<CleanupReport, WorkspaceError> {
    if !rustix::process::geteuid().is_root() {
        return Err(invalid());
    }
    let sessions = Path::new(crate::launch_supervisor::SYSTEM_SESSIONS_ROOT);
    let policy = Path::new("/var/lib/louiselm/broker/authorizations/workspace-retention");
    // No path comes from a record, argv, environment or Session process. Check
    // every ancestor so a replaced intermediate symlink cannot redirect root.
    for (path, uid) in [
        (Path::new("/var"), 0),
        (Path::new("/var/lib"), 0),
        (Path::new("/var/lib/louiselm"), 0),
        (Path::new("/var/lib/louiselm/broker"), owner.0),
        (
            Path::new("/var/lib/louiselm/broker/authorizations"),
            owner.0,
        ),
    ] {
        let meta = fs::symlink_metadata(path)?;
        if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o022 != 0 {
            return Err(invalid());
        }
    }
    sweep(sessions, policy, owner, 0, now_ms)
}

pub(crate) fn sweep(
    sessions: &Path,
    policy: &Path,
    broker: (u32, u32),
    root_uid: u32,
    now_ms: u64,
) -> Result<CleanupReport, WorkspaceError> {
    let parent = filesystem::open_directory(sessions)?;
    let meta = parent.metadata()?;
    if meta.uid() != root_uid || meta.mode() & 0o022 != 0 {
        return Err(invalid());
    }
    let mut store = Store::lock(policy, broker)?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut report = CleanupReport::default();
    let entries = fs::read_dir(sessions)?
        .take(10_001)
        .collect::<Result<Vec<_>, _>>()?;
    if entries.len() > 10_000 {
        return Err(invalid());
    }
    for entry in entries {
        let result = entry
            .file_name()
            .to_str()
            .ok_or_else(invalid)
            .and_then(|id| clean_one(&parent, id, &mut store, root_uid, now_ms, deadline));
        match result {
            Ok(true) => report.removed += 1,
            Ok(false) => report.retained += 1,
            Err(_) => report.failed += 1,
        }
    }
    Ok(report)
}

fn clean_one(
    parent: &File,
    id: &str,
    store: &mut Store,
    root_uid: u32,
    now_ms: u64,
    deadline: Instant,
) -> Result<bool, WorkspaceError> {
    let mut record = store.read(id)?;
    if record.primary_evidence == PrimaryEvidence::Removed {
        return Ok(false);
    }
    if record.pinned || now_ms < record.expires_at_ms {
        return Ok(false);
    }
    let root = open_dir(parent, id)?;
    let meta = root.metadata()?;
    if meta.uid() != root_uid {
        return Err(invalid());
    }
    if meta.mode() & 0o7777 == 0o711 {
        return Ok(false);
    }
    if meta.mode() & 0o7777 != 0o700 {
        return Err(invalid());
    }
    let marker_meta = rustix::fs::statat(&root, MARKER, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(std::io::Error::from)?;
    if marker_meta.st_uid != root_uid || marker_meta.st_mode & 0o7777 != 0o600 {
        return Err(invalid());
    }
    let bytes = filesystem::read_source(&root, MARKER, MAX_RECORD_BYTES)?
        .ok_or_else(invalid)?
        .bytes;
    let marker: SealedStorage = serde_json::from_slice(&bytes)?;
    marker.inputs.validate(&marker.launch)?;
    if marker.schema != MARKER_SCHEMA
        || marker.launch != record.launch
        || serde_json::to_vec(&marker)? != bytes
    {
        return Err(invalid());
    }
    if !marker.disposed {
        return Err(invalid());
    }
    if record
        .evidence
        .inputs
        .as_ref()
        .is_some_and(|inputs| inputs != &marker.inputs)
    {
        return Err(invalid());
    }
    let mount = mount_id(&root)?;
    // Preflight the entire sealed tree before removing any bytes. Links are
    // unlinked, never followed; nested mounts and unbounded trees are refused.
    walk(&root, mount, 0, &mut 0, deadline, false)?;
    record.evidence.inputs = Some(marker.inputs);
    record.primary_evidence = PrimaryEvidence::CleanupIncomplete;
    store.write(&record)?;
    walk(&root, mount, 0, &mut 0, deadline, true)?;
    root.sync_all()?;
    record.primary_evidence = PrimaryEvidence::Removed;
    store.write(&record)?;
    // Keep the tiny sealed launch marker as a tombstone. Reusing the Session
    // directory is forbidden even after its old host identity is recycled.
    Ok(true)
}

fn open_dir(parent: &File, name: impl rustix::path::Arg) -> Result<File, WorkspaceError> {
    Ok(rustix::fs::openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(std::io::Error::from)?
    .into())
}

fn mount_id(file: &File) -> Result<u64, WorkspaceError> {
    let stat = rustix::fs::statx(file, "", AtFlags::EMPTY_PATH, StatxFlags::MNT_ID)
        .map_err(std::io::Error::from)?;
    if stat.stx_mask & StatxFlags::MNT_ID.bits() == 0 {
        return Err(invalid());
    }
    Ok(stat.stx_mnt_id)
}

fn walk(
    root: &File,
    mount: u64,
    depth: usize,
    count: &mut usize,
    deadline: Instant,
    remove: bool,
) -> Result<(), WorkspaceError> {
    if depth > 64 || Instant::now() >= deadline || mount_id(root)? != mount {
        return Err(invalid());
    }
    for entry in Dir::read_from(root).map_err(std::io::Error::from)? {
        let entry = entry.map_err(std::io::Error::from)?;
        let name = entry.file_name();
        if name.to_bytes() == b"."
            || name.to_bytes() == b".."
            || (depth == 0 && name.to_bytes() == MARKER.as_bytes())
        {
            continue;
        }
        *count += 1;
        if *count > 100_000 || Instant::now() >= deadline {
            return Err(invalid());
        }
        let stat = rustix::fs::statx(
            root,
            name,
            AtFlags::SYMLINK_NOFOLLOW,
            StatxFlags::MNT_ID | StatxFlags::TYPE,
        )
        .map_err(std::io::Error::from)?;
        if stat.stx_mask & (StatxFlags::MNT_ID | StatxFlags::TYPE).bits()
            != (StatxFlags::MNT_ID | StatxFlags::TYPE).bits()
            || stat.stx_mnt_id != mount
        {
            return Err(invalid());
        }
        let directory = u32::from(stat.stx_mode) & 0o170_000 == 0o040_000;
        if directory {
            walk(
                &open_dir(root, name)?,
                mount,
                depth + 1,
                count,
                deadline,
                remove,
            )?;
        }
        if remove {
            #[cfg(test)]
            if FAIL_AFTER.with(|counter| {
                let left = counter.get();
                counter.set(left.saturating_sub(1));
                left == 0
            }) {
                return Err(std::io::Error::other("injected cleanup failure").into());
            }
            rustix::fs::unlinkat(
                root,
                name,
                if directory {
                    AtFlags::REMOVEDIR
                } else {
                    AtFlags::empty()
                },
            )
            .map_err(std::io::Error::from)?;
        }
    }
    if remove {
        root.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
std::thread_local! { static FAIL_AFTER: std::cell::Cell<usize> = const { std::cell::Cell::new(usize::MAX) }; }

#[cfg(test)]
mod tests;
