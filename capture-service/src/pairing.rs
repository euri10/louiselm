use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use thiserror::Error;
use url::Url;
use uuid::Uuid;

use crate::permissions::set_private_permissions;

#[path = "notifications.rs"]
mod notifications;
pub use notifications::{
    NotificationFailure, NotificationHealth, NotificationStatus, NotificationTargetStatus,
};
use notifications::{NotificationRegistration, NotificationState};

/// One-time pairing material intended for a QR payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PairingOffer {
    /// Pairing protocol version.
    #[serde(rename = "v")]
    pub version: u8,
    /// HTTPS receiver URL reachable by the phone.
    #[serde(rename = "u")]
    pub receiver_url: String,
    /// Lowercase SHA-256 fingerprint of the receiver public-key identity.
    #[serde(rename = "i")]
    pub receiver_identity_sha256: String,
    /// Short-lived bearer used only by the pairing endpoint.
    #[serde(rename = "t")]
    pub token: String,
    /// Unix epoch milliseconds after which the token is invalid.
    #[serde(rename = "e")]
    pub expires_at_ms: u64,
}

/// Secret returned exactly once to a newly paired device.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DeviceCredential {
    /// Stable public device identifier used for revocation.
    pub device_id: String,
    /// Human-readable device name.
    pub device_name: String,
    /// Bearer credential; only its hash is persisted by the receiver.
    pub credential: String,
}

/// Public device information safe to display.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct DeviceStatus {
    /// Stable public device identifier.
    pub device_id: String,
    /// Human-readable name.
    pub device_name: String,
    /// Unix epoch milliseconds when pairing succeeded.
    pub paired_at_ms: u64,
    /// Unix epoch milliseconds of the last durably accepted capture.
    pub last_delivery_at_ms: Option<u64>,
}

/// Public pairing registry state safe to display.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PairingStatus {
    /// Active devices in stable identifier order.
    pub devices: Vec<DeviceStatus>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PendingOffer {
    token_sha256: String,
    expires_at_ms: u64,
}

