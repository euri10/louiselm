//! Normalized, fail-closed Verified posture.
//!
//! Evidence enters through typed constructors in this trusted crate. None of
//! the evidence or posture types implement `Deserialize`: Agent claims and
//! arbitrary JSON may be displayed by an adapter after validation, but they
//! cannot become authority for a launch decision.

use std::collections::BTreeMap;

use serde::Serialize;
use thiserror::Error;

use crate::dossier::NextAction;

/// The Verified posture schema this build emits.
pub const POSTURE_SCHEMA: &str = "louiselm.verified-posture/1";

/// The disclosure that always accompanies a cloud Provider posture.
pub const PROVIDER_DISCLOSURE_NOTICE: &str = "Plaintext intentionally sent to a cloud Provider is visible to that Provider despite local containment.";

/// Fixed distinction between runtime instructions and admitted supply.
pub const EMBEDDED_INSTRUCTIONS_NOTICE: &str = "Instructions embedded in the measured executable are part of runtime trust, not admitted Skill supply.";

const MAX_IDENTIFIER_BYTES: usize = 256;

/// One independently evaluated posture dimension.
#[derive(Clone, Copy, Debug, Ord, PartialEq, Eq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DimensionName {
    /// Admitted LouiseLM-managed skill supply.
    ManagedSupply,
    /// Provider-native instructions and configuration.
    NativeSupply,
    /// Measured Agent and adapter runtime.
    Runtime,
    /// Filesystem, process, identity, and IPC confinement.
    Isolation,
    /// Brokered network authority.
    Network,
    /// Plaintext disclosed to the selected cloud Provider.
    ProviderDisclosure,
}

impl DimensionName {
    /// Every required dimension, in presentation order.
    pub const ALL: [Self; 6] = [
        Self::ManagedSupply,
        Self::NativeSupply,
        Self::Runtime,
        Self::Isolation,
        Self::Network,
        Self::ProviderDisclosure,
    ];

    /// Stable robot-facing name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::ManagedSupply => "managed_supply",
            Self::NativeSupply => "native_supply",
            Self::Runtime => "runtime",
            Self::Isolation => "isolation",
            Self::Network => "network",
            Self::ProviderDisclosure => "provider_disclosure",
        }
    }

    const fn requirement(self) -> Requirement {
        match self {
            Self::ManagedSupply => Requirement::WitnessedGeneration,
            Self::NativeSupply => Requirement::NativeSourcesControlled,
            Self::Runtime => Requirement::MeasuredRuntime,
            Self::Isolation => Requirement::IsolationContract,
            Self::Network => Requirement::BrokeredNetwork,
            Self::ProviderDisclosure => Requirement::CloudPlaintextDisclosure,
        }
    }

    const fn accepts_evidence(self, kind: EvidenceKind) -> bool {
        match self {
            Self::ManagedSupply => matches!(kind, EvidenceKind::SkillGeneration),
            Self::NativeSupply | Self::ProviderDisclosure => {
                matches!(kind, EvidenceKind::SessionInputManifest)
            }
            Self::Runtime => matches!(
                kind,
                EvidenceKind::RuntimeMeasurement | EvidenceKind::ReleaseManifest
            ),
            Self::Isolation => matches!(kind, EvidenceKind::IsolationReceipt),
            Self::Network => matches!(
                kind,
                EvidenceKind::CapabilityEnvelope | EvidenceKind::BrokerReceipt
            ),
        }
    }

    const fn accepts_failure(self, code: FailureCode) -> bool {
        if matches!(
            code,
            FailureCode::EvidenceMissing
                | FailureCode::AuditPersistenceUnavailable
                | FailureCode::UnknownFailure
        ) {
            return true;
        }
        match self {
            Self::ManagedSupply => matches!(
                code,
                FailureCode::RootTrustFailed
                    | FailureCode::SignatureInvalid
                    | FailureCode::WitnessMissing
            ),
            Self::NativeSupply => matches!(code, FailureCode::NativeSupplyUncertain),
            Self::Runtime => matches!(
                code,
                FailureCode::RootTrustFailed
                    | FailureCode::SignatureInvalid
                    | FailureCode::RuntimeDrift
            ),
            Self::Isolation => matches!(code, FailureCode::IsolationFailed),
            Self::Network => matches!(code, FailureCode::BrokerUnavailable),
            Self::ProviderDisclosure => {
                matches!(code, FailureCode::ProviderDisclosureMissing)
            }
        }
    }
}

