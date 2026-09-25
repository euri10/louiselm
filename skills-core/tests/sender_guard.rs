//! The launcher must carry its guard, even before production loading is enabled.

#![expect(clippy::unwrap_used, reason = "test fixtures report process failures")]

use std::process::Command;

#[test]
fn embedded_guard_has_socket_scoped_upstream_authority() {
    let object = libbpf_rs::ObjectBuilder::default()
        .open_memory(louiselm_skills::launch_supervisor::SENDER_GUARD_OBJECT)
        .unwrap();
    let maps: Vec<_> = object.maps().map(|map| map.name().to_owned()).collect();
    assert!(maps.iter().any(|name| name == "upstreams"));
    // No privileges or BPF load are needed to inspect the embedded ABI.
}

#[test]
fn launcher_exports_its_embedded_bpf_object_without_privilege() {
    let output = Command::new(env!("CARGO_BIN_EXE_louiselm-launch"))
        .arg("__sender-guard-object")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    assert_eq!(
        output.stdout,
        louiselm_skills::launch_supervisor::SENDER_GUARD_OBJECT
    );
    assert_eq!(&output.stdout[..6], b"\x7fELF\x02\x01");
    // ELF e_machine: EM_BPF, little-endian. This is an object, not host code.
    assert_eq!(&output.stdout[18..20], &[247, 0]);
}

#[test]
fn guard_export_refuses_a_caller_selected_object() {
    let output = Command::new(env!("CARGO_BIN_EXE_louiselm-launch"))
        .args(["__sender-guard-object", "/tmp/untrusted.bpf.o"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, b"louiselm-launch: unexpected arguments\n");
}
