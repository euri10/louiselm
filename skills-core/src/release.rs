//! Building, signing, and identifying a trusted release.
//!
//! A launcher, verifier, or policy that runs from the development checkout is
//! not a trust boundary: the code being confined can edit the code deciding
//! whether confinement worked. A release breaks that circle by binding one
//! exact clean commit, its locked dependencies, its toolchain, its policy, and
//! the resulting bytes into a single signed identity, authorized by a role
//! that is not the one that admits skills.
//!
//! Two things this module refuses to pretend:
//!
//! * A development build is never a release. [`running_identity`] says so with
//!   a code, and nothing downstream has to infer it.
//! * A bundle's manifest is a claim. Verification re-hashes every component
//!   from the bundle before believing any of it.

use std::{
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    canonical::{Digest, Hasher},
    policy::Policy,
    signer::{Signer, SignerError},
    sshsig,
    store::Store,
    trust::{Role, TrustError, TrustStore, persistence::LockedTrust},
};

/// The signature namespace a release is authorized in.
///
/// Distinct from Skill Admission: a release signature must never verify as an
/// Admission, and the roles are separate even on one physical token.
pub const RELEASE_NAMESPACE: &str = "louiselm.release/1";

/// The release manifest schema this build reads and writes.
pub const MANIFEST_SCHEMA: &str = "louiselm.release.manifest/1";

/// The fixed set of component names a release may install.
///
/// The manifest names components; it does not get to invent them. A caller
/// cannot introduce a new installed command by writing one into a bundle.
pub const ALLOWED_COMPONENTS: [&str; 3] =
    ["louiselm-skills", "louiselm-launch", "louiselm-control"];

/// What the source tree was when the release was built.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceIdentity {
    /// Commit the release was built from.
    pub commit: String,
    /// Whether the tree had no modifications and no untracked files.
    pub clean: bool,
    /// `git describe` output, for humans.
    pub describe: String,
    /// Digest of the dependency lockfile.
    pub dependencies_digest: String,
}

impl SourceIdentity {
    /// Reads the identity of `repository`, refusing anything but a clean tree.
    ///
    /// Untracked files count as dirty. A file that is not in the commit cannot
    /// be reviewed by looking at the commit, and it can still be compiled in.
    ///
    /// # Errors
    /// Returns Git invocation errors or refuses a dirty/untracked source tree.
    pub fn of(repository: &Path, dependencies_digest: &str) -> Result<Self, ReleaseError> {
        let status = git(repository, &["status", "--porcelain"])?;
        if !status.trim().is_empty() {
            return Err(ReleaseError::DirtySource(crate::scan::escape(
                status.lines().take(5).collect::<Vec<_>>().join("; ").trim(),
            )));
        }
        Ok(Self {
            commit: git(repository, &["rev-parse", "HEAD"])?,
            clean: true,
            describe: git(repository, &["describe", "--always", "--tags"])
                .unwrap_or_else(|_| "untagged".to_owned()),
            dependencies_digest: dependencies_digest.to_owned(),
        })
    }
}

/// What built the release.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolchainIdentity {
    /// `rustc` version.
    pub rustc: String,
    /// `cargo` version.
    pub cargo: String,
    /// Target the components run on.
    pub target: String,
}

impl ToolchainIdentity {
    /// Reads the toolchain currently on `PATH`.
    ///
    /// # Errors
    /// Returns tool invocation errors when rustc or Cargo versions cannot be read.
    pub fn detect() -> Result<Self, ReleaseError> {
        Ok(Self {
            rustc: tool_version("rustc")?,
            cargo: tool_version("cargo")?,
            target: format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS),
        })
    }
}

/// What a component is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentKind {
    /// A binary the trusted prefix will expose.
    Executable,
    /// Data the binaries read.
    Data,
}

/// A component offered to [`assemble`].
#[derive(Clone, Debug)]
pub struct ComponentInput {
    /// Name, which must be in [`ALLOWED_COMPONENTS`] for an executable.
    pub name: String,
    /// Where the built file currently is.
    pub path: PathBuf,
    /// What it is.
    pub kind: ComponentKind,
}