#[derive(Clone, Deserialize, Serialize)]
struct DeviceRecord {
    #[serde(flatten)]
    status: DeviceStatus,
    credential_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    notification: Option<NotificationRegistration>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
struct RegistryState {
    schema_version: u8,
    pending: Vec<PendingOffer>,
    devices: Vec<DeviceRecord>,
    #[serde(default)]
    notifications: NotificationState,
}

/// Pairing-registry failure.
#[derive(Debug, Error)]
pub enum PairingError {
    /// Refreshing the latest Attention generation failed before submission.
    #[error(transparent)]
    Attention(#[from] crate::AttentionError),
    /// The device bearer does not identify an active pairing.
    #[error("device credential is invalid")]
    Unauthorized,
    /// Pairing input is malformed or a token is invalid/expired.
    #[error("pairing rejected: {0}")]
    Rejected(String),
    /// Registry filesystem operation failed.
    #[error("pairing storage failed: {0}")]
    Io(#[from] io::Error),
    /// Persisted registry data is malformed.
    #[error("pairing data is malformed: {0}")]
    Json(#[from] serde_json::Error),
    /// Concurrent registry access failed after a poisoned lock.
    #[error("pairing registry lock is unavailable")]
    Lock,
}

/// Persistent owner of one-time tokens and revocable device credentials.
pub struct PairingRegistry {
    path: PathBuf,
    lock_path: PathBuf,
    process_lock: Mutex<()>,
}

impl PairingRegistry {
    /// Open or initialize a pairing registry directory.
    ///
    /// # Errors
    ///
    /// Returns filesystem or malformed-state errors.
    pub fn open(directory: impl AsRef<Path>) -> Result<Self, PairingError> {
        fs::create_dir_all(directory.as_ref())?;
        set_private_permissions(directory.as_ref(), true)?;
        let path = directory.as_ref().join("pairing.json");
        let registry = Self {
            path,
            lock_path: directory.as_ref().join("pairing.lock"),
            process_lock: Mutex::new(()),
        };
        {
            let _process_guard = registry
                .process_lock
                .lock()
                .map_err(|_| PairingError::Lock)?;
            let lock = registry.lock_file()?;
            lock.lock_exclusive()?;
            if registry.path.exists() {
                registry.load()?;
            } else {
                registry.persist(&RegistryState {
                    schema_version: 3,
                    ..RegistryState::default()
                })?;
            }
            FileExt::unlock(&lock)?;
        }
        Ok(registry)
    }

    /// Issue one short-lived QR pairing offer.
    ///
    /// # Errors
    ///
    /// Rejects invalid URLs, identities, timestamps, and persistence errors.
    pub fn issue(
        &self,
        receiver_url: &str,
        receiver_identity_sha256: &str,
        now_ms: u64,
        ttl_ms: u64,
    ) -> Result<PairingOffer, PairingError> {
        let parsed_url = Url::parse(receiver_url).ok();
        let valid_url = parsed_url.as_ref().is_some_and(|url| {
            url.scheme() == "https"
                && url.host().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && (url.path().is_empty() || url.path() == "/")
                && url.query().is_none()
                && url.fragment().is_none()
        });
        if !valid_url || receiver_url.contains(char::is_whitespace) {
            return Err(PairingError::Rejected(
                "receiver URL must be a clean HTTPS base URL".to_owned(),
            ));
        }
        if receiver_identity_sha256.len() != 64
            || !receiver_identity_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(PairingError::Rejected(
                "receiver identity fingerprint must be hexadecimal".to_owned(),
            ));
        }
        if now_ms == 0 || ttl_ms == 0 {
            return Err(PairingError::Rejected(
                "pairing timestamps must be positive".to_owned(),
            ));
        }
        self.with_state(true, |state| {
            let token = secret();
            let expires_at_ms = now_ms.saturating_add(ttl_ms);
            state.pending.retain(|offer| offer.expires_at_ms > now_ms);
            state.pending.push(PendingOffer {
                token_sha256: hash(&token),
                expires_at_ms,
            });
            Ok(PairingOffer {
                version: 2,
                receiver_url: receiver_url.to_owned(),
                receiver_identity_sha256: receiver_identity_sha256.to_ascii_lowercase(),
                token,
                expires_at_ms,
            })
        })
    }

    /// Consume one offer and return a scoped credential exactly once.
    ///
    /// # Errors
    ///
    /// Rejects invalid, expired, or reused tokens and invalid names.
    pub fn consume(
        &self,
        token: &str,
        device_name: &str,
        now_ms: u64,
    ) -> Result<DeviceCredential, PairingError> {
        let device_name = device_name.trim();
        if device_name.is_empty()
            || device_name.len() > 80
            || device_name.chars().any(char::is_control)
        {
            return Err(PairingError::Rejected("device name is invalid".to_owned()));
        }
        self.with_state(true, |state| {
            let token_hash = hash(token);
            let matched = state
                .pending
                .iter()
                .position(|offer| offer.token_sha256 == token_hash && offer.expires_at_ms >= now_ms)
                .ok_or_else(|| {
                    PairingError::Rejected("pairing token is invalid or expired".to_owned())
                })?;
            state.pending.remove(matched);
            state.pending.retain(|offer| offer.expires_at_ms >= now_ms);

            let device_id = Uuid::new_v4().to_string();
            let credential = secret();
            state.devices.push(DeviceRecord {
                status: DeviceStatus {
                    device_id: device_id.clone(),
                    device_name: device_name.to_owned(),
                    paired_at_ms: now_ms,
                    last_delivery_at_ms: None,
                },
                credential_sha256: hash(&credential),
                notification: None,
            });
            Ok(DeviceCredential {
                device_id,
                device_name: device_name.to_owned(),
                credential,
            })
        })
    }

    /// Authenticate a bearer and return its public device identifier.
    ///
    /// # Errors
    ///
    /// Returns lock failure.
    pub fn authenticate_device(&self, credential: &str) -> Result<Option<String>, PairingError> {
        self.with_state(false, |state| {
            let credential_hash = hash(credential);
            Ok(state.devices.iter().find_map(|device| {
                if bool::from(
                    device
                        .credential_sha256
                        .as_bytes()
                        .ct_eq(credential_hash.as_bytes()),
                ) {
                    Some(device.status.device_id.clone())
                } else {
                    None
                }
            }))
        })
    }

    /// Persist a successful canonical delivery for one authenticated device.
    ///
    /// # Errors
    ///
    /// Rejects unknown devices or zero timestamps and returns persistence failures.
    pub fn record_delivery(&self, device_id: &str, now_ms: u64) -> Result<(), PairingError> {
        if now_ms == 0 {
            return Err(PairingError::Rejected(
                "delivery timestamp must be positive".to_owned(),
            ));
        }
        self.with_state(true, |state| {
            let device = state
                .devices
                .iter_mut()
                .find(|device| device.status.device_id == device_id)
                .ok_or_else(|| PairingError::Rejected("device is unknown".to_owned()))?;
            device.status.last_delivery_at_ms = Some(now_ms);
            Ok(())
        })
    }

    /// Revoke one device by its public identifier.
    ///
    /// # Errors
    ///
    /// Returns a rejection for an unknown identifier or a persistence failure.
    pub fn revoke(&self, device_id: &str) -> Result<(), PairingError> {
        self.with_state(true, |state| {
            let previous = state.devices.len();
            state
                .devices
                .retain(|device| device.status.device_id != device_id);
            if previous == state.devices.len() {
                return Err(PairingError::Rejected("device is unknown".to_owned()));
            }
            Ok(())
        })
    }

    /// Return public device status without credentials or hashes.
    ///
    /// # Errors
    ///
    /// Returns lock failure.
    pub fn status(&self) -> Result<PairingStatus, PairingError> {
        self.with_state(false, |state| {
            let mut devices = state
                .devices
                .iter()
                .map(|record| record.status.clone())
                .collect::<Vec<_>>();
            devices.sort_by(|left, right| left.device_id.cmp(&right.device_id));
            Ok(PairingStatus { devices })
        })
    }

    fn with_state<R>(
        &self,
        persist: bool,
        operation: impl FnOnce(&mut RegistryState) -> Result<R, PairingError>,
    ) -> Result<R, PairingError> {
        let _process_guard = self.process_lock.lock().map_err(|_| PairingError::Lock)?;
        let lock = self.lock_file()?;
        lock.lock_exclusive()?;
        let mut state = self.load()?;
        let result = operation(&mut state)?;
        if persist {
            self.persist(&state)?;
        }
        FileExt::unlock(&lock)?;
        Ok(result)
    }

    fn lock_file(&self) -> Result<File, PairingError> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&self.lock_path)?;
        set_private_permissions(&self.lock_path, false)?;
        Ok(file)
    }

