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
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{canonical::Digest, release};

/// The registry schema this build reads.
pub const REGISTRY_SCHEMA: &str = "louiselm.launch.registry/1";

/// A file bound to the digest it had when it was registered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeasuredFile {
    /// Path relative to the runtime root.
    pub path: String,
    /// Digest the file had at registration.
    pub sha256: String,
}

/// A registered Agent: which Provider, which runtime, and what it runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentRegistration {
    /// Identifier a request may name.
    pub id: String,
    /// The Provider behind the Agent.
    pub provider: String,
    /// The runtime package the Agent runs from.
    pub runtime_id: String,
    /// Arguments after the runtime executable, fixed at registration.
    pub arguments: Vec<String>,
    /// Environment the Session starts with; the launcher adds nothing else.
    pub environment: BTreeMap<String, String>,
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
    pub fn open(root: &Path) -> Result<Self, RegistryError> {
        Ok(Self {
            root: root.to_path_buf(),
            agents: read_list(root, "agents")?,
            runtimes: read_list(root, "runtimes")?,
            envelopes: read_list(root, "envelopes")?,
        })
    }

    /// Returns the directory the registries were read from.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolves a registered Agent.
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
    pub fn agent_ids(&self) -> Vec<String> {
        self.agents.iter().map(|agent| agent.id.clone()).collect()
    }
}

impl RuntimePackage {
    /// Returns the absolute path of the runtime executable.
    pub fn executable_path(&self) -> PathBuf {
        self.root.join(&self.executable)
    }

    /// Re-hashes the runtime and refuses anything that changed.
    ///
    /// This is where "no self-update" is enforced. A Provider runtime that
    /// rewrote itself between registration and launch is not the runtime that
    /// was registered, whatever its version string still says.
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
    kind: &str,
) -> Result<Vec<T>, RegistryError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Document<T> {
        schema: String,
        entries: Vec<T>,
    }

    let path = root.join(format!("{kind}.json"));
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(RegistryError::Io {
                path: path.display().to_string(),
                source,
            });
        }
    };
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
    Ok(document.entries)
}