/// One component, as bound by the manifest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Component {
    /// Component name.
    pub name: String,
    /// Path inside the bundle and inside the installed release.
    pub path: String,
    /// Content address.
    pub sha256: String,
    /// Size in bytes.
    pub size: u64,
    /// Whether the installed file carries the executable bit.
    pub executable: bool,
}

/// The policy a release carries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyIdentity {
    /// Policy version.
    pub version: String,
    /// Content address of the exact policy bytes.
    pub digest: String,
}

/// The complete identity of one release.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseManifest {
    /// Schema identifier.
    pub schema: String,
    /// Release identity: the digest of this manifest's canonical bytes.
    pub release_id: String,
    /// Crate version the release was cut from.
    pub version: String,
    /// When it was built, by the builder's clock.
    pub built_at_ms: u64,
    /// Source identity.
    pub source: SourceIdentity,
    /// Toolchain identity.
    pub toolchain: ToolchainIdentity,
    /// Policy identity.
    pub policy: PolicyIdentity,
    /// Schema identifiers this release implements.
    pub schemas: Vec<String>,
    /// Bound components, sorted by name.
    pub components: Vec<Component>,
}

impl ReleaseManifest {
    /// Serializes the manifest to the bytes the release role signs.
    ///
    /// # Panics
    /// Panics only if serialization fails after a future schema change introduces
    /// a fallible serializer. The current derived schema has only JSON-native values.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "Derived schema has only JSON-native values and string-keyed maps, with no custom serializers."
    )]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut identified = self.clone();
        identified.release_id = String::new();
        serde_json::to_vec(&identified).expect("a release manifest is always serializable")
    }

    /// Returns the release identity: the digest of its canonical bytes.
    #[must_use]
    pub fn digest(&self) -> Digest {
        Digest::of(&self.canonical_bytes())
    }

    /// Returns the component with `name`, when the release has one.
    #[must_use]
    pub fn component(&self, name: &str) -> Option<&Component> {
        self.components
            .iter()
            .find(|component| component.name == name)
    }
}

/// What to assemble into a bundle.
pub struct AssembleRequest<'a> {
    /// Source identity, which must be clean.
    pub source: SourceIdentity,
    /// Toolchain identity.
    pub toolchain: ToolchainIdentity,
    /// Policy the release carries.
    pub policy: &'a Policy,
    /// Components to bind.
    pub components: Vec<ComponentInput>,
    /// Build time by the builder's clock.
    pub built_at_ms: u64,
}

