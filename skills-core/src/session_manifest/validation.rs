//! Structural validation shared by construction and closed canonical parsing.

use super::{
    BTreeSet, CanonicalPath, Digest, INPUT_MANIFEST_SCHEMA, MAX_INPUT_MANIFEST_BYTES,
    PROVIDER_DISCLOSURE_NOTICE, SessionInputManifest, SessionManifestError,
};

impl SessionInputManifest {
    pub(super) fn validate(&self) -> Result<(), SessionManifestError> {
        if self.schema != INPUT_MANIFEST_SCHEMA {
            return Err(SessionManifestError::UnsupportedSchema(self.schema.clone()));
        }
        if !self.acp_mcp_servers.is_empty() {
            return Err(SessionManifestError::NonEmptyMcp {});
        }
        identifier("agent", &self.agent.id)?;
        identifier("runtime_id", &self.agent.runtime_id)?;
        self.agent
            .provider
            .validate()
            .map_err(|_| malformed("agent", "invalid Provider configuration"))?;
        if self.runtime.runtime_id != self.agent.runtime_id {
            return Err(SessionManifestError::UnmeasuredRuntime {
                reason: "runtime does not match Agent registration",
            });
        }
        runtime(&self.runtime)?;
        for argument in &self.agent.arguments {
            if argument.contains('\0') {
                return Err(malformed(
                    "agent.arguments",
                    "NUL is not a process argument",
                ));
            }
        }
        for (key, value) in &self.agent.environment {
            if key.is_empty() || key.contains(['=', '\0']) || value.contains('\0') {
                return Err(malformed(
                    "agent.environment",
                    "invalid process environment",
                ));
            }
        }
        if let Some(integration) = &self.agent.tool_integration {
            nonempty("agent.tool_integration", integration)?;
        }
        digest(
            "skill_generation_id",
            &self.skill_generation.generation_digest,
            true,
        )?;
        digest("view_digest", &self.skill_generation.view_digest, true)?;
        digest("policy_digest", &self.policy_digest, true)?;
        identifier("isolation_receipt", &self.isolation_receipt)?;
        identifier("envelope_id", &self.envelope.id)?;
        for (field, entries) in [
            ("project_instructions", &self.project_instructions),
            ("tool_schemas", &self.tool_schemas),
            ("plugin_schemas", &self.plugin_schemas),
        ] {
            files(
                field,
                entries
                    .iter()
                    .map(|entry| (entry.path.as_str(), entry.sha256.as_str())),
            )?;
        }
        let expected = self
            .agent
            .reachable_providers()
            .into_iter()
            .collect::<Vec<_>>();
        if self.provider_disclosure.providers != expected
            || self.provider_disclosure.notice != PROVIDER_DISCLOSURE_NOTICE
        {
            return Err(malformed(
                "provider_disclosure",
                "disclosure does not match Agent registration",
            ));
        }
        if self.canonical_bytes().len() > MAX_INPUT_MANIFEST_BYTES {
            return Err(SessionManifestError::TooLarge);
        }
        Ok(())
    }
}

fn runtime(runtime: &crate::registry::RuntimeMeasurement) -> Result<(), SessionManifestError> {
    for (field, value) in [
        ("runtime.version", &runtime.version),
        ("runtime.origin", &runtime.origin),
        (
            "runtime.isolation_policy_version",
            &runtime.isolation_policy_version,
        ),
    ] {
        nonempty(field, value)?;
    }
    digest(
        "runtime.executable_sha256",
        &runtime.executable_sha256,
        false,
    )?;
    files(
        "runtime.adapters",
        runtime
            .adapters
            .iter()
            .map(|file| (file.path.as_str(), file.sha256.as_str())),
    )?;
    let mut libraries = BTreeSet::new();
    for library in &runtime.library_baseline {
        nonempty("runtime.library_baseline", library)?;
        if !libraries.insert(library) {
            return Err(SessionManifestError::Duplicate {
                field: "runtime.library_baseline",
            });
        }
    }
    Ok(())
}

fn malformed(field: &'static str, reason: &'static str) -> SessionManifestError {
    SessionManifestError::Malformed { field, reason }
}

fn nonempty(field: &'static str, value: &str) -> Result<(), SessionManifestError> {
    if value.trim().is_empty() {
        return Err(SessionManifestError::Missing { field });
    }
    Ok(())
}

fn identifier(field: &'static str, value: &str) -> Result<(), SessionManifestError> {
    nonempty(field, value)?;
    if value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(malformed(field, "invalid identifier"));
    }
    Ok(())
}

fn digest(field: &'static str, value: &str, prefixed: bool) -> Result<(), SessionManifestError> {
    nonempty(field, value)?;
    let parsed = Digest::parse(value).map_err(|_| malformed(field, "invalid SHA-256 digest"))?;
    let correct = if prefixed {
        parsed.to_string() == value
    } else {
        parsed.hex() == value
    };
    if !correct {
        return Err(malformed(field, "noncanonical SHA-256 spelling"));
    }
    Ok(())
}

fn files<'a>(
    field: &'static str,
    entries: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Result<(), SessionManifestError> {
    let mut keys = BTreeSet::new();
    for (path, sha256) in entries {
        let path = CanonicalPath::parse(path, false)
            .map_err(|_| malformed(field, "invalid snapshot path"))?;
        if !keys.insert(path.collision_key()) {
            return Err(SessionManifestError::Duplicate { field });
        }
        digest(field, sha256, false)?;
    }
    for key in &keys {
        for (index, _) in key.match_indices('/') {
            if keys.contains(&key[..index]) {
                return Err(malformed(field, "snapshot file is also a directory"));
            }
        }
    }
    Ok(())
}