/// What policy requires from one dimension.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Requirement {
    /// Managed supply must come from a witnessed Skill Generation.
    WitnessedGeneration,
    /// Every Provider-native instruction source must be measured or masked.
    NativeSourcesControlled,
    /// Runtime bytes must match a registered measurement.
    MeasuredRuntime,
    /// The versioned isolation contract must be satisfied.
    IsolationContract,
    /// Network access must be mediated by the authenticated broker.
    BrokeredNetwork,
    /// Cloud Provider plaintext visibility must be disclosed.
    CloudPlaintextDisclosure,
}

impl Requirement {
    /// Stable robot-facing name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::WitnessedGeneration => "witnessed_generation",
            Self::NativeSourcesControlled => "native_sources_controlled",
            Self::MeasuredRuntime => "measured_runtime",
            Self::IsolationContract => "isolation_contract",
            Self::BrokeredNetwork => "brokered_network",
            Self::CloudPlaintextDisclosure => "cloud_plaintext_disclosure",
        }
    }
}

/// Trusted component that produced an opaque evidence identifier.
#[derive(Clone, Copy, Debug, Ord, PartialEq, Eq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    /// A hardware-authorized, witnessed Skill Generation.
    SkillGeneration,
    /// The complete Session-input manifest.
    SessionInputManifest,
    /// A measured registered runtime.
    RuntimeMeasurement,
    /// A signed root-owned release manifest.
    ReleaseManifest,
    /// A successful isolation receipt.
    IsolationReceipt,
    /// The exact capability envelope revision.
    CapabilityEnvelope,
    /// An authenticated control-broker receipt.
    BrokerReceipt,
    /// A durable audit receipt.
    AuditReceipt,
    /// A bounded interactive waiver receipt.
    WaiverReceipt,
}

impl EvidenceKind {
    /// Stable robot-facing name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::SkillGeneration => "skill_generation",
            Self::SessionInputManifest => "session_input_manifest",
            Self::RuntimeMeasurement => "runtime_measurement",
            Self::ReleaseManifest => "release_manifest",
            Self::IsolationReceipt => "isolation_receipt",
            Self::CapabilityEnvelope => "capability_envelope",
            Self::BrokerReceipt => "broker_receipt",
            Self::AuditReceipt => "audit_receipt",
            Self::WaiverReceipt => "waiver_receipt",
        }
    }
}

/// Bounded pointer to trusted evidence; never raw evidence or hostile text.
#[derive(Clone, Debug, Ord, PartialEq, Eq, PartialOrd, Serialize)]
pub struct EvidenceRef {
    /// Producing trusted component.
    pub kind: EvidenceKind,
    /// Opaque digest or receipt identifier.
    pub id: String,
}

impl EvidenceRef {
    /// Validate and construct one evidence pointer.
    ///
    /// # Errors
    ///
    /// Rejects empty, oversized, path-shaped, or non-ASCII identifiers.
    pub fn new(kind: EvidenceKind, id: &str) -> Result<Self, PostureError> {
        validate_identifier("evidence id", id)?;
        Ok(Self {
            kind,
            id: id.to_owned(),
        })
    }
}

/// Stable reason one dimension does not satisfy policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureCode {
    /// The trusted release is absent or not root-owned.
    RootTrustFailed,
    /// Trusted bytes do not have a valid signature.
    SignatureInvalid,
    /// A signed Skill Generation is not remotely witnessed.
    WitnessMissing,
    /// Provider-native instruction sources were not completely measured or masked.
    NativeSupplyUncertain,
    /// Runtime bytes no longer match their registered measurement.
    RuntimeDrift,
    /// Isolation evidence is absent, contradictory, or failed.
    IsolationFailed,
    /// The authenticated local control broker is unavailable.
    BrokerUnavailable,
    /// Durable audit persistence is unavailable.
    AuditPersistenceUnavailable,
    /// Provider disclosure is absent or incomplete.
    ProviderDisclosureMissing,
    /// Required trusted evidence is absent.
    EvidenceMissing,
    /// A failure has no recognized typed diagnosis.
    UnknownFailure,
}

