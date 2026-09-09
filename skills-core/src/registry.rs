//! The root-owned registries a launch request resolves against.
//!
//! A launch request carries identifiers and nothing else. Everything that
//! decides what actually runs — the command, its arguments, the runtime it
//! comes from, the capability envelope — lives here, in files the Agent cannot
//! write. That is the whole reason the request is a closed set of identifiers:
//! a caller who can name a command can run one.
//!
//! Runtimes are measured, not looked up. There is no `PATH` search and no
//! self-update: the executable is an exact path inside a registered runtime
//! directory, bound to the digest it had when it was registered. A Provider
//! runtime that rewrites itself changes that digest, and the next launch is
//! refused rather than silently running different bytes.

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fs::{self, File},
    io::{self, Read},
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{canonical::Digest, release};

/// The registry schema this build reads.
pub const REGISTRY_SCHEMA: &str = "louiselm.launch.registry/1";

/// Maximum encoded size of one registry document.
pub const MAX_REGISTRY_BYTES: usize = 4 * 1024 * 1024;

const MAX_REGISTRY_ENTRIES: usize = 4_096;
const MAX_IDENTIFIER_BYTES: usize = 128;

/// A file bound to the digest it had when it was registered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeasuredFile {
    /// Path relative to the runtime root.
    pub path: String,
    /// Digest the file had at registration.
    pub sha256: String,
}

/// A typed advertised option value used by an exact Provider route.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProviderOption {
    /// Exact string value, including the empty string.
    String(String),
    /// Exact boolean value; never coerced to a string.
    Boolean(bool),
}

/// One Provider selected when every named option matches its typed value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderRoute {
    /// Service supplying access or quota, independent of Model manufacturer.
    pub provider: String,
    /// Nonempty map of advertised option IDs to exact values.
    pub options: BTreeMap<String, ProviderOption>,
}

/// Provider configuration with the same serialized shape as the Lua Agent field.
///
/// [`Registry::open`] validates names and nonempty routes/maps. This records
/// possible services; resolving the active Provider remains a per-turn Lua duty.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum Provider {
    /// One fixed access/quota service.
    Fixed(String),
    /// Exact typed option routes; all possible services participate in disclosure.
    Routes(Vec<ProviderRoute>),
    /// Literal, case-sensitive prefixes of a string-valued advertised option.
    Prefixes {
        /// Nonblank advertised option ID.
        option: String,
        /// Nonempty prefixes mapped to nonblank service names.
        prefixes: BTreeMap<String, String>,
    },
}

impl Provider {
    fn validate(&self) -> Result<(), RegistryError> {
        // Lua's %S excludes these six ASCII whitespace bytes, not Unicode space.
        let nonblank = |value: &str| {
            value
                .bytes()
                .any(|byte| !matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c))
        };
        let valid = match self {
            Self::Fixed(service) => nonblank(service),
            Self::Routes(routes) => {
                !routes.is_empty()
                    && routes.iter().all(|route| {
                        nonblank(&route.provider)
                            && !route.options.is_empty()
                            && route.options.keys().all(|id| !id.is_empty())
                    })
            }
            Self::Prefixes { option, prefixes } => {
                nonblank(option)
                    && !prefixes.is_empty()
                    && prefixes
                        .iter()
                        .all(|(prefix, service)| !prefix.is_empty() && nonblank(service))
            }
        };
        if valid {
            Ok(())
        } else {
            Err(RegistryError::Malformed {
                kind: "agent".to_owned(),
                reason: "Provider needs nonblank service/option names and nonempty routes, maps and keys".to_owned(),
            })
        }
    }
}

/// A registered Agent: possible Providers, which runtime, and what it runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentRegistration {
    /// Identifier a request may name.
    pub id: String,
    /// Fixed service or routing configuration, matching the Lua Agent field.
    pub provider: Provider,
    /// The runtime package the Agent runs from.
    pub runtime_id: String,
    /// Arguments after the runtime executable, fixed at registration.
    pub arguments: Vec<String>,
    /// Environment the Session starts with; the launcher adds nothing else.
    pub environment: BTreeMap<String, String>,
}

impl AgentRegistration {
    /// Returns every configured access/quota service, sorted and deduplicated.
    ///
    /// Borrows exact names without trimming or resolving active options. Needs
    /// no running Agent or Session; overlapping routes still disclose all names.
    #[must_use]
    pub fn reachable_providers(&self) -> BTreeSet<&str> {
        match &self.provider {
            Provider::Fixed(service) => BTreeSet::from([service.as_str()]),
            Provider::Routes(routes) => {
                routes.iter().map(|route| route.provider.as_str()).collect()
            }
            Provider::Prefixes { prefixes, .. } => prefixes.values().map(String::as_str).collect(),
        }
    }
}

