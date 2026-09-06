//! The robot view of a Dossier.
//!
//! This is the same normalized state the human view renders, serialized whole.
//! An Agent reading it gets versioned fields, typed findings, stable
//! identifiers, and explicit next actions — and no way to be shown a fact the
//! human view does not also carry.

use crate::dossier::Dossier;

/// Serializes a Dossier as the robot view.
///
/// # Errors
/// Returns a JSON serialization error if the Dossier schema cannot be encoded.
pub fn json(dossier: &Dossier) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(dossier)
}

/// Serializes any robot payload with the same conventions.
///
/// # Errors
/// Returns errors from `T`'s serializer, including map keys JSON cannot represent.
pub fn payload<T: serde::Serialize>(value: &T) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(value)
}
