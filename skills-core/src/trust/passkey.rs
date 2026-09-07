//! Synced-capable `WebAuthn` recovery proofs. Pending challenges never leave memory.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use webauthn_rs::prelude::*;

use super::{
    TrustStore,
    recovery::{RecoveryChange, RecoveryError},
};
use crate::canonical::Digest;

/// Fixed local relying party; capture/pairing services cannot choose trust roots.
pub const RP_ID: &str = "localhost";
/// Maximum lifetime of a registration or exact-change approval.
pub const TIMEOUT: Duration = Duration::from_mins(5);

/// Persisted public credential, including verifier-owned counter/backup metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnrolledPasskey {
    rp_id: String,
    user_id: Uuid,
    // Passkey's PartialEq compares only IDs. JSON equality also compares key and
    // counter state, which must participate in the complete trust snapshot.
    credential: serde_json::Value,
}

impl EnrolledPasskey {
    fn key(&self) -> Result<Passkey, RecoveryError> {
        if self.rp_id != RP_ID {
            return Err(RecoveryError::Passkey);
        }
        serde_json::from_value(self.credential.clone()).map_err(|_| RecoveryError::Passkey)
    }

    pub(crate) fn validate(&self) -> Result<(), RecoveryError> {
        self.key().map(|_| ())
    }

    /// Public credential fingerprint for operator review and retirement checks.
    ///
    /// # Errors
    /// Refuses malformed stored verifier data.
    pub fn fingerprint(&self) -> Result<String, RecoveryError> {
        Ok(Digest::of(self.key()?.cred_id().as_ref()).to_string())
    }

    /// Whether the verified authenticator reports eligible and completed backup.
    /// This is authenticator-reported metadata, not proof of provider durability.
    ///
    /// # Errors
    /// Refuses malformed verifier-owned credential metadata.
    pub fn backed_up(&self) -> Result<bool, RecoveryError> {
        self.validate()?;
        // webauthn-rs 0.5.5 exposes these fields in its supported persisted Passkey
        // schema, but not as getters. Refuse missing fields; never infer backup.
        let eligible = self
            .credential
            .pointer("/cred/backup_eligible")
            .and_then(serde_json::Value::as_bool)
            .ok_or(RecoveryError::Passkey)?;
        let backed_up = self
            .credential
            .pointer("/cred/backup_state")
            .and_then(serde_json::Value::as_bool)
            .ok_or(RecoveryError::Passkey)?;
        Ok(eligible && backed_up)
    }
}

/// Verified registration candidate, not authority to enroll itself.
pub struct Registration {
    credential: EnrolledPasskey,
    predecessor: String,
    deadline: Instant,
}

impl Registration {
    pub(crate) fn deadline(&self) -> Instant {
        self.deadline
    }

    /// Public candidate to include in the separately authorized change.
    #[must_use]
    pub fn credential(&self) -> &EnrolledPasskey {
        &self.credential
    }

    pub(crate) fn check(&self, change: &RecoveryChange) -> Result<(), RecoveryError> {
        if Instant::now() >= self.deadline
            || self.predecessor != change.predecessor
            || change.next_passkey.as_ref() != Some(&self.credential)
        {
            return Err(RecoveryError::Passkey);
        }
        Ok(())
    }
}

/// One-use registration challenge, tied to the complete current trust snapshot.
pub struct PendingRegistration {
    verifier: Webauthn,
    state: Option<PasskeyRegistration>,
    user_id: Uuid,
    predecessor: String,
    deadline: Instant,
}

impl PendingRegistration {
    /// Issues a fresh challenge for this ephemeral loopback listener's port.
    ///
    /// # Errors
    /// Propagates verifier or randomness failures; refuses malformed enrollment.
    pub fn start(
        trust: &TrustStore,
        port: u16,
    ) -> Result<(Self, CreationChallengeResponse), RecoveryError> {
        let verifier = verifier(port)?;
        let user_id = Uuid::new_v4();
        let exclude = trust
            .passkey
            .as_ref()
            .map(|key| key.key().map(|key| vec![key.cred_id().clone()]))
            .transpose()?;
        let (options, state) = verifier
            .start_passkey_registration(user_id, &trust.trust_domain, "LouiseLM recovery", exclude)
            .map_err(|_| RecoveryError::Passkey)?;
        Ok((
            Self {
                verifier,
                state: Some(state),
                user_id,
                predecessor: trust.digest().to_string(),
                deadline: Instant::now() + TIMEOUT,
            },
            options,
        ))
    }

    /// Consumes this attempt, even on invalid input. Does not modify authority.
    ///
    /// # Errors
    /// Refuses stale, expired, replayed, cross-origin or unverified registrations.
    pub fn finish(
        &mut self,
        trust: &TrustStore,
        response: &RegisterPublicKeyCredential,
    ) -> Result<Registration, RecoveryError> {
        let state = self.state.take().ok_or(RecoveryError::Passkey)?;
        if Instant::now() >= self.deadline || trust.digest().to_string() != self.predecessor {
            return Err(RecoveryError::Passkey);
        }
        let key = self
            .verifier
            .finish_passkey_registration(response, &state)
            .map_err(|_| RecoveryError::Passkey)?;
        let credential = EnrolledPasskey {
            rp_id: RP_ID.to_owned(),
            user_id: self.user_id,
            credential: serde_json::to_value(key).map_err(|_| RecoveryError::Passkey)?,
        };
        let fingerprint = credential.fingerprint()?;
        if trust.retired_passkeys.contains(&fingerprint)
            || trust
                .passkey
                .as_ref()
                .map(EnrolledPasskey::fingerprint)
                .transpose()?
                .as_ref()
                == Some(&fingerprint)
        {
            return Err(RecoveryError::Passkey);
        }
        Ok(Registration {
            credential,
            predecessor: self.predecessor.clone(),
            deadline: self.deadline,
        })
    }
}

