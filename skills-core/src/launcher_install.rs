//! Installation and inspection of the privileged launcher authority.
//!
//! The launcher is deliberately boring: one fixed release component, one
//! root-owned software keyring, one bounded pool of host identities, and one
//! exact sudo command.  This module provisions those bytes, but it does not
//! implement the launcher process itself.

use std::{
    collections::HashSet,
    ffi::OsString,
    fs::{self, File, OpenOptions, TryLockError},
    io::{self, Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    Digest,
    install::{self, STATE_SCHEMA as RELEASE_STATE_SCHEMA},
    launch_receipt::{RECEIPT_SCHEMA, ReceiptPayload},
    release::{MANIFEST_SCHEMA, ReleaseManifest},
    sshsig,
};

mod identity;

pub use identity::{Identity, IdentityLease, IdentityPool};

/// Schema for the launcher configuration held by root.
pub const CONFIG_SCHEMA: &str = "louiselm.launch.config/1";
/// Schema for the public launcher verification keyring.
pub const KEYRING_SCHEMA: &str = "louiselm.launch.keyring/1";
/// Schema for launcher installation diagnostics.
pub const STATUS_SCHEMA: &str = "louiselm.launch.install.status/1";

const LAUNCHER_COMPONENT: &str = "louiselm-launch";
const LAUNCHER_RELATIVE_PATH: &str = "bin/louiselm-launch";
const SUDO_PATH: &str = "/usr/bin/sudo";
const PENDING_ROTATION_SCHEMA: &str = "louiselm.launch.rotation.pending/1";
const MAX_STATE_BYTES: u64 = 4 * 1024 * 1024;
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Every path the root installer is allowed to touch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LauncherPaths {
    /// Root of the installed release store.
    pub release_prefix: PathBuf,
    /// Persistent launcher state.
    pub state_root: PathBuf,
    /// Dedicated sudoers fragment.
    pub sudoers: PathBuf,
    /// Subordinate UID reservation ledger.
    pub subuid: PathBuf,
    /// Subordinate GID reservation ledger.
    pub subgid: PathBuf,
    /// Local account database used to reject host-ID collisions.
    pub passwd: PathBuf,
    /// Local group database used to reject host-ID collisions.
    pub group: PathBuf,
    /// Name-service configuration used to establish sub-ID authority.
    pub nsswitch: PathBuf,
    /// Absolute measured OpenSSH key utility.
    pub ssh_keygen: PathBuf,
    /// Absolute measured effective identity resolver.
    pub getent: PathBuf,
    /// Absolute sudoers validator.
    pub visudo: PathBuf,
}

impl LauncherPaths {
    /// Returns the fixed production paths.
    pub fn system() -> Self {
        let release_prefix = PathBuf::from(install::DEFAULT_PREFIX);
        Self {
            state_root: release_prefix.join("launcher"),
            release_prefix,
            sudoers: PathBuf::from("/etc/sudoers.d/louiselm-launch"),
            subuid: PathBuf::from("/etc/subuid"),
            subgid: PathBuf::from("/etc/subgid"),
            passwd: PathBuf::from("/etc/passwd"),
            group: PathBuf::from("/etc/group"),
            nsswitch: PathBuf::from("/etc/nsswitch.conf"),
            ssh_keygen: PathBuf::from("/usr/bin/ssh-keygen"),
            getent: PathBuf::from("/usr/bin/getent"),
            visudo: PathBuf::from("/usr/sbin/visudo"),
        }
    }

    fn launcher(&self) -> PathBuf {
        self.release_prefix
            .join("current")
            .join(LAUNCHER_RELATIVE_PATH)
    }

    fn config(&self) -> PathBuf {
        self.state_root.join("config.json")
    }

    fn keyring(&self) -> PathBuf {
        self.state_root.join("keyring.json")
    }

    fn private(&self) -> PathBuf {
        self.state_root.join("private")
    }

    fn keys(&self) -> PathBuf {
        self.private().join("keys")
    }

    fn scratch(&self) -> PathBuf {
        self.private().join("scratch")
    }

    fn locks(&self) -> PathBuf {
        self.state_root.join("locks")
    }

    fn pending_rotation(&self) -> PathBuf {
        self.private().join("pending-rotation.json")
    }

    fn pending_key(&self) -> PathBuf {
        self.private().join("pending-key")
    }
}

/// Immutable install choices supplied by the maintainer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallRequest {
    /// Existing unprivileged account permitted to invoke the launcher.
    pub operator: String,
    /// Host identities reserved for Sessions.
    pub pool: IdentityPool,
}

/// Root-owned state consumed by the launcher.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LauncherConfig {
    /// Schema identifier.
    pub schema: String,
    /// Account allowed by sudoers.
    pub operator: String,
    /// Numeric identity pinned in sudoers so a name reassignment cannot widen authority.
    pub operator_uid: u32,
    /// Installed release identity.
    pub release_id: String,
    /// Digest of the launcher component bytes.
    pub launcher_digest: String,
    /// Fixed path passed to sudo.
    pub launcher_path: PathBuf,
    /// Measured absolute signing tool path.
    pub ssh_keygen_path: PathBuf,
    /// Digest of the measured signing tool bytes.
    pub ssh_keygen_digest: String,
    /// Measured absolute effective identity resolver.
    pub getent_path: PathBuf,
    /// Digest of the measured identity resolver bytes.
    pub getent_digest: String,
    /// Installed identity pool.
    pub pool: IdentityPool,
}

/// One launcher signing key visible to unprivileged verifiers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LauncherKey {
    /// Stable digest of the normalized public key.
    pub key_id: String,
    /// Public key in normalized authorized-keys form.
    pub public_key: String,
    /// Creation time recorded by the installer.
    pub created_at_ms: u64,
    /// Retirement time, absent only for the active key.
    pub retired_at_ms: Option<u64>,
    /// Idempotency identity of the rotation that created this key.
    pub rotation_id: Option<String>,
    /// Key replaced by that rotation.
    pub replaces: Option<String>,
}

/// Public keys accepted for launcher receipt chains.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicKeyring {
    /// Schema identifier.
    pub schema: String,
    /// Key used for new chains.
    pub active_key_id: String,
    /// Active and retained keys, in creation order.
    pub keys: Vec<LauncherKey>,
}

impl PublicKeyring {
    /// Looks up a public key by stable ID.
    pub fn key(&self, key_id: &str) -> Option<&LauncherKey> {
        self.keys.iter().find(|key| key.key_id == key_id)
    }

    /// Returns every retired key ID in creation order.
    pub fn retained_key_ids(&self) -> Vec<String> {
        self.keys
            .iter()
            .filter(|key| key.key_id != self.active_key_id)
            .map(|key| key.key_id.clone())
            .collect()
    }
}

/// Explicit, replayable key-rotation request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RotationRequest {
    /// Stable maintainer-chosen idempotency identity.
    pub rotation_id: String,
    /// Compare-and-swap expectation for the active key.
    pub expected_active_key_id: String,
}

/// Result of a key rotation or exact replay.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RotationOutcome {
    /// Key produced by this rotation identity.
    pub key_id: String,
    /// Active key after the operation.
    pub active_key_id: String,
    /// Whether this call created the key rather than replaying a result.
    pub created: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingRotation {
    schema: String,
    rotation_id: String,
    expected_active_key_id: String,
    key_id: Option<String>,
    public_key: Option<String>,
    created_at_ms: u64,
}

/// One actionable status failure.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LauncherFailure {
    /// Stable machine-readable code.
    pub code: String,
    /// Sanitized explanation.
    pub detail: String,
    /// Concrete recovery action.
    pub next_action: String,
}

/// Complete public launcher installation status.
#[derive(Clone, Debug, Serialize)]
pub struct LauncherStatus {
    /// Schema identifier.
    pub schema: String,
    /// Whether every checked trust invariant holds.
    pub trusted: bool,
    /// Parsed configuration, when readable.
    pub config: Option<LauncherConfig>,
    /// Key used for new receipt chains.
    pub active_key_id: Option<String>,
    /// Keys retained for live chains.
    pub retained_key_ids: Vec<String>,
    /// Slots currently held by a supervisor.
    pub occupied_slots: Vec<u32>,
    /// Every observed failure.
    pub failures: Vec<LauncherFailure>,
}

/// One process invocation, explicit enough to inspect in tests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandInvocation {
    /// Absolute executable path.
    pub program: PathBuf,
    /// Argument vector without shell interpretation.
    pub arguments: Vec<OsString>,
    /// Bytes written to standard input.
    pub stdin: Vec<u8>,
    /// Explicit working directory, when needed.
    pub current_dir: Option<PathBuf>,
}

/// Captured result of a command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandOutput {
    /// Whether the process exited successfully.
    pub success: bool,
    /// Numeric exit status, when the process exited normally.
    pub exit_code: Option<i32>,
    /// Captured standard output.
    pub stdout: Vec<u8>,
    /// Captured standard error.
    pub stderr: Vec<u8>,
}