/// A release operation that was refused.
#[derive(Debug, Error)]
pub enum ReleaseError {
    /// Trust enrollment or durable approval registration failed.
    #[error(transparent)]
    Trust(#[from] TrustError),
    /// The release signer failed.
    #[error(transparent)]
    Signer(#[from] SignerError),
    /// A filesystem operation failed.
    #[error("release I/O failed at '{path}': {source}")]
    Io {
        /// Path being operated on.
        path: String,
        /// Underlying failure.
        source: io::Error,
    },
    /// The source tree has modifications or untracked files.
    #[error("source tree is not clean: {0}")]
    DirtySource(String),
    /// A required tool is missing or failed.
    #[error("{tool} failed: {reason}")]
    Tool {
        /// The tool that failed.
        tool: String,
        /// What it reported.
        reason: String,
    },
    /// A component name is not one a release may install.
    #[error("'{0}' is not an installable component")]
    UnknownComponent(String),
    /// The manifest is unreadable or not this schema.
    #[error("release manifest is not usable: {0}")]
    Malformed(String),
    /// The bundle carries no signature.
    #[error("bundle is unsigned")]
    Unsigned,
    /// The signature does not authorize the manifest.
    #[error(transparent)]
    Signature(#[from] sshsig::SignatureError),
    /// No release role is enrolled.
    #[error("no key is enrolled for the release role")]
    NoReleaseRole,
    /// A component named by the manifest is absent from the bundle.
    #[error("component '{name}' is missing from the bundle")]
    ComponentMissing {
        /// The absent component.
        name: String,
    },
    /// A component's bytes are not what the manifest binds.
    #[error("component '{name}' hashes to {found}, manifest binds {expected}")]
    ComponentMismatch {
        /// The component that differs.
        name: String,
        /// Digest the manifest binds.
        expected: String,
        /// Digest the bytes actually have.
        found: String,
    },
    /// The manifest's own identity does not match its bytes.
    #[error("manifest claims {claimed} but its bytes are {actual}")]
    IdentityMismatch {
        /// Identity the manifest claims.
        claimed: String,
        /// Identity its bytes have.
        actual: String,
    },
}

/// Assembles a bundle at `bundle` from already-built components.
///
/// Building the components is the caller's job; this binds them. Keeping the
/// two apart is what lets the binding rules be tested without a nested build.
///
/// # Errors
/// Refuses dirty source identity or unknown executable components; returns bundle I/O, hashing, permissions, or manifest serialization errors.
pub fn assemble(
    request: &AssembleRequest<'_>,
    bundle: &Path,
) -> Result<ReleaseManifest, ReleaseError> {
    if !request.source.clean {
        return Err(ReleaseError::DirtySource(
            "source identity is not marked clean".to_owned(),
        ));
    }
    create_dir(bundle)?;
    create_dir(&bundle.join("bin"))?;
    create_dir(&bundle.join("policy"))?;
    create_dir(&bundle.join("schemas"))?;

    let mut components = Vec::new();
    for input in &request.components {
        let path = match input.kind {
            ComponentKind::Executable => {
                if !ALLOWED_COMPONENTS.contains(&input.name.as_str()) {
                    return Err(ReleaseError::UnknownComponent(crate::scan::escape(
                        &input.name,
                    )));
                }
                format!("bin/{}", input.name)
            }
            ComponentKind::Data => format!("data/{}", input.name),
        };
        let destination = bundle.join(&path);
        if let Some(parent) = destination.parent() {
            create_dir(parent)?;
        }
        copy(&input.path, &destination)?;
        let (sha256, size) = hash(&destination)?;
        let executable = input.kind == ComponentKind::Executable;
        set_mode(&destination, executable)?;
        components.push(Component {
            name: input.name.clone(),
            path,
            sha256: sha256.hex().to_owned(),
            size,
            executable,
        });
    }

    write(&bundle.join("policy/policy.json"), Policy::embedded_bytes())?;
    let schemas = schema_identifiers();
    write(
        &bundle.join("schemas/schemas.json"),
        &serde_json::to_vec(&schemas)
            .map_err(|error| ReleaseError::Malformed(error.to_string()))?,
    )?;
    for extra in [
        ("policy/policy.json", ComponentKind::Data),
        ("schemas/schemas.json", ComponentKind::Data),
    ] {
        let (sha256, size) = hash(&bundle.join(extra.0))?;
        components.push(Component {
            name: extra.0.to_owned(),
            path: extra.0.to_owned(),
            sha256: sha256.hex().to_owned(),
            size,
            executable: false,
        });
    }
    components.sort_by(|left, right| left.name.cmp(&right.name));

    let mut manifest = ReleaseManifest {
        schema: MANIFEST_SCHEMA.to_owned(),
        release_id: String::new(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        built_at_ms: request.built_at_ms,
        source: request.source.clone(),
        toolchain: request.toolchain.clone(),
        policy: PolicyIdentity {
            version: request.policy.document().version.clone(),
            digest: request.policy.digest().to_string(),
        },
        schemas,
        components,
    };
    manifest.release_id = manifest.digest().to_string();
    write(
        &bundle.join("manifest.json"),
        &serde_json::to_vec(&manifest)
            .map_err(|error| ReleaseError::Malformed(error.to_string()))?,
    )?;
    Ok(manifest)
}

/// Reads a bundle's manifest without verifying it.
///
/// # Errors
/// Returns file-read, malformed-JSON, or unsupported-schema errors.
pub fn read_manifest(bundle: &Path) -> Result<ReleaseManifest, ReleaseError> {
    read_manifest_bytes(bundle).map(|(manifest, _)| manifest)
}

fn read_manifest_bytes(bundle: &Path) -> Result<(ReleaseManifest, Vec<u8>), ReleaseError> {
    let path = bundle.join("manifest.json");
    let bytes = fs::read(&path).map_err(|source| ReleaseError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let manifest: ReleaseManifest = serde_json::from_slice(&bytes)
        .map_err(|error| ReleaseError::Malformed(error.to_string()))?;
    if manifest.schema != MANIFEST_SCHEMA {
        return Err(ReleaseError::Malformed(format!(
            "unsupported schema '{}'",
            crate::scan::escape(&manifest.schema)
        )));
    }
    Ok((manifest, bytes))
}

/// Signs and records an exact release approval, serialized with key retirement.
///
/// # Errors
/// Refuses missing enrollment, a busy store, malformed or changed bundles, and
/// signatures from non-current release keys. Propagates signer and persistence
/// failures; a failure may leave a signature file but never claims registration.
pub fn sign_bundle(store: &Store, bundle: &Path, signer: &dyn Signer) -> Result<(), ReleaseError> {
    let locked = LockedTrust::acquire(store)?;
    let mut trust = locked.load()?.ok_or(TrustError::NotBootstrapped)?;
    let (manifest, bytes) = read_manifest_bytes(bundle)?;
    verify_components(bundle, &manifest)?;
    let key = trust
        .key_for(Role::Release)
        .ok_or(ReleaseError::NoReleaseRole)?;
    let signature = signer.sign(RELEASE_NAMESPACE, &bytes)?;
    sshsig::verify(
        &signature,
        RELEASE_NAMESPACE,
        &bytes,
        &key.public_key,
        key.sk_policy,
    )?;
    write(&bundle.join("manifest.sig"), signature.as_bytes())?;
    trust.approved_releases.insert(manifest.release_id);
    locked.write(&trust)?;
    Ok(())
}

/// Verifies a bundle's signature and re-hashes every component it binds.
///
/// # Errors
/// Refuses inconsistent identity, missing/untrusted signatures, changed/missing components, or invalid component paths; propagates read and signature-verification errors.
pub fn verify_bundle(bundle: &Path, trust: &TrustStore) -> Result<ReleaseManifest, ReleaseError> {
    let (manifest, manifest_bytes) = read_manifest_bytes(bundle)?;
    verify_components(bundle, &manifest)?;

    let signature_path = bundle.join("manifest.sig");
    if !signature_path.is_file() {
        return Err(ReleaseError::Unsigned);
    }
    let signature = fs::read_to_string(&signature_path).map_err(|source| ReleaseError::Io {
        path: signature_path.display().to_string(),
        source,
    })?;
    let mut error = ReleaseError::NoReleaseRole;
    for key in trust.verification_keys(Role::Release, &manifest.release_id) {
        match sshsig::verify(
            &signature,
            RELEASE_NAMESPACE,
            &manifest_bytes,
            &key.public_key,
            key.sk_policy,
        ) {
            Ok(_) => return Ok(manifest),
            Err(reason) => error = ReleaseError::Signature(reason),
        }
    }
    Err(error)
}

fn verify_components(bundle: &Path, manifest: &ReleaseManifest) -> Result<(), ReleaseError> {
    let actual = manifest.digest();
    if manifest.release_id != actual.to_string() {
        return Err(ReleaseError::IdentityMismatch {
            claimed: manifest.release_id.clone(),
            actual: actual.to_string(),
        });
    }
    for component in &manifest.components {
        let path = bundle.join(&component.path);
        if !path.is_file() {
            return Err(ReleaseError::ComponentMissing {
                name: component.name.clone(),
            });
        }
        let (found, size) = hash(&path)?;
        if found.hex() != component.sha256 || size != component.size {
            return Err(ReleaseError::ComponentMismatch {
                name: component.name.clone(),
                expected: format!("sha256:{}", component.sha256),
                found: found.to_string(),
            });
        }
    }
    Ok(())
}

/// What the running executable is.
#[derive(Clone, Debug, Serialize)]
pub struct RunningIdentity {
    /// Whether this executable belongs to the current installed release.
    pub verified: bool,
    /// The release it belongs to, when it does.
    pub release_id: Option<String>,
    /// Path of the running executable.
    pub executable: Option<String>,
    /// Why it is not verified, when it is not.
    pub failure_code: Option<String>,
    /// What the failure means, in one sentence.
    pub detail: String,
}

/// Reports whether the running executable is part of a trusted release.
///
/// A development build — anything run from a cargo target directory or any
/// path that is not inside an installed release — is unverified, always. That
/// is the whole point: a build the Agent could have written must not be able
/// to claim otherwise.
#[must_use]
pub fn running_identity() -> RunningIdentity {
    match std::env::current_exe() {
        Ok(executable) => identity_of(&executable),
        Err(_) => RunningIdentity {
            verified: false,
            release_id: None,
            executable: None,
            failure_code: Some("executable_unknown".to_owned()),
            detail: "The running executable could not be located.".to_owned(),
        },
    }
}

/// Reports whether `executable` is part of a trusted, root-owned release.
///
/// Split from [`running_identity`] so the ownership rules can be exercised
/// against an install this process created, which is the only way an automated
/// test reaches them: every test runs as an ordinary user.
#[must_use]
pub fn identity_of(executable: &Path) -> RunningIdentity {
    let rendered = executable.display().to_string();
    let Some(release_root) = installed_release_root(executable) else {
        return RunningIdentity {
            verified: false,
            release_id: None,
            executable: Some(rendered),
            failure_code: Some("development_build".to_owned()),
            detail:
                "This executable is not inside an installed release; it can make no trusted claim."
                    .to_owned(),
        };
    };
    match verify_installed(&release_root, executable) {
        Ok(release_id) => RunningIdentity {
            verified: true,
            release_id: Some(release_id),
            executable: Some(rendered),
            failure_code: None,
            detail: "The running executable belongs to the current installed release.".to_owned(),
        },
        Err((code, detail)) => RunningIdentity {
            verified: false,
            release_id: None,
            executable: Some(rendered),
            failure_code: Some(code),
            detail,
        },
    }
}

fn installed_release_root(executable: &Path) -> Option<PathBuf> {
    // <prefix>/releases/<release-id>/bin/<component>
    let bin = executable.parent()?;
    let release_root = bin.parent()?;
    let releases = release_root.parent()?;
    (bin.file_name()? == "bin" && releases.file_name()? == "releases")
        .then(|| release_root.to_path_buf())
}

fn verify_installed(release_root: &Path, executable: &Path) -> Result<String, (String, String)> {
    let manifest = read_manifest(release_root).map_err(|error| {
        (
            "release_manifest_unreadable".to_owned(),
            format!("The installed release manifest is unusable: {error}"),
        )
    })?;
    let prefix = release_root
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| {
            (
                "prefix_unknown".to_owned(),
                "The install prefix could not be located.".to_owned(),
            )
        })?;
    let current = fs::read_link(prefix.join("current")).map_err(|error| {
        (
            "no_current_release".to_owned(),
            format!("The prefix names no current release: {error}"),
        )
    })?;
    if current != Path::new("releases").join(&manifest.release_id) {
        return Err((
            "not_current_release".to_owned(),
            "This executable belongs to a release that is not current.".to_owned(),
        ));
    }
    let name = executable
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let component = manifest.component(&name).ok_or_else(|| {
        (
            "component_not_in_release".to_owned(),
            "This executable is not a component of its release.".to_owned(),
        )
    })?;
    let (found, _) = hash(executable).map_err(|error| {
        (
            "executable_unreadable".to_owned(),
            format!("The running executable could not be re-hashed: {error}"),
        )
    })?;
    if found.hex() != component.sha256 {
        return Err((
            "executable_tampered".to_owned(),
            "The running executable does not match the bytes its release binds.".to_owned(),
        ));
    }
    if Policy::embedded().digest().to_string() != manifest.policy.digest {
        return Err((
            "policy_mismatch".to_owned(),
            "The compiled policy is not the policy this release binds.".to_owned(),
        ));
    }

    // Matching bytes are not a boundary if someone other than root can replace
    // them a moment later. A signed release installed somewhere the Agent can
    // write is exactly that: correctly signed, and worth nothing
    // (louiselm-jqj5).
    let ownership = crate::install::ownership_of(prefix);
    if !ownership.root_owned {
        return Err((
            "prefix_not_root_owned".to_owned(),
            format!(
                "The install prefix is owned by uid {}; a release its own Agent can rewrite makes no trusted claim.",
                ownership.prefix_uid
            ),
        ));
    }
    if ownership.world_writable {
        return Err((
            "prefix_world_writable".to_owned(),
            "The install prefix is writable beyond root, so its bytes are not fixed.".to_owned(),
        ));
    }
    Ok(manifest.release_id)
}

fn schema_identifiers() -> Vec<String> {
    [
        crate::manifest::MANIFEST_SCHEMA,
        crate::policy::POLICY_SCHEMA,
        crate::inspect::INSPECTION_SCHEMA,
        crate::dossier::DOSSIER_SCHEMA,
        crate::lineage::LINEAGE_SCHEMA,
        crate::assessment::ASSESSMENT_SCHEMA,
        crate::generation::GENERATION_SCHEMA,
        crate::generation::RECORD_SCHEMA,
        crate::trust::TRUST_SCHEMA,
        crate::trust::TRUST_CHANGE_SCHEMA,
        crate::trust::paper::PAPER_NAMESPACE,
        crate::quarantine::QUARANTINE_SCHEMA,
        crate::admission::STATUS_SCHEMA,
        crate::launch::REQUEST_SCHEMA,
        crate::launch_protocol::LAUNCH_AUTHORIZATION_SCHEMA,
        crate::launch_protocol::LIFECYCLE_REQUEST_SCHEMA,
        crate::launch_protocol::STATUS_REQUEST_SCHEMA,
        crate::launch_protocol::RECEIPT_ACK_SCHEMA,
        crate::launch_protocol::SUPERVISOR_STATUS_SCHEMA,
        crate::launch_protocol::SESSION_STATUS_SCHEMA,
        crate::launch_protocol::RESPONSE_SCHEMA,
        crate::launch_receipt::RECEIPT_SCHEMA,
        crate::launch_receipt::SIGNED_RECEIPT_SCHEMA,
        crate::launcher_install::CONFIG_SCHEMA,
        crate::launcher_install::KEYRING_SCHEMA,
        crate::launcher_install::STATUS_SCHEMA,
        MANIFEST_SCHEMA,
    ]
    .iter()
    .map(|schema| (*schema).to_owned())
    .collect()
}

pub(crate) fn hash(path: &Path) -> Result<(Digest, u64), ReleaseError> {
    use std::io::Read;

    let mut file = fs::File::open(path).map_err(|source| ReleaseError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let mut hasher = Hasher::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut size = 0_u64;
    loop {
        let read = file.read(&mut buffer).map_err(|source| ReleaseError::Io {
            path: path.display().to_string(),
            source,
        })?;
        if read == 0 {
            break;
        }
        size += read as u64;
        hasher.update(&buffer[..read]);
    }
    Ok((hasher.finish(), size))
}

pub(crate) fn create_dir(path: &Path) -> Result<(), ReleaseError> {
    fs::create_dir_all(path).map_err(|source| ReleaseError::Io {
        path: path.display().to_string(),
        source,
    })
}

pub(crate) fn write(path: &Path, bytes: &[u8]) -> Result<(), ReleaseError> {
    fs::write(path, bytes).map_err(|source| ReleaseError::Io {
        path: path.display().to_string(),
        source,
    })
}

pub(crate) fn copy(from: &Path, to: &Path) -> Result<(), ReleaseError> {
    fs::copy(from, to)
        .map(|_| ())
        .map_err(|source| ReleaseError::Io {
            path: from.display().to_string(),
            source,
        })
}

pub(crate) fn set_mode(path: &Path, executable: bool) -> Result<(), ReleaseError> {
    use std::os::unix::fs::PermissionsExt;

    let mode = if executable { 0o555 } else { 0o444 };
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|source| ReleaseError::Io {
        path: path.display().to_string(),
        source,
    })
}

fn git(repository: &Path, arguments: &[&str]) -> Result<String, ReleaseError> {
    let output = Command::new("git")
        .current_dir(repository)
        .args(arguments)
        .output()
        .map_err(|error| ReleaseError::Tool {
            tool: "git".to_owned(),
            reason: error.to_string(),
        })?;
    if !output.status.success() {
        return Err(ReleaseError::Tool {
            tool: "git".to_owned(),
            reason: crate::scan::escape(String::from_utf8_lossy(&output.stderr).trim()),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn tool_version(tool: &str) -> Result<String, ReleaseError> {
    let output = Command::new(tool)
        .arg("--version")
        .output()
        .map_err(|error| ReleaseError::Tool {
            tool: tool.to_owned(),
            reason: error.to_string(),
        })?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
