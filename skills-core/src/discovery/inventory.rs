//! Closed wire records; deserialization alone grants no evidence authority.

use super::{DiscoveryError, INVENTORY_PATH, INVENTORY_SCHEMA, MAX_DISCOVERY_BYTES, identifier};
use crate::{
    CanonicalPath, Digest,
    discovery_source::{Source, SourceKind, SourceRoot},
    registry::RuntimePackage,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, fs::File, io::Read};

/// Runtime-pinned inventory. New executable or inventory bytes need registration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inventory {
    /// Inventory schema.
    pub schema: String,
    /// Adapter implementation identity, never inferred from Provider branding.
    pub adapter_id: String,
    /// Adapter inventory version.
    pub version: String,
    /// Runtime this inventory describes.
    pub runtime_id: String,
    /// Bare lowercase executable SHA-256.
    pub executable_sha256: String,
    /// Complete source set, sorted by source ID.
    pub sources: Vec<Source>,
}

impl Inventory {
    /// Fixed canonical JSON encoding. Source order is normalized by ID.
    ///
    /// # Panics
    /// Only a future fallible serializer could fail; the closed schema is JSON-native.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "Derived closed fields are JSON-native, without custom serializers."
    )]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut ordered = self.clone();
        ordered.sources.sort_by(|a, b| a.id.cmp(&b.id));
        serde_json::to_vec(&ordered).expect("inventory is serializable")
    }

    /// Parses a bounded, closed and canonical inventory.
    ///
    /// # Errors
    /// Rejects missing categories, invalid/duplicate source paths and IDs,
    /// unsupported schema, unknown fields and alternate encodings.
    pub fn parse(bytes: &[u8]) -> Result<Self, DiscoveryError> {
        if bytes.len() > MAX_DISCOVERY_BYTES {
            return Err(DiscoveryError::Refused("inventory_too_large"));
        }
        let inventory: Self = serde_json::from_slice(bytes)?;
        if inventory.schema != INVENTORY_SCHEMA
            || !identifier(&inventory.adapter_id)
            || !identifier(&inventory.runtime_id)
            || inventory.version.trim().is_empty()
        {
            return Err(DiscoveryError::Refused("invalid_inventory"));
        }
        if Digest::parse(&inventory.executable_sha256).is_err()
            || inventory.executable_sha256.len() != 64
        {
            return Err(DiscoveryError::Refused("invalid_inventory"));
        }
        let mut ids = BTreeSet::new();
        let mut paths = BTreeSet::new();
        let mut kinds = BTreeSet::new();
        for source in &inventory.sources {
            let path = CanonicalPath::parse(&source.path, false)
                .map_err(|_| DiscoveryError::Refused("invalid_source_path"))?;
            if !identifier(&source.id)
                || !ids.insert(&source.id)
                || !paths.insert((source.root, path.collision_key()))
            {
                return Err(DiscoveryError::Refused("duplicate_or_invalid_source"));
            }
            if source.kind == SourceKind::ProjectInstructions
                && source.root != SourceRoot::Workspace
            {
                return Err(DiscoveryError::Refused("invalid_project_source"));
            }
            kinds.insert(source.kind);
        }
        if kinds != SourceKind::ALL.into_iter().collect() {
            return Err(DiscoveryError::Refused("incomplete_inventory"));
        }
        if inventory.canonical_bytes() != bytes {
            return Err(DiscoveryError::Refused("noncanonical_inventory"));
        }
        Ok(inventory)
    }

    pub(super) fn load(runtime: &RuntimePackage) -> Result<Self, DiscoveryError> {
        let entry = runtime
            .adapters
            .iter()
            .find(|entry| entry.path == INVENTORY_PATH)
            .ok_or(DiscoveryError::Refused("inventory_unregistered"))?;
        let path = runtime.root.join(INVENTORY_PATH);
        if !std::fs::symlink_metadata(&path)?.file_type().is_file() {
            return Err(DiscoveryError::Refused("inventory_not_regular"));
        }
        let mut bytes = Vec::new();
        File::open(path)?
            .take((MAX_DISCOVERY_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        if Digest::of(&bytes).hex() != entry.sha256 {
            return Err(DiscoveryError::Refused("inventory_changed"));
        }
        let inventory = Self::parse(&bytes)?;
        if inventory.runtime_id != runtime.id
            || inventory.executable_sha256 != runtime.executable_sha256
        {
            return Err(DiscoveryError::Refused("runtime_mismatch"));
        }
        Ok(inventory)
    }
}
