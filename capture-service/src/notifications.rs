//! Pairing-owned push registration, retry policy, and durable submission state.

use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;

use super::{PairingError, PairingRegistry, RegistryState, hash};

/// Closed push-delivery condition; no provider text or secrets are retained.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationHealth {
    /// No service-account credential is configured; no Google requests occur.
    #[default]
    Unconfigured,
    /// The sender may attempt due deliveries.
    Ready,
    /// Correct the service-account configuration and explicitly retry.
    ConfigurationError,
    /// Correct sender authorization and explicitly retry.
    AuthenticationError,
}

/// Sanitized outcome of one bounded provider submission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotificationFailure {
    /// Retry with exponential backoff, respecting the provider's minimum delay.
    Transient {
        /// Minimum delay in milliseconds; zero uses the local backoff alone.
        retry_after_ms: u64,
    },
    /// This registration token is no longer usable; other devices may continue.
    InvalidToken,
    /// Sender authorization requires operator intervention.
    Authentication,
    /// Sender configuration or a malformed response requires intervention.
    Configuration,
}

/// Public per-device progress, excluding the registration token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct NotificationTargetStatus {
    /// Public pairing identifier.
    pub device_id: String,
    /// Whether this registered token can receive future submissions.
    pub enabled: bool,
    /// Last generation confirmed accepted by FCM, not confirmed read on Android.
    pub submitted_generation: Option<u64>,
    /// Earliest next attempt after a transient or interrupted submission.
    pub retry_at_ms: u64,
    /// Consecutive transient or interrupted attempts.
    pub attempts: u32,
}

/// Structured local push health, safe for status output.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct NotificationStatus {
    /// Version of this bounded status schema.
    pub schema_version: u8,
    /// Sender configuration/authorization state.
    pub health: NotificationHealth,
    /// Fixed operator action, when intervention is necessary.
    pub next_action: Option<&'static str>,
    /// Registered devices with durable delivery progress.
    pub devices: Vec<NotificationTargetStatus>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NotificationState {
    pub health: NotificationHealth,
}

// Do not derive Debug: this is a push credential, not a diagnostic record.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NotificationRegistration {
    token: String,
    enabled: bool,
    submitted_generation: Option<u64>,
    retry_at_ms: u64,
    attempts: u32,
}

fn valid_token(token: &str) -> bool {
    !token.is_empty() && token.len() <= 4096 && token.bytes().all(|byte| byte.is_ascii_graphic())
}

pub(super) fn validate_state(state: &RegistryState) -> Result<(), PairingError> {
    let mut tokens = std::collections::BTreeSet::new();
    for registration in state
        .devices
        .iter()
        .filter_map(|device| device.notification.as_ref())
    {
        if !valid_token(&registration.token)
            || !tokens.insert(&registration.token)
            || registration.submitted_generation == Some(0)
            || (registration.attempts == 0) != (registration.retry_at_ms == 0)
        {
            return Err(PairingError::Rejected(
                "stored notification state is invalid".to_owned(),
            ));
        }
    }
    Ok(())
}

impl PairingRegistry {
    /// Register or rotate only the device authenticated by this bearer.
    ///
    /// This blocking transaction rechecks authority under the same lock as revocation.
    /// Repeating the same token preserves progress and does not enable a disabled token.
    ///
    /// # Errors
    /// Rejects invalid credentials, duplicate device tokens, malformed input, and storage failures.
    pub fn register_notification_token(
        &self,
        credential: &str,
        token: &str,
    ) -> Result<(), PairingError> {
        if !valid_token(token) {
            return Err(PairingError::Rejected(
                "notification token is invalid".to_owned(),
            ));
        }
        self.with_state(true, |state| {
            let credential_hash = hash(credential);
            let index = state
                .devices
                .iter()
                .position(|device| {
                    bool::from(
                        device
                            .credential_sha256
                            .as_bytes()
                            .ct_eq(credential_hash.as_bytes()),
                    )
                })
                .ok_or(PairingError::Unauthorized)?;
            if state.devices.iter().enumerate().any(|(other, device)| {
                other != index
                    && device
                        .notification
                        .as_ref()
                        .is_some_and(|registration| registration.token == token)
            }) {
                return Err(PairingError::Rejected(
                    "notification token is unavailable".to_owned(),
                ));
            }
            let current = &mut state.devices[index].notification;
            if current
                .as_ref()
                .is_some_and(|registration| registration.token == token)
            {
                return Ok(());
            }
            // Rotation preserves the device's confirmed generation: a refreshed token
            // does not turn an unchanged unresolved condition into a reminder.
            let submitted_generation = current
                .as_ref()
                .and_then(|registration| registration.submitted_generation);
            *current = Some(NotificationRegistration {
                token: token.to_owned(),
                enabled: true,
                submitted_generation,
                retry_at_ms: 0,
                attempts: 0,
            });
            Ok(())
        })
    }

