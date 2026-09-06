//! Behavioral coverage for launch.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]

//! Resolving a closed [`LaunchRequest`] into a plan a backend can spawn.

mod support;

use std::{fs, io::Read, os::unix::fs::PermissionsExt, path::Path};

use louiselm_skills::{
    canonical::Digest,
    launch::{
        self, LaunchError, LaunchRequest, MAX_REQUEST_BYTES, PROTOCOL_VERSION, REQUEST_SCHEMA,
    },
    registry::{MAX_REGISTRY_BYTES, NetworkPolicy, Registry, RegistryError},
    sandbox::{Backend, BubblewrapBackend, IdentityPlan},
};
use support::{Fixture, write_file, write_registry};

fn valid_request() -> LaunchRequest {
    LaunchRequest {
        schema: REQUEST_SCHEMA.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "request-abc123".to_owned(),
        authorization_id: "authorization-abc123".to_owned(),
        session_id: "session-abc123".to_owned(),
        run_id: "run-xyz789".to_owned(),
        agent_id: "demo".to_owned(),
        envelope_id: "denied".to_owned(),
        envelope_revision: 7,
        skill_generation_id: Digest::of(b"generation").to_string(),
        session_input_manifest_id: Digest::of(b"manifest").to_string(),
    }
}

/// Writes a working registry (agent "demo" / runtime "demo-runtime" / envelope
/// "denied") whose runtime executable runs `script`, and opens it.
fn registry_with_runtime(fixture: &Fixture, script: &str) -> (Registry, std::path::PathBuf) {
    let registry_root = fixture.path("registry");
    let runtime_root = fixture.path("runtime");
    let executable = runtime_root.join("bin/agent");
    write_file(&executable, script);
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))
        .expect("script is chmod +x");
    write_file(&runtime_root.join("lib/adapter.js"), "// adapter\n");
    write_registry(&registry_root, &runtime_root);
    (
        Registry::open(&registry_root).expect("the registry opens"),
        runtime_root,
    )
}

#[test]
fn resolve_builds_a_plan_from_registered_agent_runtime_and_envelope() {
    let fixture = Fixture::new();
    let (registry, runtime_root) = registry_with_runtime(&fixture, "#!/bin/sh\nexec cat\n");
    let sessions_root = fixture.path("sessions");
    let request = valid_request();

    let resolution = launch::resolve(
        &request,
        &registry,
        &sessions_root,
        IdentityPlan::NamespaceOnly,
    )
    .expect("a well-formed request against a matching registry resolves");

    assert_eq!(resolution.plan.session_id, request.session_id);
    assert_eq!(resolution.plan.runtime_root, runtime_root);
    assert_eq!(resolution.plan.executable, runtime_root.join("bin/agent"));
    assert_eq!(resolution.plan.arguments, vec!["--acp".to_owned()]);
    assert_eq!(
        resolution.plan.environment.get("LOUISELM_SESSION"),
        Some(&"1".to_owned()),
    );
    let session_directory = sessions_root.join(&request.session_id);
    assert_eq!(resolution.plan.home, session_directory.join("home"));
    assert_eq!(
        resolution.plan.workspace,
        session_directory.join("workspace")
    );
    assert_eq!(resolution.plan.network, NetworkPolicy::Denied);
    assert_eq!(resolution.runtime.runtime_id, "demo-runtime");
}

#[test]
fn resolve_refuses_an_unregistered_agent() {
    let fixture = Fixture::new();
    let (registry, _runtime_root) = registry_with_runtime(&fixture, "#!/bin/sh\nexec cat\n");
    let mut request = valid_request();
    request.agent_id = "not-registered".to_owned();

    let error = launch::resolve(
        &request,
        &registry,
        &fixture.path("sessions"),
        IdentityPlan::NamespaceOnly,
    )
    .expect_err("an unregistered agent cannot resolve");
    assert!(
        matches!(
            error,
            LaunchError::Registry(RegistryError::Unknown { kind: "agent", .. })
        ),
        "unexpected error: {error}",
    );
}

#[test]
fn resolve_refuses_a_runtime_that_changed_since_registration() {
    let fixture = Fixture::new();
    let (registry, runtime_root) = registry_with_runtime(&fixture, "#!/bin/sh\nexec cat\n");
    // Exactly what a self-updating Provider runtime does, after registration.
    write_file(
        &runtime_root.join("bin/agent"),
        "#!/bin/sh\nexec cat # updated\n",
    );

    let error = launch::resolve(
        &valid_request(),
        &registry,
        &fixture.path("sessions"),
        IdentityPlan::NamespaceOnly,
    )
    .expect_err("a mutated runtime is refused");
    assert!(
        matches!(
            error,
            LaunchError::Registry(RegistryError::RuntimeChanged { .. })
        ),
        "unexpected error: {error}",
    );
}