impl CommandOutput {
    /// Constructs a successful empty result, primarily for deterministic tests.
    pub fn success() -> Self {
        Self {
            success: true,
            exit_code: Some(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        }
    }

    /// Constructs a failed result with a diagnostic.
    pub fn failure(reason: &str) -> Self {
        Self {
            success: false,
            exit_code: Some(1),
            stdout: Vec::new(),
            stderr: reason.as_bytes().to_vec(),
        }
    }
}

/// Narrow process seam used only for key generation, signing, and validation.
pub trait CommandRunner {
    /// Runs one fixed-structure invocation.
    fn run(&self, invocation: &CommandInvocation) -> io::Result<CommandOutput>;
}

/// Production command runner with no inherited environment or shell.
pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn run(&self, invocation: &CommandInvocation) -> io::Result<CommandOutput> {
        let mut child = Command::new(&invocation.program);
        child
            .args(&invocation.arguments)
            .env_clear()
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(current_dir) = &invocation.current_dir {
            child.current_dir(current_dir);
        }
        if invocation.stdin.is_empty() {
            child.stdin(Stdio::null());
        } else {
            child.stdin(Stdio::piped());
        }
        let mut child = child.spawn()?;
        if !invocation.stdin.is_empty() {
            child
                .stdin
                .take()
                .expect("piped stdin is present")
                .write_all(&invocation.stdin)?;
        }
        let output = child.wait_with_output()?;
        Ok(CommandOutput {
            success: output.status.success(),
            exit_code: output.status.code(),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

/// A launcher installation or lease operation that was refused.
#[derive(Debug, Error)]
pub enum LauncherError {
    /// A filesystem operation failed.
    #[error("launcher I/O failed at '{path}': {source}")]
    Io {
        /// Public path or operation label.
        path: String,
        /// Underlying failure.
        source: io::Error,
    },
    /// Persisted or external bytes were malformed.
    #[error("launcher state is malformed: {0}")]
    Malformed(String),
    /// A requested boundary would be unsafe.
    #[error("launcher request is unsafe: {0}")]
    Invalid(String),
    /// A fixed external tool failed.
    #[error("{tool} failed: {reason}")]
    Tool {
        /// Tool name.
        tool: &'static str,
        /// Sanitized diagnostic.
        reason: String,
    },
    /// A lease is already held.
    #[error("identity slot {slot} is occupied")]
    Occupied {
        /// Contended slot.
        slot: u32,
    },
    /// The global install/rotation lock is already held.
    #[error("another launcher installation is in progress")]
    InstallBusy,
    /// A shadow-utils subordinate-ID lock is already held.
    #[error("subid identity database '{database}' is busy")]
    SubidBusy {
        /// Public database path.
        database: String,
    },
    /// Rotation idempotency or compare-and-swap failed.
    #[error("launcher rotation id conflict: {0}")]
    RotationConflict(String),
}

struct InstallLock(File);

impl Drop for InstallLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Installs or refreshes the launcher authority without silently changing its
/// operator, identity pool, or signing key.
pub fn install(
    paths: &LauncherPaths,
    runner: &impl CommandRunner,
    request: &InstallRequest,
    now_ms: u64,
) -> Result<LauncherStatus, LauncherError> {
    validate_paths(paths)?;
    require_system_install_context(paths)?;
    validate_operator(&request.operator)?;
    identity::validate_pool(&request.pool)?;
    inspect_system_existing_state_dirs(paths)?;
    ensure_state_dirs(paths)?;
    require_system_state_dirs(paths)?;
    require_system_existing_state_files(paths)?;
    let _install_lock = acquire_install_lock(paths)?;

    let release = load_current_release(paths)?;
    let ssh_digest = hash_file(&paths.ssh_keygen, "ssh-keygen")?.to_string();
    let getent_digest = hash_file(&paths.getent, "getent")?.to_string();
    let operator_uid =
        identity::validate_install_authority(paths, runner, &request.pool, &request.operator)?;

    let existing_config: Option<LauncherConfig> = read_optional_json(&paths.config())?;
    let existing_keyring: Option<PublicKeyring> = read_optional_json(&paths.keyring())?;
    if let Some(config) = &existing_config {
        validate_config(config, paths)?;
        if config.operator != request.operator
            || config.operator_uid != operator_uid
            || config.pool != request.pool
        {
            return Err(LauncherError::Invalid(
                "reinstall may not change the operator or identity pool".to_owned(),
            ));
        }
    }
    if existing_config.is_none() && existing_keyring.is_some() {
        return Err(LauncherError::Malformed(
            "public keyring exists without configuration that binds its install intent".to_owned(),
        ));
    }
    if let Some(keyring) = &existing_keyring {
        validate_keyring(paths, keyring, true)?;
        require_system_private_keys(paths, keyring)?;
    }

    let config = LauncherConfig {
        schema: CONFIG_SCHEMA.to_owned(),
        operator: request.operator.clone(),
        operator_uid,
        release_id: release.manifest.release_id,
        launcher_digest: release.launcher_digest,
        launcher_path: paths.launcher(),
        ssh_keygen_path: paths.ssh_keygen.clone(),
        ssh_keygen_digest: ssh_digest,
        getent_path: paths.getent.clone(),
        getent_digest,
        pool: request.pool.clone(),
    };
    if let Some(keyring) = &existing_keyring {
        validate_private_public_keys(paths, runner, &config, keyring)?;
    }
    validate_sudoers(paths, runner, &config)?;

    // Configuration binds bootstrap intent before any persistent reservation
    // or random key generation. It grants no authority until sudoers is
    // published as the final step.
    write_json_atomic(&paths.config(), &config, 0o600)?;

    identity::reserve_install_authority(paths, runner, &request.pool)?;

    match existing_keyring {
        Some(_) => {}
        None => {
            let generated = recover_or_generate_initial_key(paths, runner, &config)?;
            let keyring = PublicKeyring {
                schema: KEYRING_SCHEMA.to_owned(),
                active_key_id: generated.key_id.clone(),
                keys: vec![LauncherKey {
                    key_id: generated.key_id,
                    public_key: generated.public_key,
                    created_at_ms: now_ms,
                    retired_at_ms: None,
                    rotation_id: None,
                    replaces: None,
                }],
            };
            write_json_atomic(&paths.keyring(), &keyring, 0o444)?;
        }
    }

    identity::ensure_slot_files(paths, request.pool.slots)?;
    let sudoers = render_sudoers(&config)?;
    write_atomic(&paths.sudoers, sudoers.as_bytes(), 0o440)?;

    Ok(status(paths))
}

/// Rotates the launcher software key exactly once for one idempotency identity.
pub fn rotate(
    paths: &LauncherPaths,
    runner: &impl CommandRunner,
    request: &RotationRequest,
    now_ms: u64,
) -> Result<RotationOutcome, LauncherError> {
    validate_paths(paths)?;
    require_system_install_context(paths)?;
    validate_rotation_id(&request.rotation_id)?;
    let expected_digest = Digest::parse(&request.expected_active_key_id)
        .map_err(|error| LauncherError::Invalid(error.to_string()))?;
    if expected_digest.to_string() != request.expected_active_key_id {
        return Err(LauncherError::Invalid(
            "expected active key id must use canonical sha256:<hex> form".to_owned(),
        ));
    }
    inspect_system_existing_state_dirs(paths)?;
    ensure_state_dirs(paths)?;
    require_system_state_dirs(paths)?;
    require_system_existing_state_files(paths)?;
    let _install_lock = acquire_install_lock(paths)?;
    let config: LauncherConfig = read_required_json(&paths.config())?;
    validate_config(&config, paths)?;
    require_measured_tool(&config)?;
    let mut keyring = public_keyring(paths)?;
    validate_keyring(paths, &keyring, true)?;
    require_system_private_keys(paths, &keyring)?;
    require_configured_release(paths, &config)?;
    validate_private_public_keys(paths, runner, &config, &keyring)?;

    if let Some(key) = keyring
        .keys
        .iter()
        .find(|key| key.rotation_id.as_deref() == Some(&request.rotation_id))
    {
        if key.replaces.as_deref() != Some(&request.expected_active_key_id) {
            return Err(LauncherError::RotationConflict(format!(
                "rotation id '{}' already names a different transition",
                request.rotation_id
            )));
        }
        clear_matching_pending_rotation(paths, request, &key.key_id)?;
        return Ok(RotationOutcome {
            key_id: key.key_id.clone(),
            active_key_id: keyring.active_key_id,
            created: false,
        });
    }
    if keyring.active_key_id != request.expected_active_key_id {
        return Err(LauncherError::RotationConflict(format!(
            "expected active key '{}' but found '{}'",
            request.expected_active_key_id, keyring.active_key_id
        )));
    }

    let mut pending = match read_optional_json::<PendingRotation>(&paths.pending_rotation())? {
        Some(pending) => {
            validate_pending_rotation(paths, request, &pending)?;
            pending
        }
        None => {
            let pending = PendingRotation {
                schema: PENDING_ROTATION_SCHEMA.to_owned(),
                rotation_id: request.rotation_id.clone(),
                expected_active_key_id: request.expected_active_key_id.clone(),
                key_id: None,
                public_key: None,
                created_at_ms: now_ms,
            };
            write_json_atomic(&paths.pending_rotation(), &pending, 0o600)?;
            pending
        }
    };
    let generated = materialize_pending_rotation_key(paths, runner, &config, &mut pending)?;
    let active = keyring
        .keys
        .iter_mut()
        .find(|key| key.key_id == keyring.active_key_id)
        .expect("validated keyring contains its active key");
    active.retired_at_ms = Some(pending.created_at_ms);
    keyring.keys.push(LauncherKey {
        key_id: generated.key_id.clone(),
        public_key: generated.public_key,
        created_at_ms: pending.created_at_ms,
        retired_at_ms: None,
        rotation_id: Some(request.rotation_id.clone()),
        replaces: Some(request.expected_active_key_id.clone()),
    });
    keyring.active_key_id = generated.key_id.clone();
    write_json_atomic(&paths.keyring(), &keyring, 0o444)?;
    clear_pending_rotation(paths)?;
    Ok(RotationOutcome {
        key_id: generated.key_id.clone(),
        active_key_id: generated.key_id,
        created: true,
    })
}

/// Reads and validates the public keyring without exposing private paths.
pub fn public_keyring(paths: &LauncherPaths) -> Result<PublicKeyring, LauncherError> {
    require_system_public_keyring(paths)?;
    let keyring = read_required_json(&paths.keyring())?;
    validate_keyring(paths, &keyring, false)?;
    Ok(keyring)
}

/// Acquires one pool identity until the returned lease is dropped.
pub fn acquire_identity(paths: &LauncherPaths, slot: u32) -> Result<IdentityLease, LauncherError> {
    identity::acquire(paths, slot)
}

/// Builds the exact documented non-interactive launcher invocation.
pub fn sudo_invocation(config: &LauncherConfig) -> CommandInvocation {
    CommandInvocation {
        program: PathBuf::from(SUDO_PATH),
        arguments: vec![
            OsString::from("-n"),
            config.launcher_path.as_os_str().to_owned(),
            OsString::from("run"),
        ],
        stdin: Vec::new(),
        current_dir: None,
    }
}

/// Reports every launcher trust failure without returning private bytes or paths.
pub fn status(paths: &LauncherPaths) -> LauncherStatus {
    let mut failures = Vec::new();
    let mut config_valid = false;
    let mut keyring_valid = false;
    let release_ownership = install::ownership_of(&paths.release_prefix);
    if !release_ownership.root_owned || release_ownership.world_writable {
        failures.push(failure(
            "release_prefix_permissions",
            "The release prefix is not wholly root-owned and non-writable by other users.",
            "Reinstall the signed release and launcher authority under root ownership.",
        ));
    }
    let config = match read_status_json::<LauncherConfig>(paths, &paths.config(), 0o600, "config") {
        Ok(config) => config,
        Err(error) => {
            failures.push(failure(
                "config_unreadable",
                error.to_string(),
                "Run the root launcher installer to repair its configuration.",
            ));
            None
        }
    };
    let keyring =
        match read_status_json::<PublicKeyring>(paths, &paths.keyring(), 0o444, "public keyring") {
            Ok(keyring) => keyring,
            Err(error) => {
                failures.push(failure(
                    "keyring_unreadable",
                    error.to_string(),
                    "Restore the public keyring from root-owned launcher state.",
                ));
                None
            }
        };

    if config.is_none() {
        failures.push(failure(
            "not_installed",
            "No launcher configuration is installed.",
            "Run the launcher installer as root.",
        ));
    }
    if keyring.is_none() {
        failures.push(failure(
            "keyring_missing",
            "No launcher public keyring is installed.",
            "Run the launcher installer as root.",
        ));
    }

    if let Some(config) = &config {
        if let Err(error) = validate_config(config, paths) {
            failures.push(failure(
                "config_invalid",
                error.to_string(),
                "Repair the launcher installation as root.",
            ));
        } else {
            config_valid = true;
            if *paths != LauncherPaths::system()
                || check_secure_tool(&config.ssh_keygen_path, &mut failures)
            {
                match hash_file(&config.ssh_keygen_path, "ssh-keygen") {
                    Ok(found) if found.to_string() == config.ssh_keygen_digest => {}
                    Ok(_) => failures.push(failure(
                        "ssh_keygen_changed",
                        "The measured ssh-keygen bytes changed after installation.",
                        "Review the system tool update, then rerun the root launcher installer.",
                    )),
                    Err(error) => failures.push(failure(
                        "ssh_keygen_unreadable",
                        error.to_string(),
                        "Restore the measured ssh-keygen binary.",
                    )),
                }
            }
            if *paths != LauncherPaths::system()
                || identity::check_secure_tool(&config.getent_path, &mut failures)
            {
                match hash_file(&config.getent_path, "getent") {
                    Ok(found) if found.to_string() == config.getent_digest => {}
                    Ok(_) => failures.push(failure(
                        "getent_changed",
                        "The measured getent bytes changed after installation.",
                        "Review the system tool update, then rerun the root launcher installer.",
                    )),
                    Err(error) => failures.push(failure(
                        "getent_unreadable",
                        error.to_string(),
                        "Restore the measured getent binary.",
                    )),
                }
            }
            match load_current_release(paths) {
                Ok(release)
                    if release.manifest.release_id == config.release_id
                        && release.launcher_digest == config.launcher_digest => {}
                Ok(_) => failures.push(failure(
                    "release_changed",
                    "The current launcher release does not match root configuration.",
                    "Rerun the root launcher installer for the current signed release.",
                )),
                Err(error) => failures.push(failure(
                    "release_unusable",
                    error.to_string(),
                    "Repair or reinstall the signed current release.",
                )),
            }
            if let Ok(expected) = render_sudoers(config) {
                match fs::read_to_string(&paths.sudoers) {
                    Ok(found) if found == expected => {}
                    Ok(_) => failures.push(failure(
                        "sudoers_changed",
                        "The sudoers fragment does not grant only the measured run command.",
                        "Rerun the root launcher installer and validate the fragment with visudo.",
                    )),
                    Err(error) => failures.push(failure(
                        "sudoers_unreadable",
                        error.to_string(),
                        "Restore the dedicated sudoers fragment as root.",
                    )),
                }
            }
            if let Err(error) =
                identity::validate_installed_authority(paths, &SystemCommandRunner, config)
            {
                failures.push(failure(
                    "identity_authority_changed",
                    error.to_string(),
                    "Repair the operator and subordinate-ID authority before launching Sessions.",
                ));
            }
        }
    }

    if let Some(keyring) = &keyring {
        match validate_keyring(paths, keyring, true) {
            Ok(()) => keyring_valid = true,
            Err(error) => failures.push(failure(
                "keyring_invalid",
                error.to_string(),
                "Restore the complete launcher keyring as root.",
            )),
        }
    }
    if config_valid
        && keyring_valid
        && let (Some(config), Some(keyring)) = (&config, &keyring)
        && let Err(error) =
            validate_private_public_keys(paths, &SystemCommandRunner, config, keyring)
    {
        failures.push(failure(
            "private_key_mismatch",
            error.to_string(),
            "Restore each retained private key that matches the enrolled public key.",
        ));
    }

    check_metadata(&paths.state_root, 0o711, true, "state_root", &mut failures);
    check_metadata(&paths.config(), 0o600, false, "config", &mut failures);
    check_metadata(&paths.keyring(), 0o444, false, "keyring", &mut failures);
    check_metadata(
        &paths.private(),
        0o700,
        true,
        "private_state",
        &mut failures,
    );
    check_metadata(&paths.keys(), 0o700, true, "private_keys", &mut failures);
    check_metadata(
        &paths.scratch(),
        0o700,
        true,
        "private_scratch",
        &mut failures,
    );
    check_metadata(&paths.locks(), 0o700, true, "identity_locks", &mut failures);
    check_metadata(&paths.sudoers, 0o440, false, "sudoers", &mut failures);
    if let Some(keyring) = &keyring {
        check_private_key_metadata(paths, keyring, &mut failures);
    }
    let pending_rotation = fs::symlink_metadata(paths.pending_rotation());
    let pending_key = fs::symlink_metadata(paths.pending_key());
    match pending_rotation {
        Ok(_) => failures.push(failure(
            "rotation_incomplete",
            "A durable launcher key rotation has not finished publishing.",
            "Replay the same rotation id and expected key as root.",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => match pending_key {
            Ok(_) => failures.push(failure(
                "bootstrap_incomplete",
                "Initial launcher key publication has not finished.",
                "Rerun the root launcher installer with the same operator and identity pool.",
            )),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => failures.push(failure(
                "bootstrap_state_unreadable",
                error.to_string(),
                "Repair the root-only bootstrap key state, then rerun the launcher installer.",
            )),
        },
        Err(error) => failures.push(failure(
            "rotation_state_unreadable",
            error.to_string(),
            "Repair the root-only rotation journal before rotating keys.",
        )),
    }

    let occupied_slots = config
        .as_ref()
        .filter(|_| config_valid)
        .map(|config| identity::occupied_slots(paths, config.pool.slots, &mut failures))
        .unwrap_or_default();
    let active_key_id = keyring
        .as_ref()
        .filter(|_| keyring_valid)
        .map(|keyring| keyring.active_key_id.clone());
    let retained_key_ids = keyring
        .as_ref()
        .filter(|_| keyring_valid)
        .map(PublicKeyring::retained_key_ids)
        .unwrap_or_default();

    LauncherStatus {
        schema: STATUS_SCHEMA.to_owned(),
        trusted: failures.is_empty(),
        config: config.filter(|_| config_valid),
        active_key_id,
        retained_key_ids,
        occupied_slots,
        failures,
    }
}

/// A measured signer bound to the launch-receipt namespace and installed keyring.
pub struct LauncherSigner {
    ssh_keygen: PathBuf,
    ssh_keygen_digest: String,
    release_id: String,
    scratch: PathBuf,
    keys: PathBuf,
    keyring: PublicKeyring,
}

impl LauncherSigner {
    /// Opens the installed root-only signer after revalidating its authority.
    pub fn open(paths: &LauncherPaths) -> Result<Self, LauncherError> {
        validate_paths(paths)?;
        require_root_metadata(&paths.state_root, 0o711, true, "launcher state")?;
        require_root_metadata(&paths.config(), 0o600, false, "launcher config")?;
        require_root_metadata(&paths.keyring(), 0o444, false, "launcher keyring")?;
        let config: LauncherConfig = read_required_json(&paths.config())?;
        validate_config(&config, paths)?;
        require_secure_tool(&config.ssh_keygen_path, "ssh-keygen")?;
        require_measured_tool(&config)?;
        require_configured_release(paths, &config)?;
        require_root_private(paths)?;
        let keyring = public_keyring(paths)?;
        validate_keyring(paths, &keyring, true)?;
        require_private_key_ownership(paths, &keyring)?;
        validate_private_public_keys(paths, &SystemCommandRunner, &config, &keyring)?;
        Ok(Self {
            ssh_keygen: config.ssh_keygen_path,
            ssh_keygen_digest: config.ssh_keygen_digest,
            release_id: config.release_id,
            scratch: paths.scratch(),
            keys: paths.keys(),
            keyring,
        })
    }

    /// Signs exact canonical receipt bytes with the key bound to a live chain.
    ///
    /// Both active and retired keys are usable; retired private keys are kept so
    /// a chain can retain the key named by its genesis receipt.
    pub fn sign_receipt(&self, key_id: &str, payload: &[u8]) -> Result<String, LauncherError> {
        self.sign_receipt_with(&SystemCommandRunner, key_id, payload)
    }

    fn sign_receipt_with(
        &self,
        runner: &impl CommandRunner,
        key_id: &str,
        payload: &[u8],
    ) -> Result<String, LauncherError> {
        let receipt = ReceiptPayload::parse_canonical(payload)
            .map_err(|error| LauncherError::Malformed(error.to_string()))?;
        if receipt.signing_key_id != key_id {
            return Err(LauncherError::Invalid(
                "receipt signing key does not match the selected launcher key".to_owned(),
            ));
        }
        if receipt.release_id != self.release_id {
            return Err(LauncherError::Invalid(
                "receipt release does not match the configured launcher release".to_owned(),
            ));
        }
        let enrolled = self.keyring.key(key_id).ok_or_else(|| {
            LauncherError::Invalid(format!(
                "launcher key '{key_id}' is not in the installed keyring"
            ))
        })?;
        require_secure_tool(&self.ssh_keygen, "ssh-keygen")?;
        let found = hash_file(&self.ssh_keygen, "ssh-keygen")?.to_string();
        if found != self.ssh_keygen_digest {
            return Err(LauncherError::Invalid(
                "measured ssh-keygen changed before receipt signing".to_owned(),
            ));
        }
        let key = private_key_path(&self.keys, key_id)?;
        ensure_private_key(&key)?;
        let scratch = unique_path(&self.scratch, "sign");
        create_private_dir(&scratch)?;
        let result = (|| {
            let message = scratch.join("receipt");
            write_new(&message, payload, 0o600)?;
            let invocation = CommandInvocation {
                program: self.ssh_keygen.clone(),
                arguments: vec![
                    OsString::from("-Y"),
                    OsString::from("sign"),
                    OsString::from("-q"),
                    OsString::from("-n"),
                    OsString::from(RECEIPT_SCHEMA),
                    OsString::from("-f"),
                    key.into_os_string(),
                    message.as_os_str().to_owned(),
                ],
                stdin: Vec::new(),
                current_dir: Some(scratch.clone()),
            };
            let output = runner
                .run(&invocation)
                .map_err(|source| io_error("ssh-keygen", source))?;
            if !output.success {
                return Err(tool_error("ssh-keygen", &output.stderr));
            }
            let signature_path = message.with_extension("sig");
            let signature_metadata = fs::symlink_metadata(&signature_path)
                .map_err(|source| io_error("receipt signature", source))?;
            if !signature_metadata.is_file()
                || signature_metadata.file_type().is_symlink()
                || signature_metadata.len() > 64 * 1024
            {
                return Err(LauncherError::Invalid(
                    "ssh-keygen produced an unsafe receipt signature file".to_owned(),
                ));
            }
            let signature = fs::read_to_string(&signature_path)
                .map_err(|source| io_error("receipt signature", source))?;
            let parsed = sshsig::parse(&signature)
                .map_err(|error| LauncherError::Malformed(error.to_string()))?;
            if parsed.namespace != RECEIPT_SCHEMA
                || parsed.openssh_public_key() != enrolled.public_key
            {
                return Err(LauncherError::Invalid(
                    "ssh-keygen produced a receipt signature for the wrong authority".to_owned(),
                ));
            }
            let allowed_signers = scratch.join("allowed-signers");
            write_new(
                &allowed_signers,
                format!("louiselm-launch {}\n", enrolled.public_key).as_bytes(),
                0o600,
            )?;
            let verify = runner
                .run(&CommandInvocation {
                    program: self.ssh_keygen.clone(),
                    arguments: vec![
                        OsString::from("-Y"),
                        OsString::from("verify"),
                        OsString::from("-f"),
                        allowed_signers.into_os_string(),
                        OsString::from("-I"),
                        OsString::from("louiselm-launch"),
                        OsString::from("-n"),
                        OsString::from(RECEIPT_SCHEMA),
                        OsString::from("-s"),
                        signature_path.into_os_string(),
                    ],
                    stdin: payload.to_vec(),
                    current_dir: Some(scratch.clone()),
                })
                .map_err(|source| io_error("ssh-keygen", source))?;
            if !verify.success {
                return Err(LauncherError::Invalid(
                    "ssh-keygen did not verify its launcher receipt signature".to_owned(),
                ));
            }
            Ok(signature)
        })();
        finish_private_scratch(&scratch, result)
    }
}

struct ReleaseBinding {
    manifest: ReleaseManifest,
    launcher_digest: String,
}

struct GeneratedKey {
    key_id: String,
    public_key: String,
}

fn validate_paths(paths: &LauncherPaths) -> Result<(), LauncherError> {
    for (name, path) in [
        ("release prefix", &paths.release_prefix),
        ("launcher state", &paths.state_root),
        ("sudoers", &paths.sudoers),
        ("subuid", &paths.subuid),
        ("subgid", &paths.subgid),
        ("passwd", &paths.passwd),
        ("group", &paths.group),
        ("nsswitch", &paths.nsswitch),
        ("ssh-keygen", &paths.ssh_keygen),
        ("getent", &paths.getent),
        ("visudo", &paths.visudo),
    ] {
        if !path.is_absolute() {
            return Err(LauncherError::Invalid(format!(
                "{name} path must be absolute"
            )));
        }
    }
    if paths.state_root != paths.release_prefix.join("launcher") {
        return Err(LauncherError::Invalid(
            "launcher state must live below the release prefix".to_owned(),
        ));
    }
    Ok(())
}

fn require_system_install_context(paths: &LauncherPaths) -> Result<(), LauncherError> {
    if *paths != LauncherPaths::system() {
        return Ok(());
    }
    if !rustix::process::geteuid().is_root() {
        return Err(LauncherError::Invalid(
            "the fixed launcher authority must be installed by root".to_owned(),
        ));
    }
    require_secure_tool(&paths.ssh_keygen, "ssh-keygen")?;
    require_secure_tool(&paths.visudo, "visudo")?;
    identity::require_system_install_context(paths)
}

fn inspect_system_existing_state_dirs(paths: &LauncherPaths) -> Result<(), LauncherError> {
    if *paths != LauncherPaths::system() {
        return Ok(());
    }
    for (path, private) in [
        (paths.state_root.clone(), false),
        (paths.private(), true),
        (paths.keys(), true),
        (paths.scratch(), true),
        (paths.locks(), true),
        (paths.pending_key(), true),
    ] {
        match fs::symlink_metadata(&path) {
            Ok(metadata)
                if metadata.uid() == 0
                    && metadata.is_dir()
                    && !metadata.file_type().is_symlink()
                    && if private {
                        metadata.mode() & 0o077 == 0
                    } else {
                        metadata.mode() & 0o022 == 0
                    } => {}
            Ok(_) => {
                return Err(LauncherError::Invalid(
                    "existing launcher state directory is not safely root-owned".to_owned(),
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(io_error("launcher state", source)),
        }
    }
    Ok(())
}

fn require_system_state_dirs(paths: &LauncherPaths) -> Result<(), LauncherError> {
    if *paths != LauncherPaths::system() {
        return Ok(());
    }
    for (path, mode) in [
        (paths.state_root.clone(), 0o711),
        (paths.private(), 0o700),
        (paths.keys(), 0o700),
        (paths.scratch(), 0o700),
        (paths.locks(), 0o700),
    ] {
        require_root_metadata(&path, mode, true, "launcher state")?;
    }
    Ok(())
}

fn require_system_existing_state_files(paths: &LauncherPaths) -> Result<(), LauncherError> {
    if *paths != LauncherPaths::system() {
        return Ok(());
    }
    for (path, mode, label) in [
        (paths.config(), 0o600, "launcher config"),
        (paths.keyring(), 0o444, "launcher keyring"),
        (paths.pending_rotation(), 0o600, "pending launcher rotation"),
    ] {
        match fs::symlink_metadata(&path) {
            Ok(_) => require_root_metadata(&path, mode, false, label)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(io_error(label, source)),
        }
    }
    Ok(())
}

fn require_system_config(paths: &LauncherPaths) -> Result<(), LauncherError> {
    if *paths == LauncherPaths::system() {
        require_root_metadata(&paths.config(), 0o600, false, "launcher config")?;
    }
    Ok(())
}

fn require_system_public_keyring(paths: &LauncherPaths) -> Result<(), LauncherError> {
    if *paths == LauncherPaths::system() {
        require_root_metadata(&paths.state_root, 0o711, true, "launcher state")?;
        require_root_metadata(&paths.keyring(), 0o444, false, "launcher keyring")?;
    }
    Ok(())
}

fn require_system_private_keys(
    paths: &LauncherPaths,
    keyring: &PublicKeyring,
) -> Result<(), LauncherError> {
    if *paths == LauncherPaths::system() {
        require_private_key_ownership(paths, keyring)?;
    }
    Ok(())
}

fn require_root_metadata(
    path: &Path,
    mode: u32,
    directory: bool,
    label: &str,
) -> Result<(), LauncherError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| io_error(label, source))?;
    if metadata.uid() == 0
        && metadata.mode() & 0o777 == mode
        && metadata.is_dir() == directory
        && !metadata.file_type().is_symlink()
    {
        Ok(())
    } else {
        Err(LauncherError::Invalid(format!(
            "{label} is not root-owned with its fixed mode"
        )))
    }
}

fn require_secure_tool(path: &Path, label: &str) -> Result<(), LauncherError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| io_error(label, source))?;
    if metadata.is_file()
        && !metadata.file_type().is_symlink()
        && metadata.uid() == 0
        && metadata.mode() & 0o022 == 0
        && metadata.mode() & 0o111 != 0
    {
        Ok(())
    } else {
        Err(LauncherError::Invalid(format!(
            "{label} must be a root-owned, non-writable executable"
        )))
    }
}

fn validate_operator(operator: &str) -> Result<(), LauncherError> {
    let valid = !operator.is_empty()
        && operator.len() <= 32
        && operator
            .bytes()
            .enumerate()
            .all(|(index, byte)| match byte {
                b'a'..=b'z' | b'_' => true,
                b'0'..=b'9' | b'-' if index > 0 => true,
                _ => false,
            });
    if !valid {
        return Err(LauncherError::Invalid(
            "operator must be a conservative local account name".to_owned(),
        ));
    }
    Ok(())
}

fn validate_rotation_id(rotation_id: &str) -> Result<(), LauncherError> {
    let valid = !rotation_id.is_empty()
        && rotation_id.len() <= 128
        && rotation_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
    if valid {
        Ok(())
    } else {
        Err(LauncherError::Invalid(
            "rotation id must contain 1-128 ASCII letters, digits, '.', '_', or '-'".to_owned(),
        ))
    }
}

fn ensure_state_dirs(paths: &LauncherPaths) -> Result<(), LauncherError> {
    create_dir_mode(&paths.state_root, 0o711)?;
    create_dir_mode(&paths.private(), 0o700)?;
    create_dir_mode(&paths.keys(), 0o700)?;
    create_dir_mode(&paths.scratch(), 0o700)?;
    create_dir_mode(&paths.locks(), 0o700)?;
    Ok(())
}

fn create_dir_mode(path: &Path, mode: u32) -> Result<(), LauncherError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(LauncherError::Invalid(format!(
                "'{}' must be a real directory",
                path.display()
            )));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.mode(mode);
            builder
                .create(path)
                .map_err(|source| io_error(path.display().to_string(), source))?;
        }
        Err(source) => return Err(io_error(path.display().to_string(), source)),
    }
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|source| io_error(path.display().to_string(), source))
}

fn create_private_dir(path: &Path) -> Result<(), LauncherError> {
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    builder
        .create(path)
        .map_err(|source| io_error("private scratch", source))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|source| io_error("private scratch", source))
}

