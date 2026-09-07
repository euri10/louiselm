//! Public recovery readiness, without credentials, verifiers or secret material.

use serde::Serialize;

use super::{
    Role, TrustStore,
    recovery::{self, RecoveryError},
    terminal,
};
use crate::{canonical::Digest, store::Store};

/// Closed public status schema included in release compatibility metadata.
pub const STATUS_SCHEMA: &str = "louiselm.skills.recovery-status/1";

/// Public readiness is distinct from installed authority and Verified posture.
#[derive(Debug, Serialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Independent readiness facts are deliberately reported separately."
)]
pub struct Status {
    /// Public response schema.
    pub schema: &'static str,
    /// Domain of the enrolled authority, when initialized.
    pub trust_domain: Option<String>,
    /// Current complete trust snapshot fingerprint, for comparing confirmations.
    pub snapshot: Option<String>,
    /// Installed executable and root-owned protected production store were checked.
    pub protected_authority: bool,
    /// Public store provenance; alone this is not an authority check.
    pub production_store: bool,
    /// Primary public-key fingerprint, never private key material.
    pub primary: Option<String>,
    /// Distinct release public-key fingerprint.
    pub release: Option<String>,
    /// Current signing roles both require hardware presence and verification.
    pub hardware_policy: bool,
    /// A confirmed paper verifier exists; the verifier itself is not returned.
    pub paper_enrolled: bool,
    /// A recovery credential exists; the credential itself is not returned.
    pub passkey_enrolled: bool,
    /// Public recovery credential fingerprint.
    pub passkey_fingerprint: Option<String>,
    /// Authenticator-reported backup state; not an audit of a password manager.
    pub passkey_backed_up: bool,
    /// All required enrollment facts and the protected installed boundary hold.
    pub recovery_ready: bool,
    /// Concrete next action; never an instruction to promote a development store.
    pub next_action: &'static str,
}

/// Reads current public readiness. Does not open a terminal or request secrets.
///
/// # Errors
/// Propagates malformed/unreadable trust and provenance or credential metadata.
pub fn read(store: &Store) -> Result<Status, RecoveryError> {
    let trust = TrustStore::load(store)?;
    let production_store = store.provenance()?.trusted;
    let protected_authority = terminal::require_production_root(store).is_ok();
    let fingerprint = |role| {
        trust
            .as_ref()
            .and_then(|trust| trust.key_for(role))
            .map(|key| Digest::of(key.public_key.as_bytes()).to_string())
    };
    let primary = fingerprint(Role::Primary);
    let release = fingerprint(Role::Release);
    let hardware_policy = trust
        .as_ref()
        .is_some_and(|trust| recovery::require_hardware_policy(trust).is_ok());
    let paper_enrolled = trust
        .as_ref()
        .is_some_and(|trust| trust.paper_verifier.is_some());
    let passkey = trust.as_ref().and_then(|trust| trust.passkey.as_ref());
    let passkey_backed_up = passkey
        .map(super::passkey::EnrolledPasskey::backed_up)
        .transpose()?
        .unwrap_or(false);
    let recovery_ready =
        protected_authority && hardware_policy && paper_enrolled && passkey_backed_up;
    let next_action = if !production_store {
        "install_signed_release_and_setup_fresh_store"
    } else if !protected_authority {
        "use_installed_tool_with_protected_store"
    } else if trust.is_none() {
        "recovery_setup"
    } else if !hardware_policy {
        "recovery_reset_and_setup"
    } else if !paper_enrolled || !passkey_backed_up {
        "enroll_missing_recovery_methods"
    } else {
        "none"
    };
    Ok(Status {
        schema: STATUS_SCHEMA,
        trust_domain: trust.as_ref().map(|trust| trust.trust_domain.clone()),
        snapshot: trust.as_ref().map(|trust| trust.digest().to_string()),
        protected_authority,
        production_store,
        primary,
        release,
        hardware_policy,
        paper_enrolled,
        passkey_enrolled: passkey.is_some(),
        passkey_fingerprint: passkey
            .map(super::passkey::EnrolledPasskey::fingerprint)
            .transpose()?,
        passkey_backed_up,
        recovery_ready,
        next_action,
    })
}
