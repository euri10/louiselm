//! Random paper-secret encoding, deliberately without Display or serialization.

use std::{fmt, io};

use bip39::{Language, Mnemonic};
use rustix::rand::{GetRandomFlags, getrandom};
use zeroize::Zeroizing;

use super::PaperError;
use crate::canonical::Hasher;

/// A 256-bit, checksummed English paper secret. Debug output is always redacted.
///
/// Parsed input is for authentication/confirmation, not choosing a new secret.
/// New enrollment must use [`Self::generate`]. Never reuse a wallet seed here.
/// Owned mnemonic storage is zeroized on drop; callers must also protect input
/// buffers, terminal capture, and process memory. This cannot erase screenshots.
pub struct PaperPhrase(Mnemonic);

impl fmt::Debug for PaperPhrase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PaperPhrase([REDACTED])")
    }
}

impl PaperPhrase {
    /// Generates 24 words using the operating system CSPRNG.
    ///
    /// # Errors
    /// Propagates randomness failures; refuses impossible encoding failures.
    pub fn generate() -> Result<Self, PaperError> {
        let mut entropy = Zeroizing::new([0_u8; 32]);
        let mut offset = 0;
        while offset < entropy.len() {
            match getrandom(&mut entropy[offset..], GetRandomFlags::empty()) {
                Ok(0) => {
                    return Err(PaperError::Io(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "OS randomness ended",
                    )));
                }
                Ok(count) => offset += count,
                Err(rustix::io::Errno::INTR) => (),
                Err(error) => return Err(PaperError::Io(error.into())),
            }
        }
        Mnemonic::from_entropy(&*entropy)
            .map(Self)
            .map_err(|_| PaperError::InvalidPhrase)
    }

    /// Parses a 24-word English phrase, checking its checksum without echoing input.
    ///
    /// # Errors
    /// Returns [`PaperError::InvalidPhrase`] for wrong length, words, or checksum.
    pub fn parse(input: &str) -> Result<Self, PaperError> {
        if input.len() > 512 || !input.is_ascii() {
            return Err(PaperError::InvalidPhrase);
        }
        let mnemonic = Mnemonic::parse_in_normalized(Language::English, input)
            .map_err(|_| PaperError::InvalidPhrase)?;
        if mnemonic.word_count() != 24 {
            return Err(PaperError::InvalidPhrase);
        }
        Ok(Self(mnemonic))
    }

    /// Exposes words only to the trusted local display. Never log the result.
    #[must_use]
    pub fn expose_secret(&self) -> Zeroizing<String> {
        // Reserve above the maximum English phrase length, avoiding reallocations
        // that could leave old, unwiped word buffers behind.
        let mut exposed = Zeroizing::new(String::with_capacity(512));
        for word in self.0.words() {
            if !exposed.is_empty() {
                exposed.push(' ');
            }
            exposed.push_str(word);
        }
        exposed
    }

    /// Returns the domain-separated verifier of this high-entropy secret.
    ///
    /// This is not a password KDF: generation supplies 256 random bits. The final
    /// entropy input is fixed at 32 bytes, so the domain boundary is unambiguous.
    /// Comparing these hashes compares public verifiers, not secret word prefixes.
    #[must_use]
    pub fn verifier(&self, domain: &str) -> String {
        let (entropy, _) = self.0.to_entropy_array();
        let entropy = Zeroizing::new(entropy);
        let mut hasher = Hasher::new();
        hasher.update(b"louiselm.paper-verifier/1\0");
        hasher.update(domain.as_bytes());
        hasher.update(b"\0");
        hasher.update(&entropy[..32]);
        hasher.finish().to_string()
    }
}
