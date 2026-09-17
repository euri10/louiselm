//! Upstream-produced read generations, isolated from canonical mutation storage.

use super::tracker_runner::{SystemTrackerRunner, TrackerInvocation, TrackerRunner};
use super::{BrokerError, BrokerService, beads_mutation::TrackerConfig};
use crate::{
    beads_mutation::{BeadsMutationOutcome, BeadsMutationStatus},
    beads_replica::{self, Files},
    launch_protocol::LaunchAuthorization,
    workspace::filesystem,
};
use std::{
    fs::{self, File},
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::{Path, PathBuf},
    time::Duration,
};

pub(super) struct Paths {
    inputs: PathBuf,
    sessions: PathBuf,
    uid: u32,
    gid: u32,
}

/// Private staging is owned by the handshake, not by the launched Session.
pub(super) struct Input(PathBuf);

impl Drop for Input {
    fn drop(&mut self) {
        // A failed cleanup leaves only broker-private disposable input bytes.
        // Installed startup removes these before accepting another handshake.
        let _ = fs::remove_dir_all(&self.0);
    }
}

impl BrokerService {
    pub(super) fn enable_beads_replicas(
        &mut self,
        sessions: &Path,
        uid: u32,
        gid: u32,
    ) -> Result<(), BrokerError> {
        if self.tracker.is_none() {
            return Ok(());
        }
        let inputs = self
            .verification_inputs
            .parent()
            .ok_or(BrokerError::InvalidGrant)?
            .join("beads-inputs");
        match fs::DirBuilder::new().mode(0o700).create(&inputs) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(BrokerError::Storage(error)),
        }
        check_directory(&inputs, uid, gid, 0o700)?;
        // The installed exclusive state lock excludes another broker. Running
        // Sessions own separate copies; no live generation depends on staging.
        for entry in fs::read_dir(&inputs).map_err(BrokerError::Storage)? {
            let path = entry.map_err(BrokerError::Storage)?.path();
            check_directory(&path, uid, gid, 0o700)?;
            fs::remove_dir_all(path).map_err(BrokerError::Storage)?;
        }
        super::sync_directory(&inputs)?;
        self.beads_replicas = Some(Paths {
            inputs,
            sessions: sessions.into(),
            uid,
            gid,
        });
        Ok(())
    }

    pub(super) fn stage_beads_replica(
        &self,
        authorization: &LaunchAuthorization,
    ) -> Result<Option<Input>, BrokerError> {
        let Some(paths) = &self.beads_replicas else {
            return Ok(None);
        };
        check_directory(&paths.inputs, paths.uid, paths.gid, 0o700)?;
        let pending = self
            .authorizations()
            .consumed_for_session(&authorization.session_id)?
            .ok_or(BrokerError::UnknownAuthorization)?;
        let Some(permission) = &pending.beads_mutations else {
            return Ok(None);
        };
        let tracker = self.tracker.as_ref().ok_or(BrokerError::InvalidGrant)?;
        if permission.project_digest != tracker.project_digest() {
            return Err(BrokerError::InvalidGrant);
        }
        let files = export(
            tracker,
            &format!("{}/{}", pending.agent_id, pending.session_id),
        )?;
        let input = paths.inputs.join(&authorization.session_id);
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&input)
            .map_err(BrokerError::Storage)?;
        let guard = Input(input);
        let record = crate::beads_replica::InputBinding {
            request_digest: authorization.request_digest.clone(),
            assigned_uid: authorization.assigned_uid,
            assigned_gid: authorization.assigned_gid,
        };
        filesystem::write_file(
            &guard.0.join("binding.json"),
            &serde_json::to_vec(&record).map_err(|_| BrokerError::InvalidGrant)?,
            0o600,
        )?;
        write_private(&guard.0.join("files"), &files)?;
        super::sync_directory(&guard.0)?;
        Ok(Some(guard))
    }

    pub(super) fn refresh_beads_replica(
        &self,
        authorization: &LaunchAuthorization,
        status: &BeadsMutationStatus,
    ) -> Result<(), BrokerError> {
        let Some(paths) = &self.beads_replicas else {
            return Ok(());
        };
        if status.outcome != BeadsMutationOutcome::Completed {
            return Ok(());
        }
        let session = paths.sessions.join(&authorization.session_id);
        check_directory(&paths.sessions, 0, 0, 0o711)?;
        check_directory(&session, 0, 0, 0o711)?;
        let root = session.join(beads_replica::DIRECTORY);
        check_directory(&root, paths.uid, authorization.assigned_gid, 0o2750)?;
        let generation = &status.operation_id;
        if beads_replica::completed(&root, generation)? {
            return Ok(());
        }
        let pending = self
            .authorizations()
            .consumed_for_session(&authorization.session_id)?
            .ok_or(BrokerError::UnknownAuthorization)?;
        let tracker = self.tracker.as_ref().ok_or(BrokerError::InvalidGrant)?;
        let files = export(
            tracker,
            &format!("{}/{}", pending.agent_id, pending.session_id),
        )?;
        let incomplete = root.join(generation);
        match fs::symlink_metadata(&incomplete) {
            Ok(meta) if meta.is_dir() && meta.uid() == paths.uid => {
                // No durable ready marker exists. remove_dir_all does not
                // follow links planted inside this disposable generation.
                fs::remove_dir_all(&incomplete).map_err(BrokerError::Storage)?;
            }
            Ok(_) => return Err(BrokerError::InvalidGrant),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(BrokerError::Storage(error)),
        }
        beads_replica::publish(&root, generation, &files)?;
        Ok(())
    }
}

