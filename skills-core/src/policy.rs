//! The versioned, content-addressed Inspection policy and Unicode profile.
//!
//! Inspection is only as trustworthy as the rules it applies, so the rules
//! themselves are content-addressed and reported alongside every finding. The
//! default policy is compiled into the binary; a replacement is accepted only
//! when the caller states the digest it expects, which is what stops an
//! external feed — a vendor bundle, a synced dotfile, an Agent-written file —
//! from silently widening or weakening Inspection.

use std::{collections::BTreeMap, fs, io, path::Path};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::canonical::Digest;

/// The policy schema this build reads.
pub const POLICY_SCHEMA: &str = "louiselm.skills.policy/1";

const DEFAULT_POLICY: &str = include_str!("policy_default.json");

/// Capture and Inspection size limits, all enforced fail-closed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    /// Largest accepted single file.
    pub max_file_bytes: u64,
    /// Largest accepted package, summed over all files.
    pub max_total_bytes: u64,
    /// Largest accepted number of files in a package.
    pub max_entries: usize,
    /// Deepest accepted directory nesting, counting the package root as zero.
    pub max_depth: usize,
    /// Longest prefix of a text file Inspection scans in full.
    pub max_text_scan_bytes: u64,
}

/// Path rules that capture applies before a path may enter a manifest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathRules {
    /// Whether paths may contain non-ASCII bytes.
    ///
    /// The default is `false`. Admitting non-ASCII paths also admits Unicode
    /// normalization collisions that ASCII case folding cannot detect, so
    /// capture refuses the package until that gap is closed.
    pub allow_non_ascii: bool,
}

/// One named class of code points Inspection reports wherever it appears.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnicodeClass {
    /// Class name, reported verbatim in findings.
    pub name: String,
    /// Inclusive code point ranges belonging to the class.
    pub ranges: Vec<(u32, u32)>,
}

/// The Unicode profile: hidden-character classes and ASCII confusables.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnicodeProfile {
    /// Profile version, reported with every Unicode finding.
    pub profile_version: String,
    /// Code point classes reported on sight.
    pub classes: Vec<UnicodeClass>,
    /// Non-ASCII characters paired with the ASCII character they imitate.
    pub confusables: Vec<(String, String)>,
}

/// Thresholds and markers for encoded-payload detection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncodedPayloadRules {
    /// Shortest uninterrupted base64 run reported as a payload.
    pub min_base64_run: usize,
    /// Shortest uninterrupted hexadecimal run reported as a payload.
    pub min_hex_run: usize,
    /// Shortest run of `\x`/`\u` escapes reported as a payload.
    pub min_escape_run: usize,
    /// Substrings naming a decoder, reported wherever they appear.
    pub decoder_markers: Vec<String>,
}

/// The policy document, exactly as serialized.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyDocument {
    /// Schema identifier.
    pub schema: String,
    /// Policy version, reported in the Dossier.
    pub version: String,
    /// Size and shape limits.
    pub limits: Limits,
    /// Path admission rules.
    pub paths: PathRules,
    /// Unicode profile.
    pub unicode: UnicodeProfile,
    /// URL scheme prefixes reported wherever they appear in text.
    pub url_schemes: Vec<String>,
    /// Lowercase substrings that reference a credential.
    pub credential_markers: Vec<String>,
    /// Lowercase substrings that reach the network.
    pub network_imports: Vec<String>,
    /// Lowercase substrings that start a process or evaluate code.
    pub process_imports: Vec<String>,
    /// Lowercase substrings that make an SVG do something rather than draw.
    pub svg_behavior: Vec<String>,
    /// Encoded-payload thresholds and decoder markers.
    pub encoded_payload: EncodedPayloadRules,
    /// Extensions that declare compiled or archived content.
    pub declared_binary_extensions: Vec<String>,
    /// Extensions that declare image content.
    pub image_extensions: Vec<String>,
}

/// A policy that cannot be used.
#[derive(Debug, Error)]
pub enum PolicyError {
    /// The policy file could not be read.
    #[error("cannot read policy '{path}': {source}")]
    Read {
        /// Path the caller named.
        path: String,
        /// Underlying I/O failure.
        source: io::Error,
    },
    /// The policy is not valid JSON for this schema.
    #[error("policy is not valid: {0}")]
    Malformed(String),
    /// The policy declares a schema this build does not implement.
    #[error("unsupported policy schema '{0}'")]
    UnsupportedSchema(String),
    /// The policy on disk is not the policy the caller pinned.
    #[error("policy digest mismatch: expected {expected}, found {found}")]
    DigestMismatch {
        /// Digest the caller pinned.
        expected: String,
        /// Digest the file actually has.
        found: String,
    },
}

