//! Lowercase hexadecimal encoding for persisted digests.

const DIGITS: &[u8; 16] = b"0123456789abcdef";

/// Encodes `bytes` as lowercase hex, two digits per byte.
///
/// The output matches what sha2 0.10's `{:x}` produced for a digest; stored
/// and compared digests depend on that exact form.
pub(crate) fn lower_hex(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    encoded
}

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};

    use super::lower_hex;

    #[test]
    fn sha256_digest_encodes_as_lowercase_hex() {
        assert_eq!(
            lower_hex(&Sha256::digest(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn every_byte_value_keeps_its_leading_zero() {
        assert_eq!(lower_hex(&[0x00, 0x0f, 0xa0, 0xff]), "000fa0ff");
        assert_eq!(lower_hex(&[]), "");
    }
}
