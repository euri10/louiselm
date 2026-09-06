//! The canonical path, mode, hashing, and serialization contract.
//!
//! Every later stage — Inspection, the Dossier, Skill Admission signatures, and
//! remote witnessing — binds to the bytes defined here. Changing any rule in
//! this module changes every package digest, so the rules are stated once and
//! pinned by golden vectors in `tests/canonical.rs`.
//!
//! The contract is deliberately narrow:
//!
//! * A package path is a `/`-separated relative path of non-empty components,
//!   with no `.`, no `..`, no control characters, and (unless policy opts in)
//!   no bytes outside printable ASCII.
//! * File modes collapse to a single `executable` bit; owner, group, setuid,
//!   and the remaining permission bits never reach a manifest.
//! * Content is addressed by SHA-256, rendered as `sha256:<64 lowercase hex>`.

use std::fmt::{self, Write as _};

use sha2::{Digest as _, Sha256};
use thiserror::Error;

/// Longest accepted single path component, in bytes.
pub const MAX_COMPONENT_BYTES: usize = 255;

/// Longest accepted whole relative path, in bytes.
pub const MAX_PATH_BYTES: usize = 1024;

/// A relative path rejected by the canonical contract.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PathError {
    /// The path is empty or consists only of separators.
    #[error("path is empty")]
    Empty,
    /// The path is absolute, so it cannot address a location inside a package.
    #[error("path '{0}' is absolute")]
    Absolute(String),
    /// The path contains an empty component, produced by `//` or a trailing `/`.
    #[error("path '{0}' contains an empty component")]
    EmptyComponent(String),
    /// The path contains `.` or `..`, which would make its target ambiguous.
    #[error("path '{0}' contains a relative component")]
    RelativeComponent(String),
    /// The path contains a control character or NUL.
    #[error("path '{0}' contains a control character")]
    ControlCharacter(String),
    /// The path contains non-ASCII bytes and the policy does not admit them.
    #[error("path '{0}' contains non-ASCII bytes")]
    NonAscii(String),
    /// A component, or the whole path, exceeds its length limit.
    #[error("path '{0}' exceeds the length limit")]
    TooLong(String),
}

/// A relative path accepted by the canonical contract.
///
/// Construction is the only validation point: a `CanonicalPath` that exists is
/// a path a package may contain.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CanonicalPath(String);

impl CanonicalPath {
    /// Validates `raw` against the canonical path contract.
    ///
    /// `allow_non_ascii` comes from the Inspection policy rather than a
    /// caller's preference; see [`crate::Policy`]. When non-ASCII paths are
    /// admitted, [`CanonicalPath::collision_key`] no longer detects Unicode
    /// normalization collisions on its own — capture must then reject the
    /// package (louiselm-u530).
    ///
    /// # Errors
    /// Rejects empty/absolute paths, empty or relative components, controls, excessive length, and non-ASCII bytes when disallowed.
    pub fn parse(raw: &str, allow_non_ascii: bool) -> Result<Self, PathError> {
        if raw.is_empty() {
            return Err(PathError::Empty);
        }
        if raw.starts_with('/') {
            return Err(PathError::Absolute(raw.to_owned()));
        }
        if raw.len() > MAX_PATH_BYTES {
            return Err(PathError::TooLong(raw.to_owned()));
        }
        if raw.chars().any(char::is_control) {
            return Err(PathError::ControlCharacter(raw.to_owned()));
        }
        if !allow_non_ascii && !raw.is_ascii() {
            return Err(PathError::NonAscii(raw.to_owned()));
        }
        let components = raw.split('/').collect::<Vec<_>>();
        for component in &components {
            if component.is_empty() {
                return Err(PathError::EmptyComponent(raw.to_owned()));
            }
            if *component == "." || *component == ".." {
                return Err(PathError::RelativeComponent(raw.to_owned()));
            }
            if component.len() > MAX_COMPONENT_BYTES {
                return Err(PathError::TooLong(raw.to_owned()));
            }
        }
        Ok(Self(raw.to_owned()))
    }

    /// Borrows the path as it appears in a manifest.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns the key two paths share when a filesystem would confuse them.
    ///
    /// ASCII case folding covers the practical collision — a case-insensitive
    /// or case-preserving filesystem materializing `Skill.md` and `skill.md`
    /// as one file — for the ASCII-only paths the default policy admits.
    #[must_use]
    pub fn collision_key(&self) -> String {
        self.0.to_ascii_lowercase()
    }

    /// Returns the final component, which names the file itself.
    #[must_use]
    pub fn file_name(&self) -> &str {
        self.0.rsplit('/').next().unwrap_or(&self.0)
    }

    /// Returns the lowercase extension without its dot, when the name has one.
    #[must_use]
    pub fn extension(&self) -> Option<String> {
        let name = self.file_name();
        let (stem, extension) = name.rsplit_once('.')?;
        if stem.is_empty() || extension.is_empty() {
            return None;
        }
        Some(extension.to_ascii_lowercase())
    }
}

impl fmt::Display for CanonicalPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A SHA-256 content address.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Digest(String);

/// A malformed digest string.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("'{0}' is not a sha256 digest")]
pub struct DigestError(String);

impl Digest {
    /// Computes the digest of `bytes`.
    #[must_use]
    pub fn of(bytes: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        Self(hex(&hasher.finalize()))
    }

    /// Parses `sha256:<hex>`, `sha256-<hex>`, or a bare lowercase hex digest.
    ///
    /// # Errors
    /// Rejects any digest whose payload is not exactly 64 lowercase hexadecimal characters.
    pub fn parse(raw: &str) -> Result<Self, DigestError> {
        let hex_part = raw
            .strip_prefix("sha256:")
            .or_else(|| raw.strip_prefix("sha256-"))
            .unwrap_or(raw);
        let valid = hex_part.len() == 64
            && hex_part
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
        if !valid {
            return Err(DigestError(raw.to_owned()));
        }
        Ok(Self(hex_part.to_owned()))
    }

    /// Borrows the bare lowercase hex digest.
    #[must_use]
    pub fn hex(&self) -> &str {
        &self.0
    }

    /// Returns the directory name this digest owns in the immutable store.
    ///
    /// `:` is legal on Linux but hostile to shell completion, tarballs, and
    /// Windows-formatted removable media, so the store spells the separator
    /// `-` while every human- and robot-facing surface keeps `sha256:`.
    #[must_use]
    pub fn directory_name(&self) -> String {
        format!("sha256-{}", self.0)
    }

    /// Returns the first `length` hex characters, for compact display only.
    #[must_use]
    pub fn short(&self, length: usize) -> &str {
        &self.0[..length.min(self.0.len())]
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "sha256:{}", self.0)
    }
}

/// A streaming SHA-256 hasher, for content too large to hold in memory twice.
#[derive(Default)]
pub struct Hasher(Sha256);

impl Hasher {
    /// Starts an empty hash.
    #[must_use]
    pub fn new() -> Self {
        Self(Sha256::new())
    }

    /// Feeds the next chunk of content.
    pub fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    /// Consumes the hasher and returns the content address.
    #[must_use]
    pub fn finish(self) -> Digest {
        Digest(hex(&self.0.finalize()))
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut rendered = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        #[expect(
            clippy::expect_used,
            reason = "Formatting a u8 into String is infallible."
        )]
        write!(rendered, "{byte:02x}").expect("integer formatting into String is infallible");
    }
    rendered
}