fn require_private_directory(
    paths: &LauncherPaths,
    path: &Path,
    label: &str,
) -> Result<(), LauncherError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| io_error(label, source))?;
    if metadata.is_dir()
        && !metadata.file_type().is_symlink()
        && metadata.mode() & 0o777 == 0o700
        && (*paths != LauncherPaths::system() || metadata.uid() == 0)
    {
        Ok(())
    } else {
        Err(LauncherError::Invalid(format!(
            "{label} is not a safely owned private directory"
        )))
    }
}

fn acquire_install_lock(paths: &LauncherPaths) -> Result<InstallLock, LauncherError> {
    let path = paths.private().join("install.lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&path)
        .map_err(|source| io_error("launcher install lock", source))?;
    match file.try_lock() {
        Ok(()) => Ok(InstallLock(file)),
        Err(TryLockError::WouldBlock) => Err(LauncherError::InstallBusy),
        Err(TryLockError::Error(source)) => Err(io_error("launcher install lock", source)),
    }
}

fn load_current_release(paths: &LauncherPaths) -> Result<ReleaseBinding, LauncherError> {
    let state = install::load_state(&paths.release_prefix)
        .map_err(|error| LauncherError::Malformed(error.to_string()))?
        .ok_or_else(|| LauncherError::Invalid("no signed release is installed".to_owned()))?;
    if state.schema != RELEASE_STATE_SCHEMA {
        return Err(LauncherError::Malformed(format!(
            "installed release schema is '{}'",
            state.schema
        )));
    }
    let expected_current = Path::new("releases").join(&state.release_id);
    let current = fs::read_link(paths.release_prefix.join("current"))
        .map_err(|source| io_error("current release", source))?;
    if current != expected_current {
        return Err(LauncherError::Invalid(
            "current release symlink does not match installed state".to_owned(),
        ));
    }
    let root = paths.release_prefix.join(&expected_current);
    let bytes = fs::read(root.join("manifest.json"))
        .map_err(|source| io_error("current release manifest", source))?;
    let manifest: ReleaseManifest = serde_json::from_slice(&bytes)
        .map_err(|error| LauncherError::Malformed(error.to_string()))?;
    if manifest.schema != MANIFEST_SCHEMA
        || manifest.release_id != state.release_id
        || manifest.digest().to_string() != manifest.release_id
    {
        return Err(LauncherError::Malformed(
            "current release manifest identity does not verify".to_owned(),
        ));
    }
    let component = manifest.component(LAUNCHER_COMPONENT).ok_or_else(|| {
        LauncherError::Invalid("current release has no louiselm-launch component".to_owned())
    })?;
    if component.path != LAUNCHER_RELATIVE_PATH || !component.executable {
        return Err(LauncherError::Invalid(
            "launcher component does not use the fixed executable path".to_owned(),
        ));
    }
    let launcher = root.join(LAUNCHER_RELATIVE_PATH);
    let launcher_metadata = fs::symlink_metadata(&launcher)
        .map_err(|source| io_error("current launcher component", source))?;
    if !launcher_metadata.is_file()
        || launcher_metadata.file_type().is_symlink()
        || launcher_metadata.mode() & 0o111 == 0
        || launcher_metadata.mode() & 0o022 != 0
        || (*paths == LauncherPaths::system() && launcher_metadata.uid() != 0)
    {
        return Err(LauncherError::Invalid(
            "current launcher is not a root-owned, non-writable regular executable".to_owned(),
        ));
    }
    let launcher_bytes =
        fs::read(&launcher).map_err(|source| io_error("current launcher component", source))?;
    let digest = Digest::of(&launcher_bytes);
    if digest.hex() != component.sha256 || launcher_bytes.len() as u64 != component.size {
        return Err(LauncherError::Invalid(
            "current launcher bytes do not match their release manifest".to_owned(),
        ));
    }
    Ok(ReleaseBinding {
        manifest,
        launcher_digest: digest.to_string(),
    })
}

fn require_configured_release(
    paths: &LauncherPaths,
    config: &LauncherConfig,
) -> Result<(), LauncherError> {
    let release = load_current_release(paths)?;
    if release.manifest.release_id == config.release_id
        && release.launcher_digest == config.launcher_digest
    {
        Ok(())
    } else {
        Err(LauncherError::Invalid(
            "configured launcher release is not the measured current release".to_owned(),
        ))
    }
}

fn validate_sudoers(
    paths: &LauncherPaths,
    runner: &impl CommandRunner,
    config: &LauncherConfig,
) -> Result<(), LauncherError> {
    let candidate = unique_path(&paths.scratch(), "sudoers");
    let sudoers = render_sudoers(config)?;
    write_new(&candidate, sudoers.as_bytes(), 0o600)?;
    let invocation = CommandInvocation {
        program: paths.visudo.clone(),
        arguments: vec![OsString::from("-cf"), candidate.as_os_str().to_owned()],
        stdin: Vec::new(),
        current_dir: None,
    };
    let output = runner
        .run(&invocation)
        .map_err(|source| io_error("visudo", source));
    let _ = fs::remove_file(&candidate);
    let output = output?;
    if output.success {
        Ok(())
    } else {
        Err(tool_error("visudo", &output.stderr))
    }
}

fn render_sudoers(config: &LauncherConfig) -> Result<String, LauncherError> {
    let digest = Digest::parse(&config.launcher_digest)
        .map_err(|error| LauncherError::Malformed(error.to_string()))?;
    Ok(format!(
        "# Managed by louiselm-skills. Do not edit.\nDefaults!{} fdexec=digest_only\n#{} ALL=(root:root) NOPASSWD: NOSETENV: sha256:{} {} run\n",
        config.launcher_path.display(),
        config.operator_uid,
        digest.hex(),
        config.launcher_path.display(),
    ))
}

#[cfg(test)]
fn generate_key(
    paths: &LauncherPaths,
    runner: &impl CommandRunner,
) -> Result<GeneratedKey, LauncherError> {
    let scratch = unique_path(&paths.scratch(), "keygen");
    create_private_dir(&scratch)?;
    let result = (|| {
        let private = scratch.join("key");
        run_keygen(paths, runner, &scratch)?;
        let public_path = private.with_extension("pub");
        let public_key = normalize_public_key(&read_text(&public_path)?)?;
        let derived = derive_public_key(runner, &paths.ssh_keygen, &private, &scratch)?;
        if derived != public_key {
            return Err(LauncherError::Invalid(
                "generated launcher private and public keys do not match".to_owned(),
            ));
        }
        let key_id = Digest::of(public_key.as_bytes()).to_string();
        let destination = private_key_path(&paths.keys(), &key_id)?;
        let key_dir = destination
            .parent()
            .expect("private key path always has a parent");
        if key_dir.exists() {
            return Err(LauncherError::Malformed(
                "generated launcher key identity already exists".to_owned(),
            ));
        }
        create_private_dir(key_dir)?;
        fs::rename(&private, &destination)
            .map_err(|source| io_error("generated private key", source))?;
        File::open(&destination)
            .and_then(|file| file.sync_all())
            .map_err(|source| io_error("generated private key", source))?;
        sync_dir(key_dir)?;
        sync_dir(&paths.keys())?;
        Ok(GeneratedKey { key_id, public_key })
    })();
    finish_private_scratch(&scratch, result)
}

fn recover_or_generate_initial_key(
    paths: &LauncherPaths,
    runner: &impl CommandRunner,
    config: &LauncherConfig,
) -> Result<GeneratedKey, LauncherError> {
    let mut entries =
        fs::read_dir(paths.keys()).map_err(|source| io_error("launcher private keys", source))?;
    let Some(entry) = entries
        .next()
        .transpose()
        .map_err(|source| io_error("launcher private keys", source))?
    else {
        return materialize_initial_key(paths, runner, config);
    };
    if entries
        .next()
        .transpose()
        .map_err(|source| io_error("launcher private keys", source))?
        .is_some()
    {
        return Err(LauncherError::Malformed(
            "launcher bootstrap found multiple un-enrolled private keys".to_owned(),
        ));
    }
    let key_dir = entry.path();
    require_private_directory(paths, &key_dir, "un-enrolled launcher key")?;
    let mut key_entries =
        fs::read_dir(&key_dir).map_err(|source| io_error("un-enrolled launcher key", source))?;
    let Some(key_entry) = key_entries
        .next()
        .transpose()
        .map_err(|source| io_error("un-enrolled launcher key", source))?
    else {
        if paths.pending_key().exists() {
            return materialize_initial_key(paths, runner, config);
        }
        return Err(LauncherError::Malformed(
            "un-enrolled launcher key directory is empty".to_owned(),
        ));
    };
    if key_entry.file_name() != "key"
        || key_entries
            .next()
            .transpose()
            .map_err(|source| io_error("un-enrolled launcher key", source))?
            .is_some()
    {
        return Err(LauncherError::Malformed(
            "un-enrolled launcher key directory has unexpected contents".to_owned(),
        ));
    }
    let private = key_entry.path();
    if *paths == LauncherPaths::system() {
        require_secure_tool(&config.ssh_keygen_path, "ssh-keygen")?;
        require_root_metadata(&private, 0o600, false, "un-enrolled launcher key")?;
    }
    require_measured_tool(config)?;
    ensure_private_key(&private)?;
    let public_key = derive_public_key(runner, &config.ssh_keygen_path, &private, &key_dir)?;
    let generated = GeneratedKey {
        key_id: Digest::of(public_key.as_bytes()).to_string(),
        public_key,
    };
    if private_key_path(&paths.keys(), &generated.key_id)? != private {
        return Err(LauncherError::Malformed(
            "un-enrolled launcher key is stored under the wrong identity".to_owned(),
        ));
    }
    Ok(generated)
}

fn materialize_initial_key(
    paths: &LauncherPaths,
    runner: &impl CommandRunner,
    config: &LauncherConfig,
) -> Result<GeneratedKey, LauncherError> {
    let pending = paths.pending_key();
    match fs::symlink_metadata(&pending) {
        Ok(_) => require_private_directory(paths, &pending, "pending launcher key")?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            create_private_dir(&pending)?;
            sync_dir(&paths.private())?;
        }
        Err(source) => return Err(io_error("pending launcher key", source)),
    }
    let private = pending.join("key");
    let public_path = private.with_extension("pub");
    match fs::symlink_metadata(&private) {
        Ok(_) => ensure_private_key(&private)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match fs::symlink_metadata(&public_path) {
                Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                    fs::remove_file(&public_path)
                        .map_err(|source| io_error("incomplete pending public key", source))?;
                }
                Ok(_) => {
                    return Err(LauncherError::Invalid(
                        "pending launcher public key is not a regular file".to_owned(),
                    ));
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(source) => return Err(io_error("pending launcher public key", source)),
            }
            if *paths == LauncherPaths::system() {
                require_secure_tool(&config.ssh_keygen_path, "ssh-keygen")?;
            }
            require_measured_tool(config)?;
            run_keygen(paths, runner, &pending)?;
        }
        Err(source) => return Err(io_error("pending launcher private key", source)),
    }
    if *paths == LauncherPaths::system() {
        require_secure_tool(&config.ssh_keygen_path, "ssh-keygen")?;
        require_root_metadata(&private, 0o600, false, "pending launcher private key")?;
    }
    require_measured_tool(config)?;
    let public_key = derive_public_key(runner, &config.ssh_keygen_path, &private, &pending)?;
    match fs::symlink_metadata(&public_path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            if normalize_public_key(&read_text(&public_path)?)? != public_key {
                return Err(LauncherError::Invalid(
                    "pending launcher private and public keys do not match".to_owned(),
                ));
            }
            fs::remove_file(&public_path)
                .map_err(|source| io_error("pending launcher public key", source))?;
        }
        Ok(_) => {
            return Err(LauncherError::Invalid(
                "pending launcher public key is not a regular file".to_owned(),
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(source) => return Err(io_error("pending launcher public key", source)),
    }
    sync_dir(&pending)?;
    let generated = GeneratedKey {
        key_id: Digest::of(public_key.as_bytes()).to_string(),
        public_key,
    };
    let destination = private_key_path(&paths.keys(), &generated.key_id)?;
    let key_dir = destination
        .parent()
        .expect("private key path always has a parent");
    match fs::symlink_metadata(key_dir) {
        Ok(_) => {
            require_private_directory(paths, key_dir, "pending launcher key destination")?;
            if fs::read_dir(key_dir)
                .map_err(|source| io_error("pending launcher key destination", source))?
                .next()
                .transpose()
                .map_err(|source| io_error("pending launcher key destination", source))?
                .is_some()
            {
                return Err(LauncherError::Malformed(
                    "pending launcher key destination has unexpected contents".to_owned(),
                ));
            }
            fs::remove_dir(key_dir)
                .map_err(|source| io_error("pending launcher key destination", source))?;
            sync_dir(&paths.keys())?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(source) => return Err(io_error("pending launcher key destination", source)),
    }
    fs::rename(&pending, key_dir).map_err(|source| io_error("pending launcher key", source))?;
    sync_dir(&paths.private())?;
    File::open(&destination)
        .and_then(|file| file.sync_all())
        .map_err(|source| io_error("pending launcher private key", source))?;
    sync_dir(key_dir)?;
    sync_dir(&paths.keys())?;
    Ok(generated)
}

fn run_keygen(
    paths: &LauncherPaths,
    runner: &impl CommandRunner,
    directory: &Path,
) -> Result<(), LauncherError> {
    let private = directory.join("key");
    let invocation = CommandInvocation {
        program: paths.ssh_keygen.clone(),
        arguments: vec![
            OsString::from("-q"),
            OsString::from("-t"),
            OsString::from("ed25519"),
            OsString::from("-N"),
            OsString::new(),
            OsString::from("-C"),
            OsString::from("louiselm-launch"),
            OsString::from("-f"),
            private.as_os_str().to_owned(),
        ],
        stdin: Vec::new(),
        current_dir: Some(directory.to_owned()),
    };
    let output = runner
        .run(&invocation)
        .map_err(|source| io_error("ssh-keygen", source))?;
    if !output.success {
        return Err(tool_error("ssh-keygen", &output.stderr));
    }
    ensure_regular(&private, "generated private key")?;
    let public = private.with_extension("pub");
    ensure_regular(&public, "generated public key")?;
    fs::set_permissions(&private, fs::Permissions::from_mode(0o600))
        .map_err(|source| io_error("generated private key", source))?;
    File::open(&private)
        .and_then(|file| file.sync_all())
        .map_err(|source| io_error("generated private key", source))?;
    File::open(&public)
        .and_then(|file| file.sync_all())
        .map_err(|source| io_error("generated public key", source))?;
    sync_dir(directory)
}

fn materialize_pending_rotation_key(
    paths: &LauncherPaths,
    runner: &impl CommandRunner,
    config: &LauncherConfig,
    pending: &mut PendingRotation,
) -> Result<GeneratedKey, LauncherError> {
    if let (Some(key_id), Some(public_key)) = (&pending.key_id, &pending.public_key) {
        let generated = GeneratedKey {
            key_id: key_id.clone(),
            public_key: public_key.clone(),
        };
        let destination = private_key_path(&paths.keys(), &generated.key_id)?;
        if destination.exists() {
            validate_one_private_public_key(paths, runner, config, &generated, &destination)?;
            cleanup_pending_key(paths)?;
            return Ok(generated);
        }
    }

    let pending_key = paths.pending_key();
    match fs::symlink_metadata(&pending_key) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(LauncherError::Invalid(
                "pending launcher key path is not a private directory".to_owned(),
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            create_private_dir(&pending_key)?;
            sync_dir(&paths.private())?;
        }
        Err(source) => return Err(io_error("pending launcher key", source)),
    }
    if *paths == LauncherPaths::system() {
        require_root_metadata(&pending_key, 0o700, true, "pending launcher key")?;
    }

    let private = pending_key.join("key");
    let public_path = private.with_extension("pub");
    match fs::symlink_metadata(&private) {
        Ok(_) => ensure_private_key(&private)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match fs::symlink_metadata(&public_path) {
                Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                    fs::remove_file(&public_path)
                        .map_err(|source| io_error("incomplete pending public key", source))?;
                }
                Ok(_) => {
                    return Err(LauncherError::Invalid(
                        "pending launcher public key is not a regular file".to_owned(),
                    ));
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(source) => return Err(io_error("pending launcher public key", source)),
            }
            if *paths == LauncherPaths::system() {
                require_secure_tool(&config.ssh_keygen_path, "ssh-keygen")?;
            }
            require_measured_tool(config)?;
            run_keygen(paths, runner, &pending_key)?;
        }
        Err(source) => return Err(io_error("pending launcher private key", source)),
    }

    if *paths == LauncherPaths::system() {
        require_secure_tool(&config.ssh_keygen_path, "ssh-keygen")?;
        require_root_metadata(&private, 0o600, false, "launcher private key")?;
    }
    require_measured_tool(config)?;
    let public_key = derive_public_key(runner, &config.ssh_keygen_path, &private, &pending_key)?;
    if public_path.exists() && normalize_public_key(&read_text(&public_path)?)? != public_key {
        return Err(LauncherError::Invalid(
            "pending launcher private and public keys do not match".to_owned(),
        ));
    }
    let generated = GeneratedKey {
        key_id: Digest::of(public_key.as_bytes()).to_string(),
        public_key,
    };
    match (&pending.key_id, &pending.public_key) {
        (None, None) => {
            pending.key_id = Some(generated.key_id.clone());
            pending.public_key = Some(generated.public_key.clone());
            write_json_atomic(&paths.pending_rotation(), pending, 0o600)?;
        }
        (Some(key_id), Some(public_key))
            if key_id == &generated.key_id && public_key == &generated.public_key => {}
        _ => {
            return Err(LauncherError::Malformed(
                "pending launcher rotation changed its generated key".to_owned(),
            ));
        }
    }

    let destination = private_key_path(&paths.keys(), &generated.key_id)?;
    let key_dir = destination
        .parent()
        .expect("private key path always has a parent");
    match fs::symlink_metadata(key_dir) {
        Ok(_) => {
            require_private_directory(paths, key_dir, "pending launcher key destination")?;
            if fs::read_dir(key_dir)
                .map_err(|source| io_error("pending launcher key destination", source))?
                .next()
                .transpose()
                .map_err(|source| io_error("pending launcher key destination", source))?
                .is_some()
            {
                return Err(LauncherError::Malformed(
                    "pending launcher key destination has unexpected contents".to_owned(),
                ));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            create_private_dir(key_dir)?;
        }
        Err(source) => return Err(io_error("pending launcher key destination", source)),
    }
    fs::rename(&private, &destination)
        .map_err(|source| io_error("pending launcher private key", source))?;
    File::open(&destination)
        .and_then(|file| file.sync_all())
        .map_err(|source| io_error("pending launcher private key", source))?;
    sync_dir(key_dir)?;
    sync_dir(&paths.keys())?;
    cleanup_pending_key(paths)?;
    Ok(generated)
}

fn normalize_public_key(raw: &str) -> Result<String, LauncherError> {
    let fields = raw.split_whitespace().collect::<Vec<_>>();
    if fields.len() < 2
        || fields[0] != "ssh-ed25519"
        || fields[1].is_empty()
        || !fields[1]
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
    {
        return Err(LauncherError::Malformed(
            "generated launcher public key is not Ed25519 OpenSSH format".to_owned(),
        ));
    }
    Ok(format!("{} {}", fields[0], fields[1]))
}

fn validate_pending_rotation(
    _paths: &LauncherPaths,
    request: &RotationRequest,
    pending: &PendingRotation,
) -> Result<(), LauncherError> {
    if pending.schema != PENDING_ROTATION_SCHEMA
        || pending.rotation_id != request.rotation_id
        || pending.expected_active_key_id != request.expected_active_key_id
    {
        return Err(LauncherError::RotationConflict(
            "a different unfinished rotation must be resolved first".to_owned(),
        ));
    }
    match (&pending.key_id, &pending.public_key) {
        (None, None) => {}
        (Some(key_id), Some(public_key)) => {
            let normalized = normalize_public_key(public_key)?;
            if &normalized != public_key || Digest::of(normalized.as_bytes()).to_string() != *key_id
            {
                return Err(LauncherError::Malformed(
                    "pending launcher rotation key identity is invalid".to_owned(),
                ));
            }
        }
        _ => {
            return Err(LauncherError::Invalid(
                "pending launcher rotation has incomplete key metadata".to_owned(),
            ));
        }
    }
    Ok(())
}

fn clear_matching_pending_rotation(
    paths: &LauncherPaths,
    request: &RotationRequest,
    key_id: &str,
) -> Result<(), LauncherError> {
    let Some(pending) = read_optional_json::<PendingRotation>(&paths.pending_rotation())? else {
        return Ok(());
    };
    if pending.rotation_id != request.rotation_id
        || pending.expected_active_key_id != request.expected_active_key_id
        || pending.key_id.as_deref() != Some(key_id)
    {
        return Err(LauncherError::RotationConflict(
            "completed rotation conflicts with an unfinished rotation journal".to_owned(),
        ));
    }
    clear_pending_rotation(paths)
}

fn clear_pending_rotation(paths: &LauncherPaths) -> Result<(), LauncherError> {
    cleanup_pending_key(paths)?;
    match fs::remove_file(paths.pending_rotation()) {
        Ok(()) => sync_dir(&paths.private()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(io_error("pending launcher rotation", source)),
    }
}

fn cleanup_pending_key(paths: &LauncherPaths) -> Result<(), LauncherError> {
    let directory = paths.pending_key();
    match fs::symlink_metadata(&directory) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(LauncherError::Invalid(
                "pending launcher key path is not a private directory".to_owned(),
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(io_error("pending launcher key", source)),
    }
    for path in [directory.join("key"), directory.join("key.pub")] {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(io_error("pending launcher key", source)),
        }
    }
    fs::remove_dir(&directory).map_err(|source| io_error("pending launcher key", source))?;
    sync_dir(&paths.private())
}

fn validate_config(config: &LauncherConfig, paths: &LauncherPaths) -> Result<(), LauncherError> {
    if config.schema != CONFIG_SCHEMA {
        return Err(LauncherError::Malformed(format!(
            "launcher configuration schema is '{}'",
            config.schema
        )));
    }
    validate_operator(&config.operator)?;
    if config.operator_uid == 0 {
        return Err(LauncherError::Invalid(
            "launcher operator UID may not be root".to_owned(),
        ));
    }
    identity::validate_pool(&config.pool)?;
    Digest::parse(&config.release_id)
        .map_err(|error| LauncherError::Malformed(error.to_string()))?;
    Digest::parse(&config.launcher_digest)
        .map_err(|error| LauncherError::Malformed(error.to_string()))?;
    Digest::parse(&config.ssh_keygen_digest)
        .map_err(|error| LauncherError::Malformed(error.to_string()))?;
    Digest::parse(&config.getent_digest)
        .map_err(|error| LauncherError::Malformed(error.to_string()))?;
    if config.launcher_path != paths.launcher()
        || config.ssh_keygen_path != paths.ssh_keygen
        || config.getent_path != paths.getent
        || !config.launcher_path.is_absolute()
        || !config.ssh_keygen_path.is_absolute()
        || !config.getent_path.is_absolute()
    {
        return Err(LauncherError::Invalid(
            "launcher configuration contains a non-fixed executable path".to_owned(),
        ));
    }
    Ok(())
}

fn validate_keyring(
    paths: &LauncherPaths,
    keyring: &PublicKeyring,
    require_private: bool,
) -> Result<(), LauncherError> {
    if keyring.schema != KEYRING_SCHEMA || keyring.keys.is_empty() {
        return Err(LauncherError::Malformed(
            "public launcher keyring has an unsupported schema or no keys".to_owned(),
        ));
    }
    let mut ids = HashSet::new();
    let mut active = 0;
    let mut rotations = HashSet::new();
    for key in &keyring.keys {
        let expected = Digest::of(key.public_key.as_bytes()).to_string();
        if normalize_public_key(&key.public_key)? != key.public_key || expected != key.key_id {
            return Err(LauncherError::Malformed(
                "public launcher key identity does not match its bytes".to_owned(),
            ));
        }
        if !ids.insert(key.key_id.clone()) {
            return Err(LauncherError::Malformed(
                "public launcher keyring contains duplicate keys".to_owned(),
            ));
        }
        if let Some(rotation_id) = &key.rotation_id
            && !rotations.insert(rotation_id.clone())
        {
            return Err(LauncherError::Malformed(
                "public launcher keyring contains duplicate rotation ids".to_owned(),
            ));
        }
        if key.key_id == keyring.active_key_id {
            active += 1;
            if key.retired_at_ms.is_some() {
                return Err(LauncherError::Malformed(
                    "active launcher key is marked retired".to_owned(),
                ));
            }
        } else if key.retired_at_ms.is_none() {
            return Err(LauncherError::Malformed(
                "retained launcher key has no retirement time".to_owned(),
            ));
        }
        if require_private {
            let path = private_key_path(&paths.keys(), &key.key_id)?;
            ensure_private_key(&path)?;
        }
    }
    if active != 1 {
        return Err(LauncherError::Malformed(
            "public launcher keyring does not contain exactly one active key".to_owned(),
        ));
    }
    Ok(())
}

fn ensure_private_key(path: &Path) -> Result<(), LauncherError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|source| io_error("launcher private key", source))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.mode() & 0o777 != 0o600
    {
        return Err(LauncherError::Invalid(
            "launcher private key is missing or has unsafe permissions".to_owned(),
        ));
    }
    Ok(())
}

fn require_measured_tool(config: &LauncherConfig) -> Result<(), LauncherError> {
    let found = hash_file(&config.ssh_keygen_path, "ssh-keygen")?.to_string();
    if found != config.ssh_keygen_digest {
        return Err(LauncherError::Invalid(
            "measured ssh-keygen changed after installation".to_owned(),
        ));
    }
    Ok(())
}

fn validate_private_public_keys(
    paths: &LauncherPaths,
    runner: &impl CommandRunner,
    config: &LauncherConfig,
    keyring: &PublicKeyring,
) -> Result<(), LauncherError> {
    for enrolled in &keyring.keys {
        let private = private_key_path(&paths.keys(), &enrolled.key_id)?;
        validate_one_private_public_key(
            paths,
            runner,
            config,
            &GeneratedKey {
                key_id: enrolled.key_id.clone(),
                public_key: enrolled.public_key.clone(),
            },
            &private,
        )?;
    }
    Ok(())
}

fn validate_one_private_public_key(
    paths: &LauncherPaths,
    runner: &impl CommandRunner,
    config: &LauncherConfig,
    enrolled: &GeneratedKey,
    private: &Path,
) -> Result<(), LauncherError> {
    if *paths == LauncherPaths::system() {
        require_secure_tool(&config.ssh_keygen_path, "ssh-keygen")?;
        require_root_metadata(private, 0o600, false, "launcher private key")?;
    }
    require_measured_tool(config)?;
    ensure_private_key(private)?;
    let public = derive_public_key(runner, &config.ssh_keygen_path, private, &paths.private())?;
    if public != enrolled.public_key || Digest::of(public.as_bytes()).to_string() != enrolled.key_id
    {
        return Err(LauncherError::Invalid(format!(
            "private launcher key '{}' does not match its enrolled public key",
            enrolled.key_id
        )));
    }
    Ok(())
}

fn derive_public_key(
    runner: &impl CommandRunner,
    ssh_keygen: &Path,
    private: &Path,
    current_dir: &Path,
) -> Result<String, LauncherError> {
    let invocation = CommandInvocation {
        program: ssh_keygen.to_owned(),
        arguments: vec![
            OsString::from("-y"),
            OsString::from("-f"),
            private.as_os_str().to_owned(),
        ],
        stdin: Vec::new(),
        current_dir: Some(current_dir.to_owned()),
    };
    let output = runner
        .run(&invocation)
        .map_err(|source| io_error("ssh-keygen", source))?;
    if !output.success {
        return Err(LauncherError::Invalid(
            "ssh-keygen could not derive an enrolled launcher public key".to_owned(),
        ));
    }
    if output.stdout.len() > 16 * 1024 {
        return Err(LauncherError::Invalid(
            "ssh-keygen returned an oversized launcher public key".to_owned(),
        ));
    }
    let public = std::str::from_utf8(&output.stdout)
        .map_err(|_| LauncherError::Malformed("ssh-keygen returned non-UTF-8 output".to_owned()))?;
    normalize_public_key(public)
}

fn require_root_private(paths: &LauncherPaths) -> Result<(), LauncherError> {
    for (path, mode) in [
        (paths.private(), 0o700),
        (paths.keys(), 0o700),
        (paths.scratch(), 0o700),
    ] {
        let metadata = fs::symlink_metadata(&path)
            .map_err(|source| io_error("launcher private state", source))?;
        if metadata.uid() != 0
            || metadata.mode() & 0o777 != mode
            || !metadata.is_dir()
            || metadata.file_type().is_symlink()
        {
            return Err(LauncherError::Invalid(
                "launcher private state is not root-owned mode 0700".to_owned(),
            ));
        }
    }
    Ok(())
}

fn require_private_key_ownership(
    paths: &LauncherPaths,
    keyring: &PublicKeyring,
) -> Result<(), LauncherError> {
    for key in &keyring.keys {
        let path = private_key_path(&paths.keys(), &key.key_id)?;
        let metadata = fs::symlink_metadata(path)
            .map_err(|source| io_error("launcher private key", source))?;
        if metadata.uid() != 0
            || metadata.mode() & 0o777 != 0o600
            || !metadata.is_file()
            || metadata.file_type().is_symlink()
        {
            return Err(LauncherError::Invalid(
                "launcher private keys are not root-owned mode 0600".to_owned(),
            ));
        }
    }
    Ok(())
}

fn private_key_path(keys: &Path, key_id: &str) -> Result<PathBuf, LauncherError> {
    let digest =
        Digest::parse(key_id).map_err(|error| LauncherError::Malformed(error.to_string()))?;
    Ok(keys.join(digest.directory_name()).join("key"))
}

fn read_text(path: &Path) -> Result<String, LauncherError> {
    fs::read_to_string(path).map_err(|source| io_error(path.display().to_string(), source))
}

fn read_optional_json<T: for<'de> Deserialize<'de>>(
    path: &Path,
) -> Result<Option<T>, LauncherError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(io_error(path.display().to_string(), source)),
    };
    let mut bytes = Vec::new();
    file.take(MAX_STATE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| io_error(path.display().to_string(), source))?;
    if bytes.len() as u64 > MAX_STATE_BYTES {
        return Err(LauncherError::Malformed(
            "launcher state exceeds its size limit".to_owned(),
        ));
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| LauncherError::Malformed(error.to_string()))
}

