//! Parse exact starting Cargo bytes without invoking Cargo, Git or a registry.

use serde::Deserialize;

use super::{Candidate, MAX_CANDIDATES, Source, invalid};
use crate::{Digest, launch_protocol::ProtocolError};

/// Immutable dependency inventory captured from trusted starting lockfile bytes.
/// Parsing proves structure; the caller must establish source snapshot provenance.
#[derive(Clone, Debug)]
pub struct StartingLockfile {
    digest: String,
    entries: Vec<Candidate>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CargoLock {
    version: u32,
    #[serde(default)]
    package: Vec<Package>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Package {
    name: String,
    version: String,
    source: Option<String>,
    checksum: Option<String>,
    #[serde(default, rename = "dependencies")]
    _dependencies: Vec<String>,
}

impl StartingLockfile {
    /// Parses Cargo lockfile versions 3/4 from a bounded immutable byte snapshot.
    /// Local workspace packages are not downloads. Unknown sources remain typed
    /// exceptional candidates; their presence never makes them automatic.
    /// # Errors
    /// Rejects malformed/oversized lockfiles, unsupported formats, invalid exact
    /// coordinates/checksums and duplicate package identities.
    pub fn cargo(bytes: &[u8]) -> Result<Self, ProtocolError> {
        if bytes.len() > 4 * 1024 * 1024 {
            return Err(invalid());
        }
        let document: CargoLock =
            toml::from_str(std::str::from_utf8(bytes).map_err(|_| invalid())?)
                .map_err(|_| invalid())?;
        if !matches!(document.version, 3 | 4) || document.package.len() > MAX_CANDIDATES {
            return Err(invalid());
        }
        let mut entries = Vec::new();
        let mut identities = std::collections::BTreeSet::new();
        for package in document.package {
            let Some(locator) = &package.source else {
                if package.checksum.is_some() {
                    return Err(invalid());
                }
                continue;
            };
            if !identities.insert((
                package.name.clone(),
                package.version.clone(),
                locator.clone(),
            )) {
                return Err(invalid());
            }
            let candidate = Candidate {
                name: package.name,
                version: package.version,
                source: source(locator)?,
                integrity: package
                    .checksum
                    .map(|checksum| format!("sha256:{checksum}")),
            };
            candidate.validate()?;
            entries.push(candidate);
        }
        Ok(Self {
            digest: Digest::of(bytes).to_string(),
            entries,
        })
    }

    /// Digest of exact starting bytes, independent of later workspace edits.
    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// Captured external dependencies. No caller can append to this inventory.
    #[must_use]
    pub fn entries(&self) -> &[Candidate] {
        &self.entries
    }
}

fn source(locator: &str) -> Result<Source, ProtocolError> {
    if locator == "registry+https://github.com/rust-lang/crates.io-index" {
        return Ok(Source::Registry {
            registry: "crates-io".into(),
        });
    }
    if let Some(registry) = locator
        .strip_prefix("registry+")
        .or_else(|| locator.strip_prefix("sparse+"))
    {
        return Ok(Source::Registry {
            registry: registry.into(),
        });
    }
    if let Some(git) = locator.strip_prefix("git+") {
        let (repository, revision) = git.rsplit_once('#').ok_or_else(invalid)?;
        return Ok(Source::Git {
            repository: repository.into(),
            revision: revision.into(),
        });
    }
    Ok(Source::Other {
        locator: locator.into(),
    })
}