/// A measured Provider runtime.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimePackage {
    /// Identifier a request may name.
    pub id: String,
    /// Directory the runtime lives in; mounted read-only.
    pub root: PathBuf,
    /// Executable path, relative to `root`.
    pub executable: String,
    /// Digest the executable had at registration.
    pub executable_sha256: String,
    /// Adapter files that are part of the runtime's identity.
    pub adapters: Vec<MeasuredFile>,
    /// Runtime version, as the Provider reports it.
    pub version: String,
    /// Where the runtime came from.
    pub origin: String,
    /// Library baseline the runtime was registered against.
    pub library_baseline: Vec<String>,
    /// Isolation policy version this runtime was registered under.
    pub isolation_policy_version: String,
}

/// What a runtime measured to, right now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeMeasurement {
    /// Runtime identifier.
    pub runtime_id: String,
    /// Digest the executable has now.
    pub executable_sha256: String,
    /// Digests the adapters have now.
    pub adapters: Vec<MeasuredFile>,
    /// Version recorded at registration.
    pub version: String,
    /// Origin recorded at registration.
    pub origin: String,
    /// Library baseline recorded at registration.
    pub library_baseline: Vec<String>,
    /// Isolation policy version recorded at registration.
    pub isolation_policy_version: String,
}

/// Whether a Session may reach the network.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkPolicy {
    /// No network of any kind. The only value a verified launch accepts in v1.
    Denied,
    /// Brokered egress, which the control service owns (louiselm-qbr.5.1).
    Brokered,
}

/// A registered capability envelope.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvelopeRegistration {
    /// Identifier a request may name.
    pub id: String,
    /// Network policy this envelope grants.
    pub network: NetworkPolicy,
    /// What the envelope is for, in one sentence.
    pub description: String,
}

/// A registry lookup that failed.
#[derive(Debug, Error)]
pub enum RegistryError {
    /// A registry file could not be read.
    #[error("registry I/O failed at '{path}': {source}")]
    Io {
        /// Path being read.
        path: String,
        /// Underlying failure.
        source: io::Error,
    },
    /// A registry file is not usable.
    #[error("{kind} registry is malformed: {reason}")]
    Malformed {
        /// Which registry.
        kind: String,
        /// Why it is unusable.
        reason: String,
    },
    /// A registry document exceeds the fixed input bound.
    #[error("{kind} registry exceeds {MAX_REGISTRY_BYTES} bytes")]
    Oversized {
        /// Which registry.
        kind: &'static str,
    },
    /// A production registry or runtime path is not immutable authority.
    #[error("untrusted launch path '{path}': {reason}")]
    Untrusted {
        /// Path that failed the ownership boundary.
        path: String,
        /// Stable reason the path is not trusted.
        reason: &'static str,
    },
    /// Two entries claim the same identifier.
    #[error("duplicate {kind} identifier '{id}'")]
    Duplicate {
        /// Kind of entry that was duplicated.
        kind: &'static str,
        /// Duplicated identifier.
        id: String,
    },
    /// The request named something that is not registered.
    #[error("no {kind} is registered as '{id}'")]
    Unknown {
        /// Which registry was searched.
        kind: &'static str,
        /// The identifier that was not found.
        id: String,
    },
    /// A registered runtime no longer matches the bytes it was registered with.
    #[error(
        "runtime '{runtime_id}' changed since registration: {path} is {found}, registered {expected}"
    )]
    RuntimeChanged {
        /// The runtime that changed.
        runtime_id: String,
        /// Which file changed.
        path: String,
        /// Digest it had at registration.
        expected: String,
        /// Digest it has now.
        found: String,
    },
    /// A registered path is missing.
    #[error("runtime '{runtime_id}' is missing {path}")]
    RuntimeIncomplete {
        /// The incomplete runtime.
        runtime_id: String,
        /// The absent file.
        path: String,
    },
}

/// The registries a launch resolves against.
#[derive(Clone, Debug)]
pub struct Registry {
    root: PathBuf,
    agents: Vec<AgentRegistration>,
    runtimes: Vec<RuntimePackage>,
    envelopes: Vec<EnvelopeRegistration>,
}