fn read_status_json<T: for<'de> Deserialize<'de>>(
    paths: &LauncherPaths,
    path: &Path,
    mode: u32,
    label: &str,
) -> Result<Option<T>, LauncherError> {
    if *paths == LauncherPaths::system() {
        match fs::symlink_metadata(path) {
            Ok(_) => require_root_metadata(path, mode, false, label)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(io_error(label, source)),
        }
    }
    read_optional_json(path)
}

fn read_required_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, LauncherError> {
    read_optional_json(path)?.ok_or_else(|| {
        LauncherError::Malformed(format!(
            "required launcher state '{}' is missing",
            path.display()
        ))
    })
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T, mode: u32) -> Result<(), LauncherError> {
    let bytes =
        serde_json::to_vec(value).map_err(|error| LauncherError::Malformed(error.to_string()))?;
    write_atomic(path, &bytes, mode)
}

fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> Result<(), LauncherError> {
    let parent = path
        .parent()
        .ok_or_else(|| LauncherError::Invalid(format!("'{}' has no parent", path.display())))?;
    fs::create_dir_all(parent).map_err(|source| io_error(parent.display().to_string(), source))?;
    let temporary = unique_path(parent, ".pending");
    let result = (|| {
        write_new(&temporary, bytes, mode)?;
        fs::rename(&temporary, path)
            .map_err(|source| io_error(path.display().to_string(), source))?;
        sync_dir(parent)
    })();
    let _ = fs::remove_file(&temporary);
    result
}

