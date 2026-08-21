use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::Path,
    str::FromStr,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::{Host, Url};
use uuid::Uuid;

const SCHEMA_VERSION: u8 = 1;

/// Supported private receiver exposure profiles.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkProfileKind {
    /// One selected private LAN address.
    Lan,
    /// One address owned by an external private overlay.
    Overlay,
    /// An explicitly configured unusual private deployment.
    Private,
}

impl FromStr for NetworkProfileKind {
    type Err = NetworkProfileError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "lan" => Ok(Self::Lan),
            "overlay" => Ok(Self::Overlay),
            "private" => Ok(Self::Private),
            _ => Err(NetworkProfileError::Invalid(
                "profile must be lan, overlay, or private".to_owned(),
            )),
        }
    }
}

/// One persisted bind and advertised endpoint used by serve, pair, and status.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct NetworkProfile {
    schema_version: u8,
    profile: Option<NetworkProfileKind>,
    bind: SocketAddr,
    receiver_url: Option<String>,
}

impl NetworkProfile {
    /// Validate one deliberately configured private network profile.
    pub fn new(
        profile: NetworkProfileKind,
        bind: SocketAddr,
        receiver_url: &str,
    ) -> Result<Self, NetworkProfileError> {
        validate_bind(bind)?;
        validate_url(profile, bind, receiver_url)?;
        Ok(Self {
            schema_version: SCHEMA_VERSION,
            profile: Some(profile),
            bind,
            receiver_url: Some(receiver_url.trim_end_matches('/').to_owned()),
        })
    }

    /// Load a configured profile.
    pub fn load(path: &Path) -> Result<Self, NetworkProfileError> {
        let profile: Self = serde_json::from_reader(File::open(path)?)?;
        profile.validate()?;
        Ok(profile)
    }

    /// Load a profile or return the safe loopback-only state when none exists.
    pub fn load_or_default(path: &Path) -> Result<Self, NetworkProfileError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        Self::load(path)
    }

    /// Persist a configured profile atomically.
    pub fn save(&self, path: &Path) -> Result<(), NetworkProfileError> {
        self.validate()?;
        let parent = path.parent().ok_or_else(|| {
            NetworkProfileError::Invalid("network profile path has no parent".to_owned())
        })?;
        fs::create_dir_all(parent)?;
        set_private_permissions(parent, true)?;
        let temporary = path.with_file_name(format!(".network-{}", Uuid::new_v4()));
        let result = (|| {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)?;
            set_private_permissions(&temporary, false)?;
            serde_json::to_writer_pretty(&mut file, self)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            fs::rename(&temporary, path)?;
            File::open(parent)?.sync_all()?;
            Ok(())
        })();
        if temporary.exists() {
            let _ = fs::remove_file(temporary);
        }
        result
    }

    /// Configured profile kind, or none for the loopback-only default.
    #[must_use]
    pub fn kind(&self) -> Option<NetworkProfileKind> {
        self.profile
    }

    /// Socket address used by the receiver.
    #[must_use]
    pub fn bind(&self) -> SocketAddr {
        self.bind
    }

    /// HTTPS endpoint advertised to a phone, when configured.
    #[must_use]
    pub fn receiver_url(&self) -> Option<&str> {
        self.receiver_url.as_deref()
    }

    /// Whether the profile deliberately exposes a private phone endpoint.
    #[must_use]
    pub fn phone_reachable(&self) -> bool {
        self.profile.is_some() && self.receiver_url.is_some()
    }

    fn validate(&self) -> Result<(), NetworkProfileError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(NetworkProfileError::Invalid(
                "network profile version is unsupported".to_owned(),
            ));
        }
        match (self.profile, self.receiver_url.as_deref()) {
            (None, None) if self.bind == default_bind() => Ok(()),
            (Some(profile), Some(receiver_url)) => {
                validate_bind(self.bind)?;
                validate_url(profile, self.bind, receiver_url)
            }
            _ => Err(NetworkProfileError::Invalid(
                "network profile is incomplete".to_owned(),
            )),
        }
    }
}

impl Default for NetworkProfile {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            profile: None,
            bind: default_bind(),
            receiver_url: None,
        }
    }
}

/// Network-profile validation or persistence failure.
#[derive(Debug, Error)]
pub enum NetworkProfileError {
    /// Filesystem operation failed.
    #[error("network profile storage failed: {0}")]
    Io(#[from] io::Error),
    /// Persisted JSON is malformed.
    #[error("network profile is malformed: {0}")]
    Json(#[from] serde_json::Error),
    /// Operator-provided configuration is unsupported or unsafe.
    #[error("network profile is invalid: {0}")]
    Invalid(String),
}

fn default_bind() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 7391))
}

fn validate_bind(bind: SocketAddr) -> Result<(), NetworkProfileError> {
    if bind.port() == 0 || !private_ip(bind.ip()) {
        return Err(NetworkProfileError::Invalid(
            "bind must be one explicit private IP and nonzero port".to_owned(),
        ));
    }
    Ok(())
}

fn validate_url(
    profile: NetworkProfileKind,
    bind: SocketAddr,
    receiver_url: &str,
) -> Result<(), NetworkProfileError> {
    let url = Url::parse(receiver_url).map_err(|_| {
        NetworkProfileError::Invalid("receiver URL must be a clean HTTPS base URL".to_owned())
    })?;
    let valid_base = url.scheme() == "https"
        && url.host().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && (url.path().is_empty() || url.path() == "/")
        && url.query().is_none()
        && url.fragment().is_none()
        && !receiver_url.contains(char::is_whitespace);
    if !valid_base {
        return Err(NetworkProfileError::Invalid(
            "receiver URL must be a clean HTTPS base URL".to_owned(),
        ));
    }
    let advertised_ip = match url.host() {
        Some(Host::Ipv4(ip)) => Some(IpAddr::V4(ip)),
        Some(Host::Ipv6(ip)) => Some(IpAddr::V6(ip)),
        _ => None,
    };
    if advertised_ip.is_some_and(|ip| !private_ip(ip)) {
        return Err(NetworkProfileError::Invalid(
            "receiver URL must not advertise a public IP".to_owned(),
        ));
    }
    if profile == NetworkProfileKind::Lan && advertised_ip != Some(bind.ip()) {
        return Err(NetworkProfileError::Invalid(
            "LAN receiver URL must advertise the selected bind IP".to_owned(),
        ));
    }
    Ok(())
}

fn private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_private() || ip.is_link_local() || shared_overlay_ipv4(ip),
        IpAddr::V6(ip) => ip.is_unique_local() || ip.is_unicast_link_local(),
    }
}

fn shared_overlay_ipv4(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    octets[0] == 100 && (64..=127).contains(&octets[1])
}

#[cfg(unix)]
fn set_private_permissions(path: &Path, directory: bool) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(
        path,
        fs::Permissions::from_mode(if directory { 0o700 } else { 0o600 }),
    )
}

#[cfg(not(unix))]
fn set_private_permissions(_path: &Path, _directory: bool) -> io::Result<()> {
    Ok(())
}