    /// Return sender and per-device status without credential material.
    ///
    /// # Errors
    /// Returns registry lock, parsing, and storage failures.
    pub fn notification_status(&self) -> Result<NotificationStatus, PairingError> {
        self.with_state(false, |state| {
            let mut devices = state.devices.iter().filter_map(|device| {
                device.notification.as_ref().map(|registration| NotificationTargetStatus {
                    device_id: device.status.device_id.clone(), enabled: registration.enabled,
                    submitted_generation: registration.submitted_generation,
                    retry_at_ms: registration.retry_at_ms, attempts: registration.attempts,
                })
            }).collect::<Vec<_>>();
            devices.sort_by(|left, right| left.device_id.cmp(&right.device_id));
            Ok(NotificationStatus {
                schema_version: 1,
                health: state.notifications.health,
                next_action: match state.notifications.health {
                    NotificationHealth::Unconfigured => Some("configure GOOGLE_APPLICATION_CREDENTIALS and restart serve"),
                    NotificationHealth::Ready => None,
                    NotificationHealth::ConfigurationError | NotificationHealth::AuthenticationError =>
                        Some("correct sender credentials/permissions, restart serve, then run retry-notifications"),
                }, devices,
            })
        })
    }

    /// Record startup configuration without silently clearing a previous permanent failure.
    ///
    /// # Errors
    /// Returns registry persistence failures.
    pub fn configure_notifications(&self, health: NotificationHealth) -> Result<(), PairingError> {
        self.with_state(false, |state| {
            if state.notifications.health != health
                && (health != NotificationHealth::Ready
                    || state.notifications.health == NotificationHealth::Unconfigured)
            {
                state.notifications.health = health;
                self.persist(state)?;
            }
            Ok(())
        })
    }

    /// Explicitly retry after correcting sender configuration or authorization.
    ///
    /// # Errors
    /// Rejects an unconfigured sender and returns persistence failures.
    pub fn retry_notifications(&self) -> Result<(), PairingError> {
        self.with_state(true, |state| {
            if state.notifications.health == NotificationHealth::Unconfigured {
                return Err(PairingError::Rejected(
                    "notifications are unconfigured".to_owned(),
                ));
            }
            state.notifications.health = NotificationHealth::Ready;
            Ok(())
        })
    }

    /// Submit the latest eligible generation independently to each active device.
    ///
    /// Blocking: call off the async executor. The callback must have a bounded timeout
    /// and must not re-enter the pairing registry.
    /// Each submission holds the pairing lock so revocation/rotation cannot overtake it;
    /// after revocation returns, no further submission can use that device. `latest`
    /// refreshes Attention for each device under the pairing lock, before network I/O.
    /// It must release its Attention lock before returning; the lock order is pairing
    /// then Attention, never the reverse. A durable pre-attempt delay bounds
    /// retries after a crash; an interrupted remote effect can still be delivered twice.
    ///
    /// # Errors
    /// Returns persistence failures before any further submissions are attempted.
    pub fn deliver_notifications(
        &self,
        mut latest: impl FnMut() -> Result<Option<u64>, PairingError>,
        now_ms: u64,
        mut send: impl FnMut(&str, u64) -> Result<(), NotificationFailure>,
    ) -> Result<usize, PairingError> {
        if now_ms == 0 {
            return Err(PairingError::Rejected(
                "notification timestamp must be positive".to_owned(),
            ));
        }
        let devices = self.status()?.devices;
        let mut submitted = 0;
        for device in devices {
            submitted += usize::from(self.with_state(false, |state| {
                if state.notifications.health != NotificationHealth::Ready {
                    return Ok(false);
                }
                let Some(generation) = latest()? else {
                    return Ok(false);
                };
                if generation == 0 {
                    return Err(PairingError::Rejected(
                        "notification generation must be positive".to_owned(),
                    ));
                }
                let Some(registration) = state
                    .devices
                    .iter_mut()
                    .find(|current| current.status.device_id == device.device_id)
                    .and_then(|current| current.notification.as_mut())
                else {
                    return Ok(false);
                };
                if !registration.enabled
                    || registration
                        .submitted_generation
                        .is_some_and(|sent| sent >= generation)
                    || registration.retry_at_ms > now_ms
                {
                    return Ok(false);
                }
                registration.attempts = registration.attempts.saturating_add(1);
                let delay =
                    60_000_u64.saturating_mul(1 << registration.attempts.saturating_sub(1).min(6));
                registration.retry_at_ms = now_ms.saturating_add(delay);
                let token = registration.token.clone();
                self.persist(state)?;
                let result = send(&token, generation);
                let registration = state
                    .devices
                    .iter_mut()
                    .find(|current| current.status.device_id == device.device_id)
                    .and_then(|current| current.notification.as_mut())
                    .ok_or_else(|| {
                        PairingError::Rejected("notification registration disappeared".to_owned())
                    })?;
                let confirmed = match result {
                    Ok(()) => {
                        registration.submitted_generation = Some(generation);
                        registration.retry_at_ms = 0;
                        registration.attempts = 0;
                        true
                    }
                    Err(NotificationFailure::Transient { retry_after_ms }) => {
                        registration.retry_at_ms = registration
                            .retry_at_ms
                            .max(now_ms.saturating_add(retry_after_ms));
                        false
                    }
                    Err(NotificationFailure::InvalidToken) => {
                        registration.enabled = false;
                        false
                    }
                    Err(NotificationFailure::Authentication) => {
                        state.notifications.health = NotificationHealth::AuthenticationError;
                        false
                    }
                    Err(NotificationFailure::Configuration) => {
                        state.notifications.health = NotificationHealth::ConfigurationError;
                        false
                    }
                };
                self.persist(state)?;
                Ok(confirmed)
            })?);
        }
        Ok(submitted)
    }
}
