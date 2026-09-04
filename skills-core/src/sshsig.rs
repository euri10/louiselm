//! OpenSSH signature parsing, FIDO assertion flags, and verification.
//!
//! Skill Admission is authorized by an SSH signature in the `SSHSIG` format
//! (OpenSSH `PROTOCOL.sshsig`), in a namespace this project owns. Two things
//! are checked here that `ssh-keygen -Y verify` alone cannot answer:
//!
//! * **Which key signed.** Verification is against one enrolled key, not a list
//!   of acceptable ones, so a valid signature by an unenrolled key is refused
//!   with a distinct error rather than merged into "invalid signature".
//! * **How the key was touched.** A FIDO assertion carries user-presence and
//!   user-verification flags in the signature blob. `ssh-keygen` does not
//!   report them, so this module parses them and enforces the policy the role
//!   was enrolled with. The flag check runs *before* the cryptographic check:
//!   an assertion that was never touched must be refused for that reason, not
//!   for a downstream one.
//!
//! Cryptography itself is delegated to `ssh-keygen`, which is part of the
//! trusted base on this platform. This crate does not implement ed25519.

use std::{
    fs, io,
    path::PathBuf,
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The signature namespace Skill Admission owns.
///
/// Domain separation is the point: a signature made for a release, an
/// Endorsement, or a git commit must never verify as an Admission.
pub const ADMISSION_NAMESPACE: &str = "louiselm.skills.admission/1";

/// The signature namespace trust changes own.
pub const TRUST_NAMESPACE: &str = "louiselm.skills.trust/1";

const MAGIC: &[u8] = b"SSHSIG";
const BEGIN: &str = "-----BEGIN SSH SIGNATURE-----";
const END: &str = "-----END SSH SIGNATURE-----";
const FLAG_USER_PRESENT: u8 = 0x01;
const FLAG_USER_VERIFIED: u8 = 0x04;

static SCRATCH_COUNTER: AtomicU64 = AtomicU64::new(0);

/// FIDO assertion flags carried by a hardware-backed signature.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkFlags {
    /// Whether the authenticator reported a user touch.
    pub user_presence: bool,
    /// Whether the authenticator reported a verified user (PIN or biometric).
    pub user_verification: bool,
    /// The authenticator's signature counter.
    pub counter: u32,
}

/// What a role requires of the authenticator that signs for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SkPolicy {
    /// Whether the signature must come from a hardware-backed key at all.
    pub require_hardware: bool,
    /// Whether the assertion must report a user touch.
    pub require_user_presence: bool,
    /// Whether the assertion must report a verified user.
    pub require_user_verification: bool,
}

impl SkPolicy {
    /// Accepts a software key; used for tests and for enrollment bootstrap.
    pub fn none() -> Self {
        Self::default()
    }

    /// Requires a hardware key that reports both touch and user verification.
    pub fn require_presence_and_verification() -> Self {
        Self {
            require_hardware: true,
            require_user_presence: true,
            require_user_verification: true,
        }
    }
}

/// A parsed SSH signature.
#[derive(Clone, Debug, Serialize)]
pub struct SshSignature {
    /// Signature algorithm, e.g. `ssh-ed25519` or `sk-ssh-ed25519@openssh.com`.
    pub algorithm: String,
    /// Namespace the signature was made in.
    pub namespace: String,
    /// Hash algorithm named in the envelope.
    pub hash_algorithm: String,
    /// FIDO assertion flags, when the key is hardware-backed.
    pub sk_flags: Option<SkFlags>,
    public_key_blob: Vec<u8>,
    public_key_algorithm: String,
}

impl SshSignature {
    /// Returns the signing key in `authorized_keys` form.
    pub fn openssh_public_key(&self) -> String {
        format!(
            "{} {}",
            self.public_key_algorithm,
            base64_encode(&self.public_key_blob)
        )
    }

    /// Reports whether the signature came from a hardware-backed key.
    pub fn is_hardware_backed(&self) -> bool {
        self.sk_flags.is_some()
    }
}