impl Registry {
    /// Opens the registries under `root`.
    ///
    /// # Errors
    /// Returns registry I/O/JSON errors or invalid/duplicate registrations.
    pub fn open(root: &Path) -> Result<Self, RegistryError> {
        let registry = Self {
            root: root.to_path_buf(),
            agents: read_list(root, "agents")?,
            runtimes: read_list(root, "runtimes")?,
            envelopes: read_list(root, "envelopes")?,
        };
        registry.validate()?;
        Ok(registry)
    }

    /// Opens launch authority only when every path is root-owned and immutable
    /// by other users.
    ///
    /// [`Registry::open`] remains useful for unprivileged inspection and test
    /// fixtures. A privileged launcher must use this entrypoint: matching a
    /// digest does not stop an unprivileged owner from replacing the pathname
    /// between measurement and execution.
    /// The entire runtime tree is checked, including unlisted files, because
    /// the sandbox mounts that directory rather than only measured files.
    ///
    /// # Errors
    /// Returns registry-loading or runtime-enumeration errors, or refuses
    /// untrusted ownership, permissions, symlinks, or non-file/directory entries
    /// anywhere in the runtime trees or registry/runtime ancestry.
    pub fn open_trusted(root: &Path) -> Result<Self, RegistryError> {
        require_trusted_path(root, TrustedKind::Directory)?;
        for kind in ["agents", "runtimes", "envelopes"] {
            let path = root.join(format!("{kind}.json"));
            match fs::symlink_metadata(&path) {
                Ok(_) => require_trusted_path(&path, TrustedKind::File)?,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(RegistryError::Io {
                        path: path.display().to_string(),
                        source,
                    });
                }
            }
        }

        let registry = Self::open(root)?;
        for runtime in &registry.runtimes {
            require_trusted_tree(&runtime.root)?;
            require_trusted_path(&runtime.executable_path(), TrustedKind::File)?;
            for adapter in &runtime.adapters {
                require_trusted_path(&runtime.root.join(&adapter.path), TrustedKind::File)?;
            }
        }
        Ok(registry)
    }

    /// Returns the directory the registries were read from.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolves a registered Agent.
    ///
    /// # Errors
    /// Returns `Unknown` when the Agent identifier is not registered.
    pub fn agent(&self, id: &str) -> Result<AgentRegistration, RegistryError> {
        self.agents
            .iter()
            .find(|agent| agent.id == id)
            .cloned()
            .ok_or_else(|| RegistryError::Unknown {
                kind: "agent",
                id: id.to_owned(),
            })
    }

    /// Resolves a registered runtime package.
    ///
    /// # Errors
    /// Returns `Unknown` when the runtime identifier is not registered.
    pub fn runtime(&self, id: &str) -> Result<RuntimePackage, RegistryError> {
        self.runtimes
            .iter()
            .find(|runtime| runtime.id == id)
            .cloned()
            .ok_or_else(|| RegistryError::Unknown {
                kind: "runtime",
                id: id.to_owned(),
            })
    }

    /// Resolves a registered capability envelope.
    ///
    /// # Errors
    /// Returns `Unknown` when the envelope identifier is not registered.
    pub fn envelope(&self, id: &str) -> Result<EnvelopeRegistration, RegistryError> {
        self.envelopes
            .iter()
            .find(|envelope| envelope.id == id)
            .cloned()
            .ok_or_else(|| RegistryError::Unknown {
                kind: "envelope",
                id: id.to_owned(),
            })
    }

    /// Lists every registered Agent identifier.
    #[must_use]
    pub fn agent_ids(&self) -> Vec<String> {
        self.agents.iter().map(|agent| agent.id.clone()).collect()
    }

    fn validate(&self) -> Result<(), RegistryError> {
        validate_unique_ids("agent", self.agents.iter().map(|entry| &entry.id))?;
        validate_unique_ids("runtime", self.runtimes.iter().map(|entry| &entry.id))?;
        validate_unique_ids("envelope", self.envelopes.iter().map(|entry| &entry.id))?;

        for agent in &self.agents {
            validate_identifier("agent", &agent.id)?;
            validate_identifier("runtime", &agent.runtime_id)?;
            agent.provider.validate()?;
        }
        for envelope in &self.envelopes {
            validate_identifier("envelope", &envelope.id)?;
        }
        for runtime in &self.runtimes {
            validate_identifier("runtime", &runtime.id)?;
            validate_absolute_path("runtime", &runtime.root)?;
            validate_relative_path("runtime", &runtime.executable)?;
            validate_digest("runtime", &runtime.executable_sha256)?;

            let mut paths = HashSet::new();
            paths.insert(runtime.executable.as_str());
            for adapter in &runtime.adapters {
                validate_relative_path("runtime", &adapter.path)?;
                validate_digest("runtime", &adapter.sha256)?;
                if !paths.insert(adapter.path.as_str()) {
                    return Err(RegistryError::Malformed {
                        kind: "runtime".to_owned(),
                        reason: format!("duplicate measured path '{}'", adapter.path),
                    });
                }
            }
        }
        Ok(())
    }
}