#[test]
fn resolve_refuses_a_session_id_that_would_escape_its_directory() {
    let fixture = Fixture::new();
    let registry =
        Registry::open(&fixture.path("empty-registry")).expect("an absent registry opens empty");

    for hostile in ["../../etc", "a/b", "", "a b"] {
        let mut request = valid_request();
        request.session_id = hostile.to_owned();

        let error = launch::resolve(
            &request,
            &registry,
            &fixture.path("sessions"),
            IdentityPlan::NamespaceOnly,
        )
        .expect_err(&format!("session_id {hostile:?} must be refused"));
        assert!(
            matches!(
                error,
                LaunchError::MalformedIdentifier {
                    field: "session_id",
                    ..
                }
            ),
            "unexpected error for {hostile:?}: {error}",
        );
    }
}

#[test]
fn resolve_refuses_a_malformed_skill_generation_id() {
    let fixture = Fixture::new();
    let registry =
        Registry::open(&fixture.path("empty-registry")).expect("an absent registry opens empty");
    let mut request = valid_request();
    request.skill_generation_id = "not-a-digest".to_owned();

    let error = launch::resolve(
        &request,
        &registry,
        &fixture.path("sessions"),
        IdentityPlan::NamespaceOnly,
    )
    .expect_err("a malformed digest is refused");
    assert!(
        matches!(
            error,
            LaunchError::MalformedIdentifier {
                field: "skill_generation_id",
                ..
            }
        ),
        "unexpected error: {error}",
    );

    let mut alias = valid_request();
    alias.skill_generation_id = alias.skill_generation_id[7..].to_owned();
    assert!(matches!(
        alias.validate(),
        Err(LaunchError::MalformedIdentifier {
            field: "skill_generation_id",
            ..
        })
    ));
}

#[test]
fn resolve_refuses_a_request_naming_the_wrong_schema() {
    let fixture = Fixture::new();
    let registry =
        Registry::open(&fixture.path("empty-registry")).expect("an absent registry opens empty");
    let mut request = valid_request();
    request.schema = "louiselm.launch.request/99".to_owned();

    let error = launch::resolve(
        &request,
        &registry,
        &fixture.path("sessions"),
        IdentityPlan::NamespaceOnly,
    )
    .expect_err("a foreign schema is refused");
    assert!(
        matches!(error, LaunchError::Schema { .. }),
        "unexpected error: {error}",
    );
}

#[test]
fn launch_requests_are_canonical_versioned_and_bounded() {
    let request = valid_request();
    let bytes = request.canonical_bytes();

    assert_eq!(
        LaunchRequest::parse_canonical(&bytes).expect("canonical request parses"),
        request,
    );
    assert_eq!(request.digest(), Digest::of(&bytes));

    let mut noncanonical = bytes.clone();
    noncanonical.push(b'\n');
    assert!(matches!(
        LaunchRequest::parse_canonical(&noncanonical),
        Err(LaunchError::NonCanonical)
    ));

    let oversized = vec![b' '; MAX_REQUEST_BYTES + 1];
    assert!(matches!(
        LaunchRequest::parse_canonical(&oversized),
        Err(LaunchError::RequestTooLarge { .. })
    ));

    let mut wrong_version = request;
    wrong_version.protocol_version += 1;
    assert!(matches!(
        wrong_version.validate(),
        Err(LaunchError::ProtocolVersion { .. })
    ));
}

#[test]
fn launch_request_authority_fields_are_bounded_identifiers() {
    let request = valid_request();
    for field in ["request_id", "authorization_id"] {
        for hostile in ["", "../escape", "has space", "non-ascii-é"] {
            let mut candidate = request.clone();
            match field {
                "request_id" => candidate.request_id = hostile.to_owned(),
                "authorization_id" => candidate.authorization_id = hostile.to_owned(),
                _ => unreachable!(),
            }
            assert!(matches!(
                candidate.validate(),
                Err(LaunchError::MalformedIdentifier {
                    field: found,
                    ..
                }) if found == field
            ));
        }
    }
}

