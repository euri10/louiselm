//! Installing a signed release into the trusted prefix.
//!
//! Installation is content-addressed and atomic. Each release lands in its own
//! immutable directory, and the prefix's `current` symlink is replaced by a
//! rename — the one operation the kernel gives us that cannot be observed
//! half-done. There is no moment where the prefix holds part of one release
//! and part of another, and a failure anywhere before the rename leaves the
//! previous release exactly as it was.
//!
//! The caller supplies a bundle and a prefix; the caller does not get to
//! supply anything else. Component names come from a fixed allowlist, the
//! layout under the prefix is decided here, and modes are set here. An install
//! request cannot introduce a new command, a different policy path, or a
//! different owner.
//!
//! Ownership is reported, not asserted. This process cannot make a file
//! root-owned without being root, so [`status`] says what the bytes actually
//! are and names the next action; nothing claims a trust boundary that the
//! filesystem does not show.

use std::{fs, io, os::unix::fs::MetadataExt, path::Path};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    dossier::NextAction,
    release::{self, ReleaseError, ReleaseManifest},
    store::Store,
    trust::{TrustError, TrustStore},
};

/// The installed-state schema this build reads and writes.
pub const STATE_SCHEMA: &str = "louiselm.release.installed/1";

/// Where a trusted release is installed when nobody names a prefix.
///
/// Fixed on purpose: an install request that could choose its own prefix could
/// choose one the Agent can write.
pub const DEFAULT_PREFIX: &str = "/usr/local/lib/louiselm";

/// The status schema this build reports.
pub const STATUS_SCHEMA: &str = "louiselm.release.status/1";

/// What is installed in a prefix.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledState {
    /// Schema identifier.
    pub schema: String,
    /// Release currently pointed at by `current`.
    pub release_id: String,
    /// Build time of that release, used to refuse downgrades.
    pub built_at_ms: u64,
    /// When it was installed.
    pub installed_at_ms: u64,
    /// Source commit it was built from.
    pub source_commit: String,
    /// Policy version it carries.
    pub policy_version: String,
}

/// What the filesystem says about who owns the installed bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnershipEvidence {
    /// Whether every installed path is owned by uid 0.
    pub root_owned: bool,
    /// Whether an installed path has group/other write bits, excluding the
    /// validated `current` symlink whose mode does not confer write authority.
    pub world_writable: bool,
    /// Whether the invoking user can write the prefix.
    pub writable_by_invoker: bool,
    /// The uid that owns the prefix.
    pub prefix_uid: u32,
}

/// The full picture of one prefix.
#[derive(Clone, Debug, Serialize)]
pub struct InstallStatus {
    /// Schema identifier.
    pub schema: String,
    /// The prefix examined.
    pub prefix: String,
    /// What is installed, when anything is.
    pub installed: Option<InstalledState>,
    /// Component hashes of the current release.
    pub components: Vec<release::Component>,
    /// What the filesystem shows about ownership.
    pub ownership: OwnershipEvidence,
    /// Whether this prefix may be treated as a trust boundary.
    pub trusted: bool,
    /// Why it may not, when it may not.
    pub failure_code: Option<String>,
    /// The one safe thing to do next.
    pub next_action: NextAction,
}