/// One-use authentication challenge bound to the exact public recovery change.
pub struct PendingAuthentication {
    verifier: Webauthn,
    state: Option<PasskeyAuthentication>,
    change: Vec<u8>,
    credential: EnrolledPasskey,
    deadline: Instant,
}

/// Verified, expiring exact-change approval; cannot be serialized or fabricated.
pub struct Approval {
    change: Vec<u8>,
    credential: EnrolledPasskey,
    updated: EnrolledPasskey,
    deadline: Instant,
}

impl PendingAuthentication {
    /// Issues a user-verification-required assertion challenge for the current passkey.
    ///
    /// # Errors
    /// Refuses absent/malformed credentials, stale plans or verifier failures.
    pub fn start(
        trust: &TrustStore,
        change: &RecoveryChange,
        port: u16,
    ) -> Result<(Self, RequestChallengeResponse), RecoveryError> {
        super::recovery::validate(trust, change)?;
        let credential = trust.passkey.clone().ok_or(RecoveryError::Passkey)?;
        let verifier = verifier(port)?;
        let (options, state) = verifier
            .start_passkey_authentication(&[credential.key()?])
            .map_err(|_| RecoveryError::Passkey)?;
        Ok((
            Self {
                verifier,
                state: Some(state),
                change: change.canonical_bytes(),
                credential,
                deadline: Instant::now() + TIMEOUT,
            },
            options,
        ))
    }

    /// Consumes this attempt and verifies a real assertion, without writing trust.
    ///
    /// # Errors
    /// Refuses expiration, replay, wrong user/credential/challenge/origin or missing UV.
    pub fn finish(&mut self, response: &PublicKeyCredential) -> Result<Approval, RecoveryError> {
        let state = self.state.take().ok_or(RecoveryError::Passkey)?;
        if Instant::now() >= self.deadline
            || response
                .response
                .user_handle
                .as_ref()
                .is_some_and(|handle| handle.as_ref() != self.credential.user_id.as_bytes())
        {
            return Err(RecoveryError::Passkey);
        }
        let result = self
            .verifier
            .finish_passkey_authentication(response, &state)
            .map_err(|_| RecoveryError::Passkey)?;
        if !result.user_verified() {
            return Err(RecoveryError::Passkey);
        }
        let mut key = self.credential.key()?;
        key.update_credential(&result)
            .ok_or(RecoveryError::Passkey)?;
        let mut updated = self.credential.clone();
        updated.credential = serde_json::to_value(key).map_err(|_| RecoveryError::Passkey)?;
        Ok(Approval {
            change: self.change.clone(),
            credential: self.credential.clone(),
            updated,
            deadline: self.deadline,
        })
    }
}

impl Approval {
    pub(crate) fn deadline(&self) -> Instant {
        self.deadline
    }

    pub(crate) fn authorize(
        self,
        trust: &TrustStore,
        change: &RecoveryChange,
    ) -> Result<EnrolledPasskey, RecoveryError> {
        if Instant::now() >= self.deadline
            || self.change != change.canonical_bytes()
            || trust.passkey.as_ref() != Some(&self.credential)
        {
            return Err(RecoveryError::Passkey);
        }
        Ok(self.updated)
    }
}

fn verifier(port: u16) -> Result<Webauthn, RecoveryError> {
    if port == 0 {
        return Err(RecoveryError::Passkey);
    }
    let origin =
        Url::parse(&format!("http://localhost:{port}")).map_err(|_| RecoveryError::Passkey)?;
    WebauthnBuilder::new(RP_ID, &origin)
        .map_err(|_| RecoveryError::Passkey)?
        .rp_name("LouiseLM recovery")
        .timeout(TIMEOUT)
        .build()
        .map_err(|_| RecoveryError::Passkey)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        reason = "Expired opaque-token binding fixtures, independent of cryptographic verification tests."
    )]
    use super::super::{paper::PaperPhrase, recovery::RecoveryChange};
    use super::*;

    #[test]
    fn verified_tokens_expire_even_when_all_public_bindings_still_match() {
        let directory = tempfile::tempdir().unwrap();
        let store = crate::Store::open(directory.path()).unwrap();
        let mut trust = TrustStore::bootstrap(
            &store,
            "test/expiry",
            "fixture-primary",
            "fixture-recovery",
            crate::sshsig::SkPolicy::none(),
            0,
        )
        .unwrap();
        // These unit fixtures exercise only the opaque token's deadline check.
        // Public API integration tests obtain tokens solely through real WebAuthn.
        let credential = EnrolledPasskey {
            rp_id: RP_ID.to_owned(),
            user_id: Uuid::new_v4(),
            credential: serde_json::Value::Null,
        };
        trust.passkey = Some(credential.clone());
        let paper = PaperPhrase::generate().unwrap();
        let mut change = RecoveryChange::new(&trust, vec![], Some(&paper)).unwrap();
        change.next_passkey = Some(credential.clone());
        let mut registration = Registration {
            credential: credential.clone(),
            predecessor: change.predecessor.clone(),
            deadline: Instant::now(),
        };
        assert!(registration.check(&change).is_err());
        registration.deadline = Instant::now() + TIMEOUT;
        assert!(registration.check(&change).is_ok());
        let approval = |deadline| Approval {
            change: change.canonical_bytes(),
            credential: credential.clone(),
            updated: credential.clone(),
            deadline,
        };
        assert!(approval(Instant::now()).authorize(&trust, &change).is_err());
        assert!(
            approval(Instant::now() + TIMEOUT)
                .authorize(&trust, &change)
                .is_ok()
        );
    }
}
