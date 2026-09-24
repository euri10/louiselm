//! Package identity and supported interfaces, independent of stored schemas.

use serde::{Deserialize, Serialize};

/// Bounded public identity; conveys no authorization or host information.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceMetadata {
    /// Packaged component name.
    pub component: String,
    /// Cargo package version, not an interface version.
    pub version: String,
    /// Independently versioned integration contracts.
    pub interfaces: Interfaces,
}

/// Supported interface revisions; increment when a consumed contract breaks.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Interfaces {
    /// Local capture command contract.
    pub capture: u32,
    /// Owner-only Run socket contract.
    pub run: u32,
    /// Owner-only Attention socket contract.
    pub attention: u32,
    /// Paired Android HTTPS contract.
    pub receiver: u32,
}

/// Return the identity compiled into this exact binary without I/O.
#[must_use]
pub fn metadata() -> ServiceMetadata {
    ServiceMetadata {
        component: "capture".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        interfaces: Interfaces {
            capture: 1,
            run: 1,
            attention: 1,
            receiver: 1,
        },
    }
}
