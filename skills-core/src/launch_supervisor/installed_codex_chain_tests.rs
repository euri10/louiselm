//! A Codex-shaped measured chain for installed composition tests.
//!
//! No real Codex is needed: "node" is the system shell, `codex-acp.js` is a
//! script that runs `$CODEX_PATH` as a waited child (never `exec`), and "codex"
//! is the deterministic test Agent. The tree is therefore exactly adapter plus
//! one runtime child, as `louiselm.codex-acp-integration/1` requires.

use super::*;
use crate::registry::{MeasuredFile, RuntimeMeasurement};

const ADAPTER_SCRIPT: &[u8] = b"\"$CODEX_PATH\"\nstatus=$?\nexit \"$status\"\n";

fn shell() -> PathBuf {
    fs::canonicalize("/bin/sh").unwrap()
}

fn digest(bytes: &[u8]) -> String {
    Digest::of(bytes).hex().into()
}

/// The runtime measurement a Codex-chain registration must produce.
pub(super) fn measurement(binaries: &Path) -> RuntimeMeasurement {
    RuntimeMeasurement {
        runtime_id: "runtime".into(),
        executable_sha256: digest(&fs::read(shell()).unwrap()),
        adapters: vec![
            MeasuredFile {
                path: "codex-acp.js".into(),
                sha256: digest(ADAPTER_SCRIPT),
            },
            MeasuredFile {
                path: "codex".into(),
                sha256: digest(&fs::read(binaries.join("louiselm-tool-test-agent")).unwrap()),
            },
        ],
        version: "fixture".into(),
        origin: "fixture".into(),
    }
}

/// Replaces the fixture Agent with the Codex-shaped chain and restages inputs.
pub(super) fn install(root: &Path, registry: &Path, config: &LauncherConfig) {
    let binaries = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let runtime = root.join("runtime-codex");
    fs::create_dir(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o755)).unwrap();
    fs::copy(shell(), runtime.join("node")).unwrap();
    fs::write(runtime.join("codex-acp.js"), ADAPTER_SCRIPT).unwrap();
    fs::copy(
        binaries.join("louiselm-tool-test-agent"),
        runtime.join("codex"),
    )
    .unwrap();
    for (name, mode) in [("node", 0o555), ("codex-acp.js", 0o444), ("codex", 0o555)] {
        fs::set_permissions(runtime.join(name), fs::Permissions::from_mode(mode)).unwrap();
    }
    let measured = measurement(&binaries);
    registry_record(
        &registry.join("agents.json"),
        &serde_json::json!([{
            "id":"agent","provider":"fixture","runtime_id":"runtime","arguments":[],"environment":{},
            "tool_integration":crate::launch_supervisor::tool_integration::CODEX_CONTRACT
        }]),
    );
    registry_record(
        &registry.join("runtimes.json"),
        &serde_json::json!([{
            "id":"runtime","root":runtime,"executable":"node",
            "executable_sha256":measured.executable_sha256,"adapters":measured.adapters,
            "version":"fixture","origin":"fixture"
        }]),
    );
    workspace::stage_manifest(config, &workspace::fixture_manifest_for(true));
    fs::write(root.join("codex-chain"), b"").unwrap();
}
