//! The Merkle tree behind one Endorsement batch signature.
//!
//! Shape and hashing follow RFC 9162 §2.1: a leaf hashes as
//! `SHA-256(0x00 || leaf bytes)`, an interior node as
//! `SHA-256(0x01 || left || right)`, and a tree of `n > 1` leaves splits at the
//! largest power of two below `n`. The two prefixes keep a leaf from ever
//! being read as an interior node. Inclusion proofs are RFC 9162 audit paths,
//! so another implementation can verify a record without this crate.

use sha2::{Digest as _, Sha256};

/// One SHA-256 tree node.
pub(super) type Node = [u8; 32];

const LEAF_PREFIX: u8 = 0x00;
const NODE_PREFIX: u8 = 0x01;

/// Hashes exact leaf bytes.
pub(super) fn leaf_hash(bytes: &[u8]) -> Node {
    let mut hasher = Sha256::new();
    hasher.update([LEAF_PREFIX]);
    hasher.update(bytes);
    hasher.finalize().into()
}

fn node_hash(left: &Node, right: &Node) -> Node {
    let mut hasher = Sha256::new();
    hasher.update([NODE_PREFIX]);
    hasher.update(left);
    hasher.update(right);
    hasher.finalize().into()
}

/// Largest power of two strictly below `count`, for `count >= 2`.
fn split(count: usize) -> usize {
    let mut half = 1;
    while half * 2 < count {
        half *= 2;
    }
    half
}

/// Returns the tree head over `leaves`, or `None` for an empty tree.
pub(super) fn root(leaves: &[Node]) -> Option<Node> {
    match leaves {
        [] => None,
        [only] => Some(*only),
        _ => {
            let (left, right) = leaves.split_at(split(leaves.len()));
            Some(node_hash(&root(left)?, &root(right)?))
        }
    }
}

/// Returns the audit path for the leaf at `index`, nearest sibling first.
pub(super) fn path(leaves: &[Node], index: usize) -> Vec<Node> {
    let mut path = Vec::new();
    collect_path(leaves, index, &mut path);
    path
}

fn collect_path(leaves: &[Node], index: usize, path: &mut Vec<Node>) {
    if leaves.len() < 2 {
        return;
    }
    let (left, right) = leaves.split_at(split(leaves.len()));
    let (inner, inner_index, sibling) = if index < left.len() {
        (left, index, right)
    } else {
        (right, index - left.len(), left)
    };
    collect_path(inner, inner_index, path);
    if let Some(sibling) = root(sibling) {
        path.push(sibling);
    }
}

/// Verifies an RFC 9162 §2.1.3.2 inclusion proof.
///
/// An audit path does not bind `size` by itself: a path for a leaf in the
/// left subtree of a 3-leaf tree also verifies as a 4-leaf tree with the same
/// root. As in RFC 9162, `size` must come from signed data — here the batch
/// header's `leaf_count` — never from the unsigned part of a record.
pub(super) fn verify(leaf: &Node, index: u64, size: u64, path: &[Node], expected: &Node) -> bool {
    if index >= size {
        return false;
    }
    let (mut node_index, mut last) = (index, size - 1);
    let mut computed = *leaf;
    for sibling in path {
        if last == 0 {
            return false;
        }
        if node_index & 1 == 1 || node_index == last {
            computed = node_hash(sibling, &computed);
            while node_index & 1 == 0 && node_index != 0 {
                node_index >>= 1;
                last >>= 1;
            }
        } else {
            computed = node_hash(&computed, sibling);
        }
        node_index >>= 1;
        last >>= 1;
    }
    last == 0 && &computed == expected
}

/// Renders a node as `sha256:<64 lowercase hex>`.
pub(super) fn encode(node: &Node) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(71);
    text.push_str("sha256:");
    for byte in node {
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    text
}

/// Parses exactly `sha256:<64 lowercase hex>`.
pub(super) fn decode(text: &str) -> Option<Node> {
    let (pairs, rest) = text.strip_prefix("sha256:")?.as_bytes().as_chunks::<2>();
    if pairs.len() != 32 || !rest.is_empty() {
        return None;
    }
    let mut node = [0_u8; 32];
    for (byte, [high, low]) in node.iter_mut().zip(pairs) {
        *byte = (nibble(*high)? << 4) | nibble(*low)?;
    }
    Some(node)
}

fn nibble(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]
mod tests {
    use super::{decode, encode, leaf_hash, path, root, verify};

    #[test]
    fn every_leaf_of_every_size_proves_and_no_other_position_does() {
        for size in 1..=17_u8 {
            let leaves: Vec<_> = (0..size).map(|value| leaf_hash(&[value])).collect();
            let head = root(&leaves).unwrap();
            for (index, leaf) in leaves.iter().enumerate() {
                let proof = path(&leaves, index);
                let size = leaves.len() as u64;
                assert!(
                    verify(leaf, index as u64, size, &proof, &head),
                    "{size}/{index}"
                );
                for other in (0..size).filter(|other| *other != index as u64) {
                    assert!(
                        !verify(leaf, other, size, &proof, &head),
                        "{size}/{index}->{other}"
                    );
                }
                // No size+1 assertion: a path alone does not bind the size
                // (see `verify`); the signed header does.
            }
        }
    }

    #[test]
    fn rfc_9162_leaf_and_node_prefixes_are_applied() {
        let leaves = [leaf_hash(b"a"), leaf_hash(b"b")];
        let mut joined = vec![0x01];
        joined.extend_from_slice(&leaves[0]);
        joined.extend_from_slice(&leaves[1]);
        let expected: [u8; 32] = <sha2::Sha256 as sha2::Digest>::digest(&joined).into();
        assert_eq!(root(&leaves), Some(expected));
        let unprefixed: [u8; 32] = <sha2::Sha256 as sha2::Digest>::digest(b"a").into();
        assert_ne!(leaf_hash(b"a"), unprefixed);
    }

    #[test]
    fn nodes_round_trip_through_their_text_form_only() {
        let node = leaf_hash(b"x");
        assert_eq!(decode(&encode(&node)), Some(node));
        assert_eq!(decode(&encode(&node).to_uppercase()), None);
        assert_eq!(decode(&encode(&node).replace("sha256:", "sha256-")), None);
    }
}