impl FailureCode {
    /// Stable robot-facing name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::RootTrustFailed => "root_trust_failed",
            Self::SignatureInvalid => "signature_invalid",
            Self::WitnessMissing => "witness_missing",
            Self::NativeSupplyUncertain => "native_supply_uncertain",
            Self::RuntimeDrift => "runtime_drift",
            Self::IsolationFailed => "isolation_failed",
            Self::BrokerUnavailable => "broker_unavailable",
            Self::AuditPersistenceUnavailable => "audit_persistence_unavailable",
            Self::ProviderDisclosureMissing => "provider_disclosure_missing",
            Self::EvidenceMissing => "evidence_missing",
            Self::UnknownFailure => "unknown_failure",
        }
    }
}

/// Result of evaluating one dimension.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DimensionState {
    /// Trusted evidence satisfies the requirement.
    Verified,
    /// The requirement is not satisfied.
    Failed,
    /// An interactive waiver applies to this exact failure.
    Waived,
}

impl DimensionState {
    /// Stable robot-facing name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Failed => "failed",
            Self::Waived => "waived",
        }
    }
}

/// Aggregate posture state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PostureState {
    /// Every required dimension has trusted evidence.
    FullyVerified,
    /// At least one required dimension failed.
    Unverified,
    /// No dimension failed, but at least one is waived.
    Waived,
}

impl PostureState {
    /// Stable robot-facing name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::FullyVerified => "fully_verified",
            Self::Unverified => "unverified",
            Self::Waived => "waived",
        }
    }
}

/// Trusted input for one dimension.
///
/// Constructors deliberately derive state and failure shape rather than
/// accepting a caller-built serialized record.
#[derive(Clone, Debug)]
pub struct DimensionInput {
    dimension: DimensionName,
    state: DimensionState,
    evidence: Vec<EvidenceRef>,
    failure_code: Option<FailureCode>,
}

impl DimensionInput {
    /// State that trusted evidence satisfies one dimension.
    #[must_use]
    pub fn verified(dimension: DimensionName, evidence: Vec<EvidenceRef>) -> Self {
        Self {
            dimension,
            state: DimensionState::Verified,
            evidence,
            failure_code: None,
        }
    }

    /// State one typed failure.
    #[must_use]
    pub fn failed(
        dimension: DimensionName,
        failure_code: FailureCode,
        evidence: Vec<EvidenceRef>,
    ) -> Self {
        Self {
            dimension,
            state: DimensionState::Failed,
            evidence,
            failure_code: Some(failure_code),
        }
    }

    /// State one exact waived failure, referencing its validated receipt.
    #[must_use]
    pub fn waived(
        dimension: DimensionName,
        failure_code: FailureCode,
        mut evidence: Vec<EvidenceRef>,
        waiver_receipt: EvidenceRef,
    ) -> Self {
        evidence.push(waiver_receipt);
        Self {
            dimension,
            state: DimensionState::Waived,
            evidence,
            failure_code: Some(failure_code),
        }
    }
}

/// Normalized result for one dimension.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DimensionPosture {
    /// Whether trusted evidence satisfies the requirement.
    pub state: DimensionState,
    /// Whether this is an enforced control or a required disclosure.
    pub requirement: Requirement,
    /// Bounded identifiers for supporting evidence.
    pub evidence: Vec<EvidenceRef>,
    /// Stable failure code when not verified.
    pub failure_code: Option<FailureCode>,
    /// One fixed safe next action.
    pub next_action: NextAction,
}

/// The six fixed posture dimensions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PostureDimensions {
    /// Admitted managed skill supply.
    pub managed_supply: DimensionPosture,
    /// Controlled Provider-native supply.
    pub native_supply: DimensionPosture,
    /// Measured trusted runtime.
    pub runtime: DimensionPosture,
    /// Enforced isolation contract.
    pub isolation: DimensionPosture,
    /// Brokered network authority.
    pub network: DimensionPosture,
    /// Truthful cloud Provider disclosure.
    pub provider_disclosure: DimensionPosture,
}

impl PostureDimensions {
    /// Dimensions in canonical presentation order.
    #[must_use]
    pub fn ordered(&self) -> [(DimensionName, &DimensionPosture); 6] {
        [
            (DimensionName::ManagedSupply, &self.managed_supply),
            (DimensionName::NativeSupply, &self.native_supply),
            (DimensionName::Runtime, &self.runtime),
            (DimensionName::Isolation, &self.isolation),
            (DimensionName::Network, &self.network),
            (DimensionName::ProviderDisclosure, &self.provider_disclosure),
        ]
    }
}