/// A loaded policy, bound to the digest of the bytes it came from.
#[derive(Clone, Debug)]
pub struct Policy {
    document: PolicyDocument,
    digest: Digest,
    confusables: BTreeMap<char, char>,
}

impl Policy {
    /// Returns the exact bytes of the policy compiled into this build.
    ///
    /// Printing these is how an operator obtains a starting point for a pinned
    /// replacement, and how the digest in a Dossier can be checked by hand.
    #[must_use]
    pub fn embedded_bytes() -> &'static [u8] {
        DEFAULT_POLICY.as_bytes()
    }

    /// Returns the policy compiled into this build.
    ///
    /// # Panics
    /// Panics if the trusted, compile-time `policy_default.json` is malformed or
    /// names an unsupported schema; caller-supplied policy bytes never reach this path.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "Only the versioned include_str policy asset is parsed; tests validate that asset."
    )]
    pub fn embedded() -> Self {
        Self::from_bytes(DEFAULT_POLICY.as_bytes())
            .expect("the embedded policy is validated by tests")
    }

    /// Loads a replacement policy, refusing bytes the caller did not pin.
    ///
    /// `expected` is mandatory by design: a policy accepted because it parsed
    /// is a policy an attacker may rewrite.
    ///
    /// # Errors
    /// Returns file-read errors, a digest mismatch, or [`Self::from_bytes`] errors.
    pub fn load(path: &Path, expected: &Digest) -> Result<Self, PolicyError> {
        let bytes = fs::read(path).map_err(|source| PolicyError::Read {
            path: path.display().to_string(),
            source,
        })?;
        let found = Digest::of(&bytes);
        if &found != expected {
            return Err(PolicyError::DigestMismatch {
                expected: expected.to_string(),
                found: found.to_string(),
            });
        }
        Self::from_bytes(&bytes)
    }

    /// Parses policy bytes and binds them to their digest.
    ///
    /// # Errors
    /// Rejects malformed JSON or an unsupported policy schema.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, PolicyError> {
        let document: PolicyDocument = serde_json::from_slice(bytes)
            .map_err(|error| PolicyError::Malformed(error.to_string()))?;
        if document.schema != POLICY_SCHEMA {
            return Err(PolicyError::UnsupportedSchema(document.schema));
        }
        let confusables = document
            .unicode
            .confusables
            .iter()
            .filter_map(|(from, to)| Some((from.chars().next()?, to.chars().next()?)))
            .collect();
        Ok(Self {
            digest: Digest::of(bytes),
            document,
            confusables,
        })
    }

    /// Borrows the policy document.
    #[must_use]
    pub fn document(&self) -> &PolicyDocument {
        &self.document
    }

    /// Returns the content address of the exact policy bytes in force.
    #[must_use]
    pub fn digest(&self) -> &Digest {
        &self.digest
    }

    /// Borrows the limits.
    #[must_use]
    pub fn limits(&self) -> &Limits {
        &self.document.limits
    }

    /// Reports whether capture may admit non-ASCII paths.
    #[must_use]
    pub fn allows_non_ascii_paths(&self) -> bool {
        self.document.paths.allow_non_ascii
    }

    /// Returns the class name for `codepoint`, when the profile names one.
    #[must_use]
    pub fn unicode_class(&self, codepoint: u32) -> Option<&str> {
        self.document
            .unicode
            .classes
            .iter()
            .find(|class| {
                class
                    .ranges
                    .iter()
                    .any(|(start, end)| (*start..=*end).contains(&codepoint))
            })
            .map(|class| class.name.as_str())
    }

    /// Returns the ASCII character `character` imitates, when it imitates one.
    #[must_use]
    pub fn confusable_target(&self, character: char) -> Option<char> {
        self.confusables.get(&character).copied()
    }

    /// Reports whether `extension` declares compiled or archived content.
    #[must_use]
    pub fn is_declared_binary_extension(&self, extension: &str) -> bool {
        self.document
            .declared_binary_extensions
            .iter()
            .any(|known| known == extension)
    }

    /// Reports whether `extension` declares image content.
    #[must_use]
    pub fn is_image_extension(&self, extension: &str) -> bool {
        self.document
            .image_extensions
            .iter()
            .any(|known| known == extension)
    }
}