impl RuntimePackage {
    /// Returns the absolute path of the runtime executable.
    #[must_use]
    pub fn executable_path(&self) -> PathBuf {
        self.root.join(&self.executable)
    }

    /// Re-hashes the runtime and refuses anything that changed.
    ///
    /// This is where "no self-update" is enforced. A Provider runtime that
    /// rewrote itself between registration and launch is not the runtime that
    /// was registered, whatever its version string still says.
    ///
    /// # Errors
    /// Refuses missing or changed executable/adapter files; propagates file-hashing errors.
    pub fn measure(&self) -> Result<RuntimeMeasurement, RegistryError> {
        let executable = self.executable_path();
        let found = self.hash(&executable, &self.executable)?;
        if found != self.executable_sha256 {
            return Err(RegistryError::RuntimeChanged {
                runtime_id: self.id.clone(),
                path: self.executable.clone(),
                expected: self.executable_sha256.clone(),
                found,
            });
        }
        let mut adapters = Vec::with_capacity(self.adapters.len());
        for adapter in &self.adapters {
            let found = self.hash(&self.root.join(&adapter.path), &adapter.path)?;
            if found != adapter.sha256 {
                return Err(RegistryError::RuntimeChanged {
                    runtime_id: self.id.clone(),
                    path: adapter.path.clone(),
                    expected: adapter.sha256.clone(),
                    found,
                });
            }
            adapters.push(MeasuredFile {
                path: adapter.path.clone(),
                sha256: found,
            });
        }
        Ok(RuntimeMeasurement {
            runtime_id: self.id.clone(),
            executable_sha256: self.executable_sha256.clone(),
            adapters,
            version: self.version.clone(),
            origin: self.origin.clone(),
            library_baseline: self.library_baseline.clone(),
            isolation_policy_version: self.isolation_policy_version.clone(),
        })
    }

    fn hash(&self, path: &Path, relative: &str) -> Result<String, RegistryError> {
        if !path.is_file() {
            return Err(RegistryError::RuntimeIncomplete {
                runtime_id: self.id.clone(),
                path: relative.to_owned(),
            });
        }
        let (digest, _) = release::hash(path).map_err(|error| RegistryError::Io {
            path: path.display().to_string(),
            source: io::Error::other(error.to_string()),
        })?;
        Ok(digest.hex().to_owned())
    }
}

/// Computes the digest a runtime file should be registered with.
///
/// # Errors
/// Returns a file-open/read error when the digest cannot be computed.
pub fn measure_file(path: &Path) -> Result<Digest, RegistryError> {
    release::hash(path)
        .map(|(digest, _)| digest)
        .map_err(|error| RegistryError::Io {
            path: path.display().to_string(),
            source: io::Error::other(error.to_string()),
        })
}

fn read_list<T: for<'de> Deserialize<'de>>(
    root: &Path,
    kind: &'static str,
) -> Result<Vec<T>, RegistryError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Document<T> {
        schema: String,
        entries: Vec<T>,
    }

    let path = root.join(format!("{kind}.json"));
    let mut file = match File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(RegistryError::Io {
                path: path.display().to_string(),
                source,
            });
        }
    };
    let mut bytes = Vec::new();
    file.by_ref()
        .take((MAX_REGISTRY_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|source| RegistryError::Io {
            path: path.display().to_string(),
            source,
        })?;
    if bytes.len() > MAX_REGISTRY_BYTES {
        return Err(RegistryError::Oversized { kind });
    }
    let document: Document<T> =
        serde_json::from_slice(&bytes).map_err(|error| RegistryError::Malformed {
            kind: kind.to_owned(),
            reason: error.to_string(),
        })?;
    if document.schema != REGISTRY_SCHEMA {
        return Err(RegistryError::Malformed {
            kind: kind.to_owned(),
            reason: format!(
                "unsupported schema '{}'",
                crate::scan::escape(&document.schema)
            ),
        });
    }
    if document.entries.len() > MAX_REGISTRY_ENTRIES {
        return Err(RegistryError::Malformed {
            kind: kind.to_owned(),
            reason: format!("must contain at most {MAX_REGISTRY_ENTRIES} entries"),
        });
    }
    Ok(document.entries)
}

