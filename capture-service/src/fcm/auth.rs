//! Fixed Google service-account OAuth assertion; no endpoint discovery.

use std::{fs::File, io::Read, path::Path};

use aws_lc_rs::{
    rand::SystemRandom,
    signature::{RSA_PKCS1_SHA256, RsaKeyPair},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use serde_json::json;

use crate::NotificationFailure;

pub(super) const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
pub(super) const SCOPE: &str = "https://www.googleapis.com/auth/firebase.messaging";
const MAX_CREDENTIAL_BYTES: u64 = 64 * 1024;

// Standard service-account key format. Optional metadata is never used for routing.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ServiceAccount {
    #[serde(rename = "type")]
    kind: String,
    project_id: String,
    private_key_id: String,
    private_key: String,
    client_email: String,
    token_uri: String,
    #[serde(default, rename = "client_id")]
    _client_id: Option<String>,
    #[serde(default, rename = "auth_uri")]
    _auth_uri: Option<String>,
    #[serde(default, rename = "auth_provider_x509_cert_url")]
    _auth_provider_x509_cert_url: Option<String>,
    #[serde(default, rename = "client_x509_cert_url")]
    _client_x509_cert_url: Option<String>,
    #[serde(default)]
    universe_domain: Option<String>,
}

pub(super) struct Credentials {
    pub project: String,
    email: String,
    key_id: String,
    key: RsaKeyPair,
}

impl Credentials {
    pub fn load(path: &Path) -> Result<Self, NotificationFailure> {
        let file = File::open(path).map_err(|_| NotificationFailure::Configuration)?;
        let metadata = file
            .metadata()
            .map_err(|_| NotificationFailure::Configuration)?;
        if !metadata.is_file() || metadata.len() > MAX_CREDENTIAL_BYTES {
            return Err(NotificationFailure::Configuration);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(NotificationFailure::Configuration);
            }
        }
        let mut bytes = Vec::new();
        file.take(MAX_CREDENTIAL_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| NotificationFailure::Configuration)?;
        Self::parse(&bytes)
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, NotificationFailure> {
        if bytes.len() as u64 > MAX_CREDENTIAL_BYTES {
            return Err(NotificationFailure::Configuration);
        }
        let account: ServiceAccount =
            serde_json::from_slice(bytes).map_err(|_| NotificationFailure::Configuration)?;
        if account.kind != "service_account"
            || account.token_uri != TOKEN_URL
            || account
                .universe_domain
                .as_deref()
                .is_some_and(|domain| domain != "googleapis.com")
            || !(6..=30).contains(&account.project_id.len())
            || !account
                .project_id
                .starts_with(|c: char| c.is_ascii_lowercase())
            || !account
                .project_id
                .ends_with(|c: char| c.is_ascii_alphanumeric())
            || !account
                .project_id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            || account.private_key_id.is_empty()
            || account.private_key_id.len() > 128
            || !account
                .private_key_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || account.client_email.len() > 254
            || !account.client_email.ends_with(".gserviceaccount.com")
            || account.client_email.bytes().filter(|b| *b == b'@').count() != 1
            || !account
                .client_email
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"@.-_".contains(&b))
        {
            return Err(NotificationFailure::Configuration);
        }
        let pem =
            pem::parse(&account.private_key).map_err(|_| NotificationFailure::Configuration)?;
        if pem.tag() != "PRIVATE KEY" {
            return Err(NotificationFailure::Configuration);
        }
        let key = RsaKeyPair::from_pkcs8(pem.contents())
            .map_err(|_| NotificationFailure::Configuration)?;
        Ok(Self {
            project: account.project_id,
            email: account.client_email,
            key_id: account.private_key_id,
            key,
        })
    }

    pub fn assertion(&self, now_ms: u64) -> Result<String, NotificationFailure> {
        let issued = now_ms / 1_000;
        if issued == 0 {
            return Err(NotificationFailure::Configuration);
        }
        let expires = issued
            .checked_add(3_600)
            .ok_or(NotificationFailure::Configuration)?;
        let header = URL_SAFE_NO_PAD
            .encode(json!({"alg": "RS256", "typ": "JWT", "kid": self.key_id}).to_string());
        let claims = URL_SAFE_NO_PAD.encode(json!({"iss": self.email, "scope": SCOPE, "aud": TOKEN_URL, "iat": issued, "exp": expires}).to_string());
        let signing_input = format!("{header}.{claims}");
        let mut signature = vec![0; self.key.public_modulus_len()];
        self.key
            .sign(
                &RSA_PKCS1_SHA256,
                &SystemRandom::new(),
                signing_input.as_bytes(),
                &mut signature,
            )
            .map_err(|_| NotificationFailure::Configuration)?;
        Ok(format!(
            "{signing_input}.{}",
            URL_SAFE_NO_PAD.encode(signature)
        ))
    }
}