#[test]
fn deserializing_a_request_with_an_extra_field_fails_closed() {
    let request = valid_request();
    let base = serde_json::to_value(request).expect("request serializes");
    for (field, value) in [
        ("command", serde_json::json!("whoami")),
        ("environment", serde_json::json!({ "TOKEN": "secret" })),
        ("mount", serde_json::json!("/")),
        ("path", serde_json::json!("/bin/sh")),
        ("identity", serde_json::json!(0)),
        ("backend", serde_json::json!("none")),
        ("policy", serde_json::json!("allow-all")),
        ("systemd", serde_json::json!({ "property": "Delegate=yes" })),
    ] {
        let mut hostile = base.clone();
        hostile
            .as_object_mut()
            .expect("request is an object")
            .insert(field.to_owned(), value);
        let result: Result<LaunchRequest, _> = serde_json::from_value(hostile);
        assert!(
            result.is_err(),
            "a request carrying '{field}' must fail closed"
        );
    }
}

#[test]
fn a_resolved_plan_actually_spawns_through_the_bubblewrap_backend() {
    let fixture = Fixture::new();
    let (registry, _runtime_root) =
        registry_with_runtime(&fixture, "#!/bin/sh\necho launched-via-request\n");
    let sessions_root = fixture.path("sessions");

    let resolution = launch::resolve(
        &valid_request(),
        &registry,
        &sessions_root,
        IdentityPlan::NamespaceOnly,
    )
    .expect("resolution succeeds");

    let backend =
        BubblewrapBackend::new().with_bootstrap(Path::new(env!("CARGO_BIN_EXE_louiselm-launch")));
    let mut session = backend
        .spawn(&resolution.plan)
        .expect("bwrap starts the resolved plan");

    let mut stdout = String::new();
    session
        .take_stdout()
        .expect("stdout is piped")
        .read_to_string(&mut stdout)
        .expect("stdout reads to EOF");
    let exit_code = session.wait().expect("the session exits");

    assert_eq!(exit_code, 0);
    assert_eq!(stdout.trim(), "launched-via-request");

    session.dispose().expect("disposal succeeds");
}

#[test]
fn trusted_registry_rejects_writable_authority_before_lookup() {
    let fixture = Fixture::new();
    let registry_root = fixture.path("registry");
    let runtime_root = fixture.path("runtime");
    write_file(&runtime_root.join("bin/agent"), "#!/bin/sh\nexec cat\n");
    fs::set_permissions(
        runtime_root.join("bin/agent"),
        fs::Permissions::from_mode(0o755),
    )
    .expect("script is chmod +x");
    write_file(&runtime_root.join("lib/adapter.js"), "// adapter\n");
    write_registry(&registry_root, &runtime_root);
    fs::set_permissions(&registry_root, fs::Permissions::from_mode(0o777))
        .expect("registry becomes writable");

    let error = Registry::open_trusted(&registry_root)
        .expect_err("a writable registry is never launch authority");

    assert!(matches!(error, RegistryError::Untrusted { .. }));
}

#[test]
fn registry_documents_are_bounded_and_duplicate_ids_are_rejected() {
    let fixture = Fixture::new();
    let registry_root = fixture.path("registry");
    write_file(
        &registry_root.join("agents.json"),
        &" ".repeat(MAX_REGISTRY_BYTES + 1),
    );

    assert!(matches!(
        Registry::open(&registry_root),
        Err(RegistryError::Oversized { kind: "agents" })
    ));

    write_file(
        &registry_root.join("agents.json"),
        r#"{"schema":"louiselm.launch.registry/1","entries":[{"id":"same","provider":"one","runtime_id":"runtime","arguments":[],"environment":{}},{"id":"same","provider":"two","runtime_id":"runtime","arguments":[],"environment":{}}]}"#,
    );

    assert!(matches!(
        Registry::open(&registry_root),
        Err(RegistryError::Duplicate { kind: "agent", .. })
    ));
}

#[test]
fn registry_rejects_runtime_paths_that_escape_the_registered_root() {
    let fixture = Fixture::new();
    let registry_root = fixture.path("registry");
    write_file(
        &registry_root.join("runtimes.json"),
        r#"{"schema":"louiselm.launch.registry/1","entries":[{"id":"runtime","root":"/tmp/runtime","executable":"../agent","executable_sha256":"00","adapters":[],"version":"1","origin":"test","library_baseline":[],"isolation_policy_version":"louiselm.isolation/1"}]}"#,
    );

    assert!(matches!(
        Registry::open(&registry_root),
        Err(RegistryError::Malformed { kind, .. }) if kind == "runtime"
    ));
}