/// One Session's normalized Verified posture.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Posture {
    /// Schema identifier.
    pub schema: String,
    /// Session this posture describes.
    pub session_id: String,
    /// Run that owns the Session.
    pub run_id: String,
    /// Aggregate fail-closed state.
    pub state: PostureState,
    /// Fixed cloud disclosure boundary.
    pub provider_disclosure_notice: String,
    /// Fixed disclosure of executable-embedded instructions as runtime trust.
    pub embedded_instructions_notice: String,
    /// Independently evaluated dimensions.
    pub dimensions: PostureDimensions,
}

impl Posture {
    /// Evaluate exactly one input for each required dimension.
    ///
    /// This is a trusted-code boundary: input types cannot be deserialized and
    /// contain only typed evidence identifiers, not Agent-authored claims.
    ///
    /// # Errors
    ///
    /// Rejects malformed subject identifiers, missing or duplicate
    /// dimensions, mismatched evidence, and contradictory failure codes.
    pub fn evaluate(
        session_id: &str,
        run_id: &str,
        inputs: Vec<DimensionInput>,
    ) -> Result<Self, PostureError> {
        validate_identifier("session_id", session_id)?;
        validate_identifier("run_id", run_id)?;

        let mut by_dimension = BTreeMap::new();
        for input in inputs {
            let dimension = input.dimension;
            if by_dimension.insert(dimension, input).is_some() {
                return Err(PostureError::DuplicateDimension(dimension));
            }
        }
        for dimension in DimensionName::ALL {
            if !by_dimension.contains_key(&dimension) {
                return Err(PostureError::MissingDimension(dimension));
            }
        }

        let mut take = |dimension| {
            evaluate_dimension(
                by_dimension
                    .remove(&dimension)
                    .ok_or(PostureError::MissingDimension(dimension))?,
            )
        };
        let dimensions = PostureDimensions {
            managed_supply: take(DimensionName::ManagedSupply)?,
            native_supply: take(DimensionName::NativeSupply)?,
            runtime: take(DimensionName::Runtime)?,
            isolation: take(DimensionName::Isolation)?,
            network: take(DimensionName::Network)?,
            provider_disclosure: take(DimensionName::ProviderDisclosure)?,
        };
        let state = aggregate_state(&dimensions);

        Ok(Self {
            schema: POSTURE_SCHEMA.to_owned(),
            session_id: session_id.to_owned(),
            run_id: run_id.to_owned(),
            state,
            provider_disclosure_notice: PROVIDER_DISCLOSURE_NOTICE.to_owned(),
            embedded_instructions_notice: EMBEDDED_INSTRUCTIONS_NOTICE.to_owned(),
            dimensions,
        })
    }

    /// Whether all six required dimensions are verified without a waiver.
    #[must_use]
    pub const fn is_fully_verified(&self) -> bool {
        matches!(self.state, PostureState::FullyVerified)
    }
}

/// A malformed or contradictory posture input.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PostureError {
    /// A subject or evidence identifier was not bounded and opaque.
    #[error("invalid {field}: {reason}")]
    Identifier {
        /// Field that failed validation.
        field: &'static str,
        /// Fixed validation reason.
        reason: &'static str,
    },
    /// The same dimension was supplied more than once.
    #[error("duplicate posture dimension {0:?}")]
    DuplicateDimension(DimensionName),
    /// A required dimension was absent.
    #[error("missing posture dimension {0:?}")]
    MissingDimension(DimensionName),
    /// A verified dimension had no primary evidence.
    #[error("verified posture dimension {0:?} has no trusted evidence")]
    EvidenceRequired(DimensionName),
    /// Evidence came from a component that cannot establish this dimension.
    #[error("evidence kind {kind:?} cannot establish posture dimension {dimension:?}")]
    EvidenceKind {
        /// Dimension being evaluated.
        dimension: DimensionName,
        /// Incompatible evidence kind.
        kind: EvidenceKind,
    },
    /// A failure code was attached to an unrelated dimension.
    #[error("failure code {code:?} does not apply to posture dimension {dimension:?}")]
    FailureCode {
        /// Dimension being evaluated.
        dimension: DimensionName,
        /// Incompatible failure code.
        code: FailureCode,
    },
    /// A waived dimension did not carry a waiver receipt.
    #[error("waived posture dimension {0:?} has no waiver receipt")]
    WaiverReceipt(DimensionName),
}