/// An install that was refused.
#[derive(Debug, Error)]
pub enum InstallError {
    /// A filesystem operation failed.
    #[error("install I/O failed at '{path}': {source}")]
    Io {
        /// Path being operated on.
        path: String,
        /// Underlying failure.
        source: io::Error,
    },
    /// The bundle is not a usable release.
    #[error(transparent)]
    Release(#[from] ReleaseError),
    /// Trust is not usable.
    #[error(transparent)]
    Trust(#[from] TrustError),
    /// The installed state is unreadable.
    #[error("installed state is malformed: {0}")]
    Malformed(String),
    /// The bundle is older than what is installed.
    #[error("release {attempted} was built before the installed release {installed}")]
    Downgrade {
        /// Release someone tried to install.
        attempted: String,
        /// Release currently installed.
        installed: String,
    },
    /// The same release is already installed.
    #[error("release {0} is already current")]
    AlreadyCurrent(String),
}

/// Verifies `bundle` and installs it into `prefix`, atomically.
///
/// # Errors
/// Rejects missing trust, invalid bundles, already-current releases, or downgrades; propagates staging, permission, and installation-state I/O errors.
pub fn install(
    store: &Store,
    bundle: &Path,
    prefix: &Path,
    installed_at_ms: u64,
) -> Result<InstalledState, InstallError> {
    let trust = TrustStore::load(store)?.ok_or(TrustError::NotBootstrapped)?;
    let manifest = release::verify_bundle(bundle, &trust)?;

    if let Some(existing) = load_state(prefix)? {
        if existing.release_id == manifest.release_id {
            return Err(InstallError::AlreadyCurrent(manifest.release_id));
        }
        if manifest.built_at_ms <= existing.built_at_ms {
            return Err(InstallError::Downgrade {
                attempted: manifest.release_id,
                installed: existing.release_id,
            });
        }
    }

    let releases = prefix.join("releases");
    let destination = releases.join(&manifest.release_id);
    let staging = releases.join(format!(".staging-{}", manifest.release_id));
    if staging.exists() {
        remove(&staging)?;
    }
    release::create_dir(&staging)?;

    for component in &manifest.components {
        let from = bundle.join(&component.path);
        let to = staging.join(&component.path);
        if let Some(parent) = to.parent() {
            release::create_dir(parent)?;
        }
        release::copy(&from, &to)?;
        release::set_mode(&to, component.executable)?;
    }
    release::copy(
        &bundle.join("manifest.json"),
        &staging.join("manifest.json"),
    )?;
    release::copy(&bundle.join("manifest.sig"), &staging.join("manifest.sig"))?;
    release::set_mode(&staging.join("manifest.json"), false)?;
    release::set_mode(&staging.join("manifest.sig"), false)?;

    // Re-verify from the staged copy, not from the bundle: what is checked
    // must be what will be pointed at.
    release::verify_bundle(&staging, &trust)?;

    if destination.exists() {
        remove(&destination)?;
    }
    fs::rename(&staging, &destination).map_err(|source| InstallError::Io {
        path: destination.display().to_string(),
        source,
    })?;

    let target = Path::new("releases").join(&manifest.release_id);
    let pending = prefix.join(".current-pending");
    let _ = fs::remove_file(&pending);
    std::os::unix::fs::symlink(&target, &pending).map_err(|source| InstallError::Io {
        path: pending.display().to_string(),
        source,
    })?;
    fs::rename(&pending, prefix.join("current")).map_err(|source| InstallError::Io {
        path: prefix.join("current").display().to_string(),
        source,
    })?;

    let state = InstalledState {
        schema: STATE_SCHEMA.to_owned(),
        release_id: manifest.release_id.clone(),
        built_at_ms: manifest.built_at_ms,
        installed_at_ms,
        source_commit: manifest.source.commit.clone(),
        policy_version: manifest.policy.version.clone(),
    };
    let bytes =
        serde_json::to_vec(&state).map_err(|error| InstallError::Malformed(error.to_string()))?;
    release::write(&prefix.join("state.json"), &bytes)?;
    Ok(state)
}

/// Reports what is installed in `prefix` and whether it may be trusted.
///
/// # Errors
/// Returns installed-state read/JSON errors. Trust, ownership, or manifest problems are reported in the returned status.
pub fn status(prefix: &Path) -> Result<InstallStatus, InstallError> {
    let installed = load_state(prefix)?;
    let manifest = installed.as_ref().and_then(|state| {
        release::read_manifest(&prefix.join("releases").join(&state.release_id)).ok()
    });
    let ownership = ownership_of(prefix);

    let (failure_code, next_action) = if installed.is_none() {
        (
            Some("no_release_installed".to_owned()),
            NextAction {
                id: "install_release".to_owned(),
                detail: "No release is installed in this prefix; build, sign, and install one."
                    .to_owned(),
            },
        )
    } else if !ownership.root_owned {
        (
            Some("prefix_not_root_owned".to_owned()),
            NextAction {
                id: "install_as_root".to_owned(),
                detail: format!(
                    "The prefix is owned by uid {}; a prefix its own Agent can rewrite is not a trust boundary.",
                    ownership.prefix_uid
                ),
            },
        )
    } else if ownership.world_writable {
        (
            Some("prefix_world_writable".to_owned()),
            NextAction {
                id: "restrict_prefix_permissions".to_owned(),
                detail: "Installed paths are writable beyond root; tighten them before trusting this prefix."
                    .to_owned(),
            },
        )
    } else if let Some(mismatch) = manifest
        .as_ref()
        .and_then(|manifest| tampered_component(prefix, manifest))
    {
        (
            Some("release_tampered".to_owned()),
            NextAction {
                id: "reinstall_release".to_owned(),
                detail: format!(
                    "Installed component '{mismatch}' no longer matches the bytes its release binds; reinstall."
                ),
            },
        )
    } else if manifest.is_none() {
        (
            Some("release_manifest_unreadable".to_owned()),
            NextAction {
                id: "reinstall_release".to_owned(),
                detail: "The current release manifest is unreadable; reinstall the release."
                    .to_owned(),
            },
        )
    } else {
        (
            None,
            NextAction {
                id: "none".to_owned(),
                detail: "The installed release is complete and root-owned.".to_owned(),
            },
        )
    };

    Ok(InstallStatus {
        schema: STATUS_SCHEMA.to_owned(),
        prefix: prefix.display().to_string(),
        installed,
        components: manifest
            .map(|manifest| manifest.components)
            .unwrap_or_default(),
        trusted: failure_code.is_none(),
        ownership,
        failure_code,
        next_action,
    })
}

/// Reads the installed state, when a prefix has one.
///
/// # Errors
/// Returns read/JSON errors; an absent state file is `Ok(None)`.
pub fn load_state(prefix: &Path) -> Result<Option<InstalledState>, InstallError> {
    let path = prefix.join("state.json");
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(InstallError::Io {
                path: path.display().to_string(),
                source,
            });
        }
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| InstallError::Malformed(error.to_string()))
}