fn write_new(path: &Path, bytes: &[u8], mode: u32) -> Result<(), LauncherError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)
        .map_err(|source| io_error(path.display().to_string(), source))?;
    file.write_all(bytes)
        .map_err(|source| io_error(path.display().to_string(), source))?;
    file.set_permissions(fs::Permissions::from_mode(mode))
        .map_err(|source| io_error(path.display().to_string(), source))?;
    file.sync_all()
        .map_err(|source| io_error(path.display().to_string(), source))
}

fn sync_dir(path: &Path) -> Result<(), LauncherError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error(path.display().to_string(), source))
}

fn unique_path(parent: &Path, label: &str) -> PathBuf {
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    parent.join(format!("{label}-{}-{counter}", std::process::id()))
}

fn finish_private_scratch<T>(
    path: &Path,
    result: Result<T, LauncherError>,
) -> Result<T, LauncherError> {
    let cleanup = fs::remove_dir_all(path).map_err(|source| io_error("private scratch", source));
    match (result, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (_, Err(error)) => Err(error),
    }
}

fn hash_file(path: &Path, label: &str) -> Result<Digest, LauncherError> {
    ensure_regular(path, label)?;
    let bytes = fs::read(path).map_err(|source| io_error(label, source))?;
    Ok(Digest::of(&bytes))
}