fn evaluate_dimension(mut input: DimensionInput) -> Result<DimensionPosture, PostureError> {
    input.evidence.sort();
    input.evidence.dedup();

    let has_waiver = input
        .evidence
        .iter()
        .any(|evidence| evidence.kind == EvidenceKind::WaiverReceipt);
    if input.state == DimensionState::Waived && !has_waiver {
        return Err(PostureError::WaiverReceipt(input.dimension));
    }
    for evidence in &input.evidence {
        if evidence.kind == EvidenceKind::WaiverReceipt {
            if input.state != DimensionState::Waived {
                return Err(PostureError::EvidenceKind {
                    dimension: input.dimension,
                    kind: evidence.kind,
                });
            }
        } else if evidence.kind != EvidenceKind::AuditReceipt
            && !input.dimension.accepts_evidence(evidence.kind)
        {
            return Err(PostureError::EvidenceKind {
                dimension: input.dimension,
                kind: evidence.kind,
            });
        }
    }

    if input.state == DimensionState::Verified
        && !input
            .evidence
            .iter()
            .any(|evidence| input.dimension.accepts_evidence(evidence.kind))
    {
        return Err(PostureError::EvidenceRequired(input.dimension));
    }
    if let Some(code) = input.failure_code
        && !input.dimension.accepts_failure(code)
    {
        return Err(PostureError::FailureCode {
            dimension: input.dimension,
            code,
        });
    }

    Ok(DimensionPosture {
        state: input.state,
        requirement: input.dimension.requirement(),
        evidence: input.evidence,
        failure_code: input.failure_code,
        next_action: next_action(input.failure_code),
    })
}

fn aggregate_state(dimensions: &PostureDimensions) -> PostureState {
    let states = dimensions
        .ordered()
        .map(|(_, dimension)| dimension.state)
        .into_iter();
    if states.clone().any(|state| state == DimensionState::Failed) {
        PostureState::Unverified
    } else if states.clone().any(|state| state == DimensionState::Waived) {
        PostureState::Waived
    } else {
        PostureState::FullyVerified
    }
}

fn next_action(failure: Option<FailureCode>) -> NextAction {
    let (id, detail) = match failure {
        None => ("none", "No action is required."),
        Some(FailureCode::RootTrustFailed) => (
            "install_trusted_release",
            "Install and run the root-owned signed LouiseLM release.",
        ),
        Some(FailureCode::SignatureInvalid) => (
            "restore_trusted_signature",
            "Reinstall or readmit bytes with a valid trusted signature.",
        ),
        Some(FailureCode::WitnessMissing) => (
            "publish_generation_witness",
            "Publish the signed Skill Generation to the protected witness before launch.",
        ),
        Some(FailureCode::NativeSupplyUncertain) => (
            "mask_or_measure_native_supply",
            "Measure or mask every native Provider instruction source before launch.",
        ),
        Some(FailureCode::RuntimeDrift) => (
            "restage_runtime",
            "Restage the registered runtime from trusted release bytes.",
        ),
        Some(FailureCode::IsolationFailed) => (
            "repair_isolation",
            "Repair the isolation boundary and rerun hostile conformance checks.",
        ),
        Some(FailureCode::BrokerUnavailable) => (
            "restore_control_broker",
            "Restore the authenticated control broker before granting network authority.",
        ),
        Some(FailureCode::AuditPersistenceUnavailable) => (
            "restore_audit_persistence",
            "Restore durable audit persistence before launch or privileged effects.",
        ),
        Some(FailureCode::ProviderDisclosureMissing) => (
            "record_provider_disclosure",
            "Record the cloud Provider plaintext disclosure before launch.",
        ),
        Some(FailureCode::EvidenceMissing) => (
            "collect_trusted_evidence",
            "Collect trusted evidence for this dimension before launch.",
        ),
        Some(FailureCode::UnknownFailure) => (
            "inspect_unknown_failure",
            "Inspect the unknown failure and add a typed trusted diagnosis.",
        ),
    };
    NextAction {
        id: id.to_owned(),
        detail: detail.to_owned(),
    }
}

fn validate_identifier(field: &'static str, value: &str) -> Result<(), PostureError> {
    if value.is_empty() {
        return Err(PostureError::Identifier {
            field,
            reason: "must not be empty",
        });
    }
    if value.len() > MAX_IDENTIFIER_BYTES {
        return Err(PostureError::Identifier {
            field,
            reason: "must be at most 256 bytes",
        });
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err(PostureError::Identifier {
            field,
            reason: "must be an opaque ASCII identifier",
        });
    }
    Ok(())
}