/// Returns the manifest of whatever release a prefix currently points at.
///
/// # Errors
/// Returns installed-state read/JSON errors. An absent state or unreadable manifest is `Ok(None)`.
pub fn current_manifest(prefix: &Path) -> Result<Option<ReleaseManifest>, InstallError> {
    let Some(state) = load_state(prefix)? else {
        return Ok(None);
    };
    Ok(release::read_manifest(&prefix.join("releases").join(&state.release_id)).ok())
}

/// Returns the first installed component whose bytes changed after install.
fn tampered_component(prefix: &Path, manifest: &ReleaseManifest) -> Option<String> {
    let root = prefix.join("releases").join(&manifest.release_id);
    manifest.components.iter().find_map(|component| {
        let path = root.join(&component.path);
        match release::hash(&path) {
            Ok((found, size)) if found.hex() == component.sha256 && size == component.size => None,
            _ => Some(component.name.clone()),
        }
    })
}

pub(crate) fn ownership_of(prefix: &Path) -> OwnershipEvidence {
    let mut world_writable = false;
    let mut prefix_uid = u32::MAX;
    let mut root_owned = false;

    if let Ok(metadata) = fs::metadata(prefix) {
        prefix_uid = metadata.uid();
        root_owned = metadata.uid() == 0;
        world_writable = metadata.mode() & 0o022 != 0;
    }
    let current = prefix.join("current");
    let current_is_valid = expected_current_link(prefix);
    walk(prefix, &mut |path, metadata| {
        if metadata.uid() != 0 {
            root_owned = false;
        }
        // Linux ordinary symlink modes are 0777, not write authority. Exempt
        // only our validated current link; its owner and every containing
        // directory/target remain checked. Never follow arbitrary links.
        if metadata.mode() & 0o022 != 0
            && !(metadata.is_symlink() && path == current && current_is_valid)
        {
            world_writable = true;
        }
    });

    OwnershipEvidence {
        root_owned,
        world_writable,
        writable_by_invoker: prefix_uid == rustix::process::geteuid().as_raw(),
        prefix_uid,
    }
}

fn expected_current_link(prefix: &Path) -> bool {
    let Ok(Some(state)) = load_state(prefix) else {
        return false;
    };
    if crate::Digest::parse(&state.release_id).is_err() {
        return false;
    }
    let target = Path::new("releases").join(state.release_id);
    fs::read_link(prefix.join("current")).is_ok_and(|found| found == target)
        && fs::symlink_metadata(prefix.join(target)).is_ok_and(|metadata| metadata.is_dir())
}

fn walk(path: &Path, visit: &mut impl FnMut(&Path, &fs::Metadata)) {
    let Ok(listing) = fs::read_dir(path) else {
        return;
    };
    for entry in listing.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        visit(&entry.path(), &metadata);
        if metadata.is_dir() {
            walk(&entry.path(), visit);
        }
    }
}

fn remove(path: &Path) -> Result<(), InstallError> {
    restore_writability(path);
    fs::remove_dir_all(path).map_err(|source| InstallError::Io {
        path: path.display().to_string(),
        source,
    })
}

fn restore_writability(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    if metadata.is_dir() {
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o755));
        if let Ok(listing) = fs::read_dir(path) {
            for entry in listing.flatten() {
                restore_writability(&entry.path());
            }
        }
    } else {
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o644));
    }
}
