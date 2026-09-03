//! The versioned, Provider-neutral isolation contract.
//!
//! An adapter must not get to decide what "sandboxed" means for its Provider,
//! and a backend must not be trusted for its name. The contract names the
//! dimensions every backend has to cover — bubblewrap, systemd, Landlock, or
//! anything else — and a launch is verified only when there is exactly one
//! piece of satisfied evidence per dimension, from a backend whose kernel
//! prerequisites are actually present.
//!
//! Three ways to fail, all closed:
//!
//! * **Missing** — a dimension nobody answered. Silence is not a pass.
//! * **Unsatisfied** — a dimension a backend admits it cannot cover.
//! * **Contradictory** — two answers that disagree. Picking one would mean
//!   choosing which part of the sandbox to believe.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The isolation contract version this build implements.
pub const CONTRACT_VERSION: &str = "louiselm.isolation/1";

/// One property a confined Session must have.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dimension {
    /// What the Session can see of the filesystem.
    FilesystemVisibility,
    /// What the Session can change on the filesystem.
    FilesystemMutation,
    /// Whether the Session's processes are separated from everything else.
    ProcessSeparation,
    /// What the Session inherits from the process that launched it.
    ProcessInheritance,
    /// Which sockets and IPC endpoints the Session can reach.
    IpcAccess,
    /// Whether the Session can reach the network.
    NetworkDenial,
    /// Whether the Session runs as an identity of its own.
    Identity,
    /// Whether the whole process tree can be frozen and disposed of.
    Lifecycle,
    /// Whether the backend produced evidence at all.
    Evidence,
}

impl Dimension {
    /// Every dimension the contract requires, in reporting order.
    pub const ALL: [Dimension; 9] = [
        Dimension::FilesystemVisibility,
        Dimension::FilesystemMutation,
        Dimension::ProcessSeparation,
        Dimension::ProcessInheritance,
        Dimension::IpcAccess,
        Dimension::NetworkDenial,
        Dimension::Identity,
        Dimension::Lifecycle,
        Dimension::Evidence,
    ];

    /// Returns the name used in evidence and robot output.
    pub fn name(self) -> &'static str {
        match self {
            Self::FilesystemVisibility => "filesystem_visibility",
            Self::FilesystemMutation => "filesystem_mutation",
            Self::ProcessSeparation => "process_separation",
            Self::ProcessInheritance => "process_inheritance",
            Self::IpcAccess => "ipc_access",
            Self::NetworkDenial => "network_denial",
            Self::Identity => "identity",
            Self::Lifecycle => "lifecycle",
            Self::Evidence => "evidence",
        }
    }

    /// Returns what a backend has to establish for this dimension.
    pub fn requirement(self) -> &'static str {
        match self {
            Self::FilesystemVisibility => {
                "The Session sees only its measured runtime, its private home, and its workspace."
            }
            Self::FilesystemMutation => {
                "Everything the Session can see is read-only except its private home and workspace."
            }
            Self::ProcessSeparation => {
                "The Session cannot see, signal, or trace any process outside itself."
            }
            Self::ProcessInheritance => {
                "The Session inherits no descriptor, environment, or session from the launcher."
            }
            Self::IpcAccess => {
                "The Session reaches no socket except the channels the launcher created."
            }
            Self::NetworkDenial => "The Session has no network of its own and no route out.",
            Self::Identity => "The Session runs under an identity distinct from the operator.",
            Self::Lifecycle => {
                "The whole process tree can be frozen and disposed of, with no survivors."
            }
            Self::Evidence => "The backend reported what it actually did, not what it intended.",
        }
    }
}

/// What one backend established for one dimension.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DimensionEvidence {
    /// The dimension this answers.
    pub dimension: Dimension,
    /// Whether the backend established it.
    pub satisfied: bool,
    /// The mechanism used, named concretely (`pid namespace`, `cgroup.freeze`).
    pub mechanism: String,
    /// What was actually done or observed.
    pub detail: String,
}

/// Kernel features a backend depends on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KernelPrerequisites {
    /// Whether unprivileged user namespaces are available.
    pub user_namespaces: bool,
    /// Whether PID namespaces are available.
    pub pid_namespaces: bool,
    /// Whether network namespaces are available.
    pub network_namespaces: bool,
    /// Whether a writable cgroup v2 hierarchy is available.
    pub cgroup_v2: bool,
    /// Anything else worth recording, as free text already escaped.
    pub details: Vec<String>,
}