/// A signature that cannot be accepted.
#[derive(Debug, Error)]
pub enum SignatureError {
    /// The armored envelope or its contents are malformed.
    #[error("signature is malformed: {0}")]
    Malformed(String),
    /// The signature was made in a different namespace.
    #[error("signature namespace is '{found}', expected '{expected}'")]
    NamespaceMismatch {
        /// Namespace the signature carries.
        found: String,
        /// Namespace required here.
        expected: String,
    },
    /// The signature was made by a key that is not the enrolled one.
    #[error("signature is by an unenrolled key: {found}")]
    KeyMismatch {
        /// Key that actually signed.
        found: String,
    },
    /// The role requires a hardware key and the signature is from a software key.
    #[error("role requires a hardware-backed key; '{algorithm}' is not one")]
    NotHardwareBacked {
        /// Algorithm that signed.
        algorithm: String,
    },
    /// The assertion does not report the flags the role requires.
    #[error("hardware assertion is missing {missing}")]
    AssertionFlags {
        /// Human-readable list of missing flags.
        missing: String,
    },
    /// `ssh-keygen` rejected the signature.
    #[error("signature failed cryptographic verification: {0}")]
    Cryptographic(String),
    /// `ssh-keygen` is not installed.
    #[error("ssh-keygen is required to verify signatures: {0}")]
    ToolMissing(String),
    /// Scratch files could not be written.
    #[error("cannot prepare verification input: {0}")]
    Io(#[from] io::Error),
}

/// Parses an armored SSH signature without verifying it.
pub fn parse(armored: &str) -> Result<SshSignature, SignatureError> {
    let blob = dearmor(armored)?;
    let mut reader = Reader::new(&blob);
    let magic = reader.take(MAGIC.len())?;
    if magic != MAGIC {
        return Err(SignatureError::Malformed(
            "envelope does not start with SSHSIG".to_owned(),
        ));
    }
    let version = reader.u32()?;
    if version != 1 {
        return Err(SignatureError::Malformed(format!(
            "unsupported signature version {version}"
        )));
    }
    let public_key = reader.string()?.to_vec();
    let namespace = reader.utf8_string()?;
    let _reserved = reader.string()?;
    let hash_algorithm = reader.utf8_string()?;
    let signature = reader.string()?.to_vec();
    if !reader.is_empty() {
        return Err(SignatureError::Malformed(
            "trailing bytes after the signature".to_owned(),
        ));
    }

    let public_key_algorithm = Reader::new(&public_key).utf8_string()?;
    let mut signature_reader = Reader::new(&signature);
    let algorithm = signature_reader.utf8_string()?;
    let _raw = signature_reader.string()?;
    let sk_flags = if algorithm.starts_with("sk-") {
        let flags = signature_reader.u8()?;
        let counter = signature_reader.u32()?;
        Some(SkFlags {
            user_presence: flags & FLAG_USER_PRESENT != 0,
            user_verification: flags & FLAG_USER_VERIFIED != 0,
            counter,
        })
    } else {
        None
    };

    Ok(SshSignature {
        algorithm,
        namespace,
        hash_algorithm,
        sk_flags,
        public_key_blob: public_key,
        public_key_algorithm,
    })
}

/// Verifies `armored` over `payload`, in `namespace`, by exactly `public_key`.
///
/// The order of checks is deliberate: shape, namespace, key identity, and
/// assertion flags are all decided before `ssh-keygen` is asked anything, so
/// each refusal names the reason a reviewer needs rather than the first one a
/// verifier happened to hit.
pub fn verify(
    armored: &str,
    namespace: &str,
    payload: &[u8],
    public_key: &str,
    policy: SkPolicy,
) -> Result<SshSignature, SignatureError> {
    let signature = parse(armored)?;
    if signature.namespace != namespace {
        return Err(SignatureError::NamespaceMismatch {
            found: signature.namespace.clone(),
            expected: namespace.to_owned(),
        });
    }
    let found = signature.openssh_public_key();
    if found != normalize_public_key(public_key) {
        return Err(SignatureError::KeyMismatch { found });
    }
    check_assertion(&signature, policy)?;
    verify_cryptographically(armored, namespace, payload, &found)?;
    Ok(signature)
}

fn check_assertion(signature: &SshSignature, policy: SkPolicy) -> Result<(), SignatureError> {
    let Some(flags) = signature.sk_flags else {
        if policy.require_hardware {
            return Err(SignatureError::NotHardwareBacked {
                algorithm: signature.algorithm.clone(),
            });
        }
        return Ok(());
    };
    let mut missing = Vec::new();
    if policy.require_user_presence && !flags.user_presence {
        missing.push("user presence");
    }
    if policy.require_user_verification && !flags.user_verification {
        missing.push("user verification");
    }
    if missing.is_empty() {
        return Ok(());
    }
    Err(SignatureError::AssertionFlags {
        missing: missing.join(" and "),
    })
}

fn verify_cryptographically(
    armored: &str,
    namespace: &str,
    payload: &[u8],
    public_key: &str,
) -> Result<(), SignatureError> {
    let scratch = Scratch::new()?;
    let signature_path = scratch.path.join("signature");
    let allowed_path = scratch.path.join("allowed_signers");
    let payload_path = scratch.path.join("payload");
    fs::write(&signature_path, armored)?;
    fs::write(&allowed_path, format!("admission {public_key}\n"))?;
    fs::write(&payload_path, payload)?;

    let output = Command::new("ssh-keygen")
        .arg("-Y")
        .arg("verify")
        .arg("-f")
        .arg(&allowed_path)
        .arg("-I")
        .arg("admission")
        .arg("-n")
        .arg(namespace)
        .arg("-s")
        .arg(&signature_path)
        .stdin(Stdio::from(fs::File::open(&payload_path)?))
        .output()
        .map_err(|error| SignatureError::ToolMissing(error.to_string()))?;

    if output.status.success() {
        return Ok(());
    }
    let mut reason = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if reason.is_empty() {
        reason = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    }
    Err(SignatureError::Cryptographic(crate::scan::escape(&reason)))
}

fn normalize_public_key(public_key: &str) -> String {
    public_key
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ")
}

struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new() -> Result<Self, SignatureError> {
        let counter = SCRATCH_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "louiselm-skills-verify-{}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&path)?;
        Ok(Self { path })
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn is_empty(&self) -> bool {
        self.offset >= self.bytes.len()
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], SignatureError> {
        let end = self
            .offset
            .checked_add(length)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| SignatureError::Malformed("truncated signature".to_owned()))?;
        let taken = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(taken)
    }

