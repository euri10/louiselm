#![cfg(target_os = "linux")]

use std::process::Command;

#[test]
fn privileged_entrypoint_accepts_only_the_fixed_run_verb() {
    let binary = env!("CARGO_BIN_EXE_louiselm-launch");
    for arguments in [
        vec![],
        vec!["status"],
        vec!["run", "--broker", "/tmp/socket"],
    ] {
        let output = Command::new(binary)
            .args(&arguments)
            .output()
            .expect("launcher binary executes");
        assert!(!output.status.success(), "{arguments:?} must be rejected");
        assert!(output.stdout.is_empty(), "stdout is reserved for ACP bytes");
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            "louiselm-launch: expected exactly 'run'\n"
        );
    }
}