fn ensure_regular(path: &Path, label: &str) -> Result<(), LauncherError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| io_error(label, source))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(LauncherError::Invalid(format!(
            "{label} must be a regular file"
        )));
    }
    Ok(())
}

fn check_secure_tool(path: &Path, failures: &mut Vec<LauncherFailure>) -> bool {
    match fs::symlink_metadata(path) {
        Ok(metadata)
            if metadata.is_file()
                && !metadata.file_type().is_symlink()
                && metadata.uid() == 0
                && metadata.mode() & 0o022 == 0
                && metadata.mode() & 0o111 != 0 =>
        {
            true
        }
        Ok(_) => {
            failures.push(failure(
                "ssh_keygen_permissions",
                "The measured ssh-keygen is not a root-owned, non-writable executable.",
                "Restore the system OpenSSH executable before signing receipts.",
            ));
            false
        }
        Err(error) => {
            failures.push(failure(
                "ssh_keygen_unreadable",
                error.to_string(),
                "Restore the measured ssh-keygen executable.",
            ));
            false
        }
    }
}

fn check_private_key_metadata(
    paths: &LauncherPaths,
    keyring: &PublicKeyring,
    failures: &mut Vec<LauncherFailure>,
) {
    for key in &keyring.keys {
        let Ok(path) = private_key_path(&paths.keys(), &key.key_id) else {
            continue;
        };
        match fs::symlink_metadata(path) {
            Ok(metadata)
                if metadata.is_file()
                    && !metadata.file_type().is_symlink()
                    && metadata.uid() == 0
                    && metadata.mode() & 0o777 == 0o600 => {}
            Ok(_) => failures.push(failure(
                "private_key_permissions",
                "A launcher private key has unsafe ownership, type, or mode.",
                "Restore every launcher private key as root-owned mode 0600.",
            )),
            Err(_) => failures.push(failure(
                "private_key_unreadable",
                "A launcher private key is missing or unreadable.",
                "Restore the retained private key before its live chains need another receipt.",
            )),
        }
    }
}