    fn load(&self) -> Result<RegistryState, PairingError> {
        let mut value: serde_json::Value =
            serde_json::from_reader(BufReader::new(File::open(&self.path)?))?;
        let upgrading = value["schema_version"] == 2;
        if upgrading {
            // Retire token routing without losing pairings or confirmed generations.
            // Android must register its FID before any further delivery is allowed.
            if let Some(devices) = value["devices"].as_array_mut() {
                for device in devices {
                    if let Some(registration) = device
                        .get_mut("notification")
                        .and_then(serde_json::Value::as_object_mut)
                    {
                        if registration.contains_key("fid")
                            || !registration
                                .remove("token")
                                .as_ref()
                                .and_then(serde_json::Value::as_str)
                                .is_some_and(|token| {
                                    !token.is_empty()
                                        && token.len() <= 4096
                                        && token.bytes().all(|byte| byte.is_ascii_graphic())
                                })
                            || registration
                                .get("enabled")
                                .and_then(serde_json::Value::as_bool)
                                .is_none()
                        {
                            return Err(PairingError::Rejected(
                                "stored notification state is invalid".to_owned(),
                            ));
                        }
                        registration.insert("fid".into(), serde_json::Value::Null);
                        registration.insert("enabled".into(), false.into());
                    }
                }
            }
            value["schema_version"] = 3.into();
        }
        let state: RegistryState = serde_json::from_value(value)?;
        if state.schema_version != 3 {
            return Err(PairingError::Rejected(
                "pairing registry version is unsupported".to_owned(),
            ));
        }
        notifications::validate_state(&state)?;
        if upgrading {
            self.persist(&state)?;
        }
        Ok(state)
    }

    fn persist(&self, state: &RegistryState) -> Result<(), PairingError> {
        let temporary = self
            .path
            .with_file_name(format!(".pairing-{}", Uuid::new_v4()));
        let result = (|| {
            let file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)?;
            set_private_permissions(&temporary, false)?;
            let mut writer = BufWriter::new(file);
            serde_json::to_writer_pretty(&mut writer, state)?;
            writer.write_all(b"\n")?;
            writer.flush()?;
            writer.get_ref().sync_all()?;
            fs::rename(&temporary, &self.path)?;
            File::open(self.path.parent().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "pairing path has no parent")
            })?)?
            .sync_all()?;
            Ok(())
        })();
        if temporary.exists() {
            let _ = fs::remove_file(temporary);
        }
        result
    }
}

fn secret() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

fn hash(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