    fn u8(&mut self) -> Result<u8, SignatureError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, SignatureError> {
        let bytes = self.take(4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn string(&mut self) -> Result<&'a [u8], SignatureError> {
        let length = self.u32()? as usize;
        self.take(length)
    }

    fn utf8_string(&mut self) -> Result<String, SignatureError> {
        let bytes = self.string()?;
        String::from_utf8(bytes.to_vec())
            .map_err(|_| SignatureError::Malformed("field is not UTF-8".to_owned()))
    }
}

fn dearmor(armored: &str) -> Result<Vec<u8>, SignatureError> {
    let trimmed = armored.trim();
    if !trimmed.starts_with(BEGIN) || !trimmed.ends_with(END) {
        return Err(SignatureError::Malformed(
            "missing SSH SIGNATURE armor".to_owned(),
        ));
    }
    let body = trimmed
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .collect::<String>();
    base64_decode(&body)
}

fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let mut buffer = [0_u8; 3];
        buffer[..chunk.len()].copy_from_slice(chunk);
        let value = u32::from_be_bytes([0, buffer[0], buffer[1], buffer[2]]);
        for position in 0..4 {
            if position <= chunk.len() {
                let index = (value >> (18 - 6 * position)) & 0x3f;
                encoded.push(ALPHABET[index as usize] as char);
            } else {
                encoded.push('=');
            }
        }
    }
    encoded
}

fn base64_decode(text: &str) -> Result<Vec<u8>, SignatureError> {
    let mut decoded = Vec::with_capacity(text.len() / 4 * 3);
    let mut accumulator = 0_u32;
    let mut bits = 0_u32;
    for character in text.chars() {
        if character.is_whitespace() || character == '=' {
            continue;
        }
        let value = match character {
            'A'..='Z' => character as u32 - 'A' as u32,
            'a'..='z' => character as u32 - 'a' as u32 + 26,
            '0'..='9' => character as u32 - '0' as u32 + 52,
            '+' => 62,
            '/' => 63,
            other => {
                return Err(SignatureError::Malformed(format!(
                    "'{}' is not base64",
                    crate::scan::escape(&other.to_string())
                )));
            }
        };
        accumulator = (accumulator << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            decoded.push((accumulator >> bits) as u8);
        }
    }
    Ok(decoded)
}