fn check_metadata(
    path: &Path,
    expected_mode: u32,
    directory: bool,
    code: &str,
    failures: &mut Vec<LauncherFailure>,
) {
    match fs::symlink_metadata(path) {
        Ok(metadata)
            if !metadata.file_type().is_symlink()
                && metadata.uid() == 0
                && metadata.mode() & 0o777 == expected_mode
                && metadata.is_dir() == directory => {}
        Ok(_) => failures.push(failure(
            &format!("{code}_permissions"),
            "A launcher installation path has unsafe ownership, type, or mode.",
            "Restore the documented root ownership and permissions.",
        )),
        Err(error) => failures.push(failure(
            &format!("{code}_unreadable"),
            error.to_string(),
            "Restore the missing launcher installation path as root.",
        )),
    }
}

fn failure(code: &str, detail: impl Into<String>, next_action: &str) -> LauncherFailure {
    LauncherFailure {
        code: code.to_owned(),
        detail: detail.into(),
        next_action: next_action.to_owned(),
    }
}

fn io_error(path: impl Into<String>, source: io::Error) -> LauncherError {
    LauncherError::Io {
        path: path.into(),
        source,
    }
}

fn tool_error(tool: &'static str, stderr: &[u8]) -> LauncherError {
    let stderr = &stderr[..stderr.len().min(4096)];
    let reason = crate::scan::escape(String::from_utf8_lossy(stderr).trim());
    LauncherError::Tool {
        tool,
        reason: if reason.is_empty() {
            "command exited unsuccessfully".to_owned()
        } else {
            reason
        },
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use tempfile::TempDir;

    use super::*;
    use crate::launch_receipt::{Authorization, ReceiptOutcome, SessionState};

    struct RecordingRunner {
        calls: RefCell<Vec<CommandInvocation>>,
    }

    impl CommandRunner for RecordingRunner {
        fn run(&self, invocation: &CommandInvocation) -> io::Result<CommandOutput> {
            self.calls.borrow_mut().push(invocation.clone());
            SystemCommandRunner.run(invocation)
        }
    }

    struct TamperingRunner;

    impl CommandRunner for TamperingRunner {
        fn run(&self, invocation: &CommandInvocation) -> io::Result<CommandOutput> {
            if invocation
                .arguments
                .windows(2)
                .any(|pair| pair[0] == "-Y" && pair[1] == "verify")
            {
                let signature = invocation
                    .arguments
                    .iter()
                    .position(|argument| argument == "-s")
                    .and_then(|index| invocation.arguments.get(index + 1))
                    .expect("verification names its signature");
                fs::write(signature, b"not an SSH signature\n")?;
            }
            SystemCommandRunner.run(invocation)
        }
    }

    fn signer_paths(root: &Path) -> LauncherPaths {
        let release_prefix = root.join("release");
        LauncherPaths {
            state_root: release_prefix.join("launcher"),
            release_prefix,
            sudoers: root.join("sudoers"),
            subuid: root.join("subuid"),
            subgid: root.join("subgid"),
            passwd: root.join("passwd"),
            group: root.join("group"),
            nsswitch: root.join("nsswitch"),
            ssh_keygen: PathBuf::from("/usr/bin/ssh-keygen"),
            getent: PathBuf::from("/usr/bin/getent"),
            visudo: PathBuf::from("/usr/sbin/visudo"),
        }
    }

    fn receipt_bytes(release_id: &str, signing_key_id: &str) -> Vec<u8> {
        let request_id = "request-1".to_owned();
        ReceiptPayload {
            schema: RECEIPT_SCHEMA.to_owned(),
            session_id: "session-1".to_owned(),
            run_id: "run-1".to_owned(),
            request_id: request_id.clone(),
            envelope_revision: 1,
            sequence: 1,
            previous_receipt_digest: Some(Digest::of(b"previous").to_string()),
            release_id: release_id.to_owned(),
            signing_key_id: signing_key_id.to_owned(),
            outcome: ReceiptOutcome::Interrupt {
                authorization: Authorization {
                    authorization_id: "authorization-1".to_owned(),
                    request_id,
                    request_digest: Digest::of(b"request-1").to_string(),
                },
            },
            resulting_state: SessionState::Running,
        }
        .canonical_bytes()
    }

    #[test]
    fn signer_uses_measured_absolute_tool_fixed_namespace_and_retired_key() {
        let root = TempDir::new().expect("scratch root is creatable");
        let paths = signer_paths(root.path());
        fs::create_dir(&paths.release_prefix).expect("release prefix is creatable");
        ensure_state_dirs(&paths).expect("signer directories are creatable");
        let runner = RecordingRunner {
            calls: RefCell::new(Vec::new()),
        };
        let first = generate_key(&paths, &runner).expect("first key is generated");
        let second = generate_key(&paths, &runner).expect("second key is generated");
        runner.calls.borrow_mut().clear();
        let keyring = PublicKeyring {
            schema: KEYRING_SCHEMA.to_owned(),
            active_key_id: second.key_id.clone(),
            keys: vec![
                LauncherKey {
                    key_id: first.key_id.clone(),
                    public_key: first.public_key.clone(),
                    created_at_ms: 1,
                    retired_at_ms: Some(2),
                    rotation_id: None,
                    replaces: None,
                },
                LauncherKey {
                    key_id: second.key_id,
                    public_key: second.public_key,
                    created_at_ms: 2,
                    retired_at_ms: None,
                    rotation_id: Some("rotate-1".to_owned()),
                    replaces: Some(first.key_id.clone()),
                },
            ],
        };
        let mut signer = LauncherSigner {
            ssh_keygen: paths.ssh_keygen.clone(),
            ssh_keygen_digest: hash_file(&paths.ssh_keygen, "ssh-keygen")
                .unwrap()
                .to_string(),
            release_id: Digest::of(b"release").to_string(),
            scratch: paths.scratch(),
            keys: paths.keys(),
            keyring,
        };
        let payload = receipt_bytes(&signer.release_id, &first.key_id);

        let signature = signer
            .sign_receipt_with(&runner, &first.key_id, &payload)
            .expect("retired chain key still signs");

        sshsig::verify(
            &signature,
            RECEIPT_SCHEMA,
            &payload,
            &first.public_key,
            sshsig::SkPolicy::none(),
        )
        .expect("real SSHSIG verifies");
        let calls = runner.calls.borrow();
        assert_eq!(calls.len(), 2);
        assert!(
            calls
                .iter()
                .all(|call| call.program == Path::new("/usr/bin/ssh-keygen"))
        );
        let arguments = calls
            .iter()
            .flat_map(|call| &call.arguments)
            .map(|argument| argument.to_string_lossy())
            .collect::<Vec<_>>();
        assert!(
            arguments
                .windows(2)
                .any(|pair| { pair[0] == "-n" && pair[1] == RECEIPT_SCHEMA })
        );
        assert!(calls.iter().any(|call| {
            call.arguments
                .windows(2)
                .any(|pair| pair[0] == "-Y" && pair[1] == "sign")
        }));
        let verify = calls
            .iter()
            .find(|call| {
                call.arguments
                    .windows(2)
                    .any(|pair| pair[0] == "-Y" && pair[1] == "verify")
            })
            .expect("the measured tool verifies its signature before release");
        assert_eq!(verify.stdin, payload);
        assert_eq!(
            fs::read_dir(paths.scratch()).unwrap().count(),
            0,
            "private scratch is removed after signing"
        );
        drop(calls);

        let wrong_key = receipt_bytes(&signer.release_id, &signer.keyring.active_key_id);
        let error = signer
            .sign_receipt_with(&runner, &first.key_id, &wrong_key)
            .expect_err("payload cannot name a different signing key");
        assert!(error.to_string().contains("signing key"), "{error}");
        let wrong_release = receipt_bytes(&Digest::of(b"other release").to_string(), &first.key_id);
        let error = signer
            .sign_receipt_with(&runner, &first.key_id, &wrong_release)
            .expect_err("payload cannot name a different release");
        assert!(error.to_string().contains("release"), "{error}");
        let mut noncanonical = payload.clone();
        noncanonical.push(b'\n');
        signer
            .sign_receipt_with(&runner, &first.key_id, &noncanonical)
            .expect_err("non-canonical payload bytes are refused");
        assert_eq!(
            runner.calls.borrow().len(),
            2,
            "invalid payloads never invoke the signing tool"
        );

        let error = signer
            .sign_receipt_with(&TamperingRunner, &first.key_id, &payload)
            .expect_err("a signature changed after creation is never released");
        assert!(error.to_string().contains("did not verify"), "{error}");
        assert_eq!(
            fs::read_dir(paths.scratch()).unwrap().count(),
            0,
            "failed verification also removes private scratch"
        );

        signer.ssh_keygen_digest = Digest::of(b"different tool").to_string();
        let error = signer
            .sign_receipt_with(&runner, &first.key_id, &payload)
            .expect_err("changed signing tool is refused");
        assert!(error.to_string().contains("changed"), "{error}");
        assert_eq!(runner.calls.borrow().len(), 2, "changed tool was not run");
    }
}