fn check_directory(path: &Path, uid: u32, gid: u32, mode: u32) -> Result<(), BrokerError> {
    // Session parents deliberately grant traversal, not directory listing, to
    // the broker. O_PATH validates their inode without requiring read access.
    let directory = rustix::fs::open(
        path,
        rustix::fs::OFlags::PATH | rustix::fs::OFlags::DIRECTORY | rustix::fs::OFlags::NOFOLLOW,
        rustix::fs::Mode::empty(),
    )
    .map(File::from)
    .map_err(|error| BrokerError::Storage(error.into()))?;
    let meta = directory.metadata().map_err(BrokerError::Storage)?;
    if meta.uid() != uid || meta.gid() != gid || meta.mode() & 0o7777 != mode {
        return Err(BrokerError::InvalidGrant);
    }
    Ok(())
}

fn write_private(root: &Path, files: &Files) -> Result<(), crate::workspace::WorkspaceError> {
    fs::DirBuilder::new().mode(0o700).create(root)?;
    for (name, bytes) in files {
        let path = root.join(name);
        fs::create_dir_all(
            path.parent()
                .ok_or(crate::workspace::WorkspaceError::Invalid(
                    "missing replica parent",
                ))?,
        )?;
        filesystem::write_file(&path, bytes, 0o600)?;
    }
    File::open(root)?.sync_all()?;
    Ok(())
}

fn export(tracker: &TrackerConfig, actor: &str) -> Result<Files, BrokerError> {
    let staging = tempfile::tempdir_in(&tracker.scratch).map_err(BrokerError::Storage)?;
    let directory = staging.path().join(".beads");
    fs::create_dir(&directory).map_err(BrokerError::Storage)?;
    let root = directory.as_path();
    // Mutation (30s) plus both replica processes fit the relay's 60s deadline.
    let runner = SystemTrackerRunner::new(Duration::from_secs(10));
    let invocation = TrackerInvocation {
        program: tracker.program.clone(),
        program_digest: tracker.program_digest.clone(),
        arguments: vec![
            "sync".into(),
            "--flush-only".into(),
            "--allow-external-jsonl".into(),
            "--no-auto-import".into(),
            "--no-auto-flush".into(),
            "--db".into(),
            tracker
                .workspace_root
                .join(".beads/beads.db")
                .into_os_string(),
            "--actor".into(),
            actor.into(),
            "--json".into(),
        ],
        environment: vec![
            ("TMPDIR".into(), staging.path().as_os_str().to_owned()),
            (
                "BEADS_JSONL".into(),
                root.join("issues.jsonl").into_os_string(),
            ),
        ],
        current_dir: tracker.workspace_root.clone(),
    };
    if runner.run(&invocation)?.exit_code != Some(0) || !root.join("issues.jsonl").is_file() {
        return Err(BrokerError::TrackerConfiguration(
            "cannot export canonical tracker",
        ));
    }
    let canonical = filesystem::open_directory(&tracker.workspace_root.join(".beads"))?;
    if let Some(policy) = filesystem::read_source(&canonical, "policy.yaml", 1024 * 1024)? {
        filesystem::write_file(&root.join("policy.yaml"), &policy.bytes, 0o600)?;
    }
    filesystem::write_file(
        &root.join("metadata.json"),
        br#"{"database":"beads.db","jsonl_export":"issues.jsonl"}"#,
        0o600,
    )?;
    let invocation = TrackerInvocation {
        arguments: vec![
            "list".into(),
            "--limit".into(),
            "1".into(),
            "--json".into(),
            "--actor".into(),
            actor.into(),
        ],
        environment: vec![
            ("TMPDIR".into(), staging.path().as_os_str().to_owned()),
            ("BEADS_DIR".into(), root.as_os_str().to_owned()),
        ],
        current_dir: root.into(),
        ..invocation
    };
    if runner.run(&invocation)?.exit_code != Some(0) || !root.join("beads.db").is_file() {
        return Err(BrokerError::TrackerConfiguration(
            "cannot build local tracker replica",
        ));
    }
    // The pinned upstream process and its descendants have exited. Its lock
    // files carry the broker UID, not data, and must be created anew by the
    // Session. Preserve the database, WAL, journal and durability certificates.
    let mut files = beads_replica::read_generated(root)?;
    files.retain(|name, _| {
        name.contains('/')
            || !(Path::new(name).extension().is_some_and(|ext| ext == "lock")
                || matches!(
                    name.as_str(),
                    "beads.db-fsqlite-ns-gate" | "beads.db-fsqlite-ns-use"
                ))
    });
    Ok(files
        .into_iter()
        .map(|(name, bytes)| (format!(".beads/{name}"), bytes))
        .collect())
}

#[cfg(test)]
#[path = "beads_replica_tests.rs"]
mod tests;
