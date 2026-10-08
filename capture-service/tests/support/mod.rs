//! Fixture helpers shared by integration tests.

use std::fmt::Write as _;

/// Encodes a digest as lowercase hex, independently of the crate's own encoder,
/// so fixtures check stored digests against the form sha2 0.10's `{:x}` wrote.
pub fn lower_hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut hex, byte| {
            write!(hex, "{byte:02x}").unwrap();
            hex
        })
}