fn validate_unique_ids<'a>(
    kind: &'static str,
    ids: impl Iterator<Item = &'a String>,
) -> Result<(), RegistryError> {
    let mut seen = HashSet::new();
    for id in ids {
        if !seen.insert(id) {
            return Err(RegistryError::Duplicate {
                kind,
                id: id.clone(),
            });
        }
    }
    Ok(())
}

fn validate_identifier(kind: &'static str, value: &str) -> Result<(), RegistryError> {
    if value.is_empty()
        || value.len() > MAX_IDENTIFIER_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(RegistryError::Malformed {
            kind: kind.to_owned(),
            reason: format!("invalid identifier '{}'", crate::scan::escape(value)),
        });
    }
    Ok(())
}

fn validate_digest(kind: &'static str, value: &str) -> Result<(), RegistryError> {
    let parsed = Digest::parse(value).map_err(|error| RegistryError::Malformed {
        kind: kind.to_owned(),
        reason: error.to_string(),
    })?;
    if parsed.hex() != value {
        return Err(RegistryError::Malformed {
            kind: kind.to_owned(),
            reason: "digests must be bare lowercase sha256 hex".to_owned(),
        });
    }
    Ok(())
}

fn validate_absolute_path(kind: &'static str, path: &Path) -> Result<(), RegistryError> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return Err(RegistryError::Malformed {
            kind: kind.to_owned(),
            reason: format!(
                "runtime root '{}' must be an absolute normalized path",
                path.display()
            ),
        });
    }
    Ok(())
}

fn validate_relative_path(kind: &'static str, raw: &str) -> Result<(), RegistryError> {
    let path = Path::new(raw);
    if raw.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(RegistryError::Malformed {
            kind: kind.to_owned(),
            reason: format!(
                "'{}' must be a normalized relative path",
                crate::scan::escape(raw)
            ),
        });
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum TrustedKind {
    Directory,
    File,
}

fn require_trusted_tree(root: &Path) -> Result<(), RegistryError> {
    require_trusted_path(root, TrustedKind::Directory)?;
    let mut directories = vec![root.to_path_buf()];
    while let Some(directory) = directories.pop() {
        // Each directory and its ancestors were validated before enumeration;
        // an unprivileged writer cannot replace the entries being inspected.
        let directory_error = |source| RegistryError::Io {
            path: directory.display().to_string(),
            source,
        };
        for entry in fs::read_dir(&directory).map_err(&directory_error)? {
            let path = entry.map_err(&directory_error)?.path();
            let metadata = fs::symlink_metadata(&path).map_err(|source| RegistryError::Io {
                path: path.display().to_string(),
                source,
            })?;
            let kind = if metadata.is_dir() {
                TrustedKind::Directory
            } else {
                TrustedKind::File
            };
            require_trusted_metadata(&path, &metadata, kind)?;
            if metadata.is_dir() {
                directories.push(path);
            }
        }
    }
    Ok(())
}

fn require_trusted_path(path: &Path, kind: TrustedKind) -> Result<(), RegistryError> {
    validate_absolute_path("launch path", path)?;

    let mut current = PathBuf::from("/");
    let components: Vec<_> = path.components().collect();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            continue;
        };
        current.push(name);
        let metadata = fs::symlink_metadata(&current).map_err(|source| RegistryError::Io {
            path: current.display().to_string(),
            source,
        })?;
        let final_component = index + 1 == components.len();
        let expected_type = if final_component {
            kind
        } else {
            TrustedKind::Directory
        };
        require_trusted_metadata(&current, &metadata, expected_type)?;
    }
    Ok(())
}

fn require_trusted_metadata(
    path: &Path,
    metadata: &fs::Metadata,
    kind: TrustedKind,
) -> Result<(), RegistryError> {
    let right_type = match kind {
        TrustedKind::Directory => metadata.is_dir(),
        TrustedKind::File => metadata.is_file(),
    };
    let reason = if metadata.file_type().is_symlink() || !right_type {
        "must not contain symlinks or unexpected file types"
    } else if metadata.uid() != 0 {
        "must be owned by root"
    } else if metadata.mode() & 0o022 != 0 {
        "must not be writable by group or other users"
    } else {
        return Ok(());
    };
    Err(RegistryError::Untrusted {
        path: path.display().to_string(),
        reason,
    })
}