impl KernelPrerequisites {
    /// Names every prerequisite that is absent.
    pub fn missing(&self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if !self.user_namespaces {
            missing.push("user namespaces");
        }
        if !self.pid_namespaces {
            missing.push("pid namespaces");
        }
        if !self.network_namespaces {
            missing.push("network namespaces");
        }
        if !self.cgroup_v2 {
            missing.push("cgroup v2");
        }
        missing
    }
}

/// Everything one backend established for one Session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IsolationEvidence {
    /// Contract version this evidence answers.
    pub contract_version: String,
    /// Backend that produced it.
    pub backend: String,
    /// Backend version.
    pub backend_version: String,
    /// Kernel features the backend found.
    pub kernel: KernelPrerequisites,
    /// One entry per dimension.
    pub dimensions: Vec<DimensionEvidence>,
}

/// Why isolation evidence cannot support a verified launch.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum IsolationFailure {
    /// The evidence answers a different contract version.
    #[error("evidence answers contract '{found}', not '{expected}'")]
    ContractVersion {
        /// Version the evidence carries.
        found: String,
        /// Version required here.
        expected: String,
    },
    /// A kernel prerequisite is absent.
    #[error("kernel prerequisites are missing: {0:?}")]
    KernelPrerequisite(Vec<String>),
    /// One or more dimensions have no evidence.
    #[error("no evidence for {0:?}")]
    Missing(Vec<Dimension>),
    /// One or more dimensions were reported unsatisfied.
    #[error("unsatisfied: {0:?}")]
    Unsatisfied(Vec<Dimension>),
    /// One or more dimensions have disagreeing evidence.
    #[error("contradictory evidence for {0:?}")]
    Contradictory(Vec<Dimension>),
}

impl IsolationEvidence {
    /// Reports whether the evidence supports a verified launch.
    ///
    /// Checks run in a fixed order — contract, kernel, contradictions,
    /// omissions, admissions — so a caller always learns the most fundamental
    /// reason rather than whichever happened to be noticed first.
    pub fn check(&self) -> Result<(), IsolationFailure> {
        if self.contract_version != CONTRACT_VERSION {
            return Err(IsolationFailure::ContractVersion {
                found: self.contract_version.clone(),
                expected: CONTRACT_VERSION.to_owned(),
            });
        }
        let missing_kernel = self.kernel.missing();
        if !missing_kernel.is_empty() {
            return Err(IsolationFailure::KernelPrerequisite(
                missing_kernel.into_iter().map(str::to_owned).collect(),
            ));
        }

        let mut contradictory = Vec::new();
        let mut missing = Vec::new();
        let mut unsatisfied = Vec::new();
        for dimension in Dimension::ALL {
            let answers = self
                .dimensions
                .iter()
                .filter(|evidence| evidence.dimension == dimension)
                .collect::<Vec<_>>();
            match answers.as_slice() {
                [] => missing.push(dimension),
                [only] => {
                    if !only.satisfied {
                        unsatisfied.push(dimension);
                    }
                }
                many => {
                    if many
                        .iter()
                        .any(|evidence| evidence.satisfied != many[0].satisfied)
                    {
                        contradictory.push(dimension);
                    } else if !many[0].satisfied {
                        unsatisfied.push(dimension);
                    }
                }
            }
        }
        if !contradictory.is_empty() {
            return Err(IsolationFailure::Contradictory(contradictory));
        }
        if !missing.is_empty() {
            return Err(IsolationFailure::Missing(missing));
        }
        if !unsatisfied.is_empty() {
            return Err(IsolationFailure::Unsatisfied(unsatisfied));
        }
        Ok(())
    }

    /// Returns the dimensions this evidence establishes.
    pub fn satisfied_dimensions(&self) -> Vec<Dimension> {
        Dimension::ALL
            .iter()
            .copied()
            .filter(|dimension| {
                let answers = self
                    .dimensions
                    .iter()
                    .filter(|evidence| evidence.dimension == *dimension)
                    .collect::<Vec<_>>();
                !answers.is_empty() && answers.iter().all(|evidence| evidence.satisfied)
            })
            .collect()
    }
}
