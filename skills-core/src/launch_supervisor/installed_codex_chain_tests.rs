//! Codex-contract chains for installed composition tests.
//!
//! The fixture chain needs no real Codex: "node" is the system shell,
//! `codex-acp.js` runs `$CODEX_PATH` as a waited child (never `exec`), and
//! "codex" is the deterministic test Agent. The stock chain copies the three
//! hash-pinned files from `scripts/fetch-stock-codex-chain`. Either way the
//! tree is exactly adapter plus one runtime child, as
//! `louiselm.codex-acp-integration/1` requires.

use super::*;
use crate::registry::{MeasuredFile, RegistryError, RuntimeMeasurement, RuntimePackage};

const ADAPTER_SCRIPT: &[u8] = b"\"$CODEX_PATH\"\nstatus=$?\nexit \"$status\"\n";
pub(super) const MISSING_RUNTIME_SCRIPT: &[u8] = b"read line\n";
pub(super) const WRONG_ANCESTRY_SCRIPT: &[u8] = b"/bin/sh -c '\"$CODEX_PATH\"; /bin/true'\n";

/// Where the installed Codex-contract runtime lives in a fixture root.
pub(super) fn runtime_directory(root: &Path) -> Option<PathBuf> {
    root.join("codex-chain")
        .exists()
        .then(|| root.join("runtime-codex"))
}

fn digest(path: &Path) -> String {
    Digest::of(&fs::read(path).unwrap()).hex().into()
}

/// The runtime measurement of an installed Codex-contract directory.
pub(super) fn measurement(runtime: &Path) -> RuntimeMeasurement {
    RuntimeMeasurement {
        runtime_id: "runtime".into(),
        executable_sha256: digest(&runtime.join("node")),
        adapters: ["codex-acp.js", "codex", "codex-code-mode-host"]
            .into_iter()
            .map(|name| MeasuredFile {
                path: name.into(),
                sha256: digest(&runtime.join(name)),
            })
            .collect(),
        version: "fixture".into(),
        origin: "fixture".into(),
    }
}

/// Replaces the fixture Agent with a Codex-contract chain and restages inputs.
///
/// `stock` names a directory produced by `scripts/fetch-stock-codex-chain`;
/// without it the shell/test-Agent fixture chain is installed.
pub(super) fn install(
    root: &Path,
    registry: &Path,
    config: &LauncherConfig,
    stock: Option<&Path>,
    synthetic_script: Option<&[u8]>,
) {
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
    if let Some(stock) = stock {
        for name in ["node", "codex-acp.js", "codex", "codex-code-mode-host"] {
            fs::copy(stock.join(name), runtime.join(name)).unwrap();
        }
    } else {
        fs::copy(fs::canonicalize("/bin/sh").unwrap(), runtime.join("node")).unwrap();
        fs::write(
            runtime.join("codex-acp.js"),
            synthetic_script.unwrap_or(ADAPTER_SCRIPT),
        )
        .unwrap();
        fs::copy(
            binaries.join("louiselm-tool-test-agent"),
            runtime.join("codex"),
        )
        .unwrap();
        fs::write(runtime.join("codex-code-mode-host"), b"fixture-host").unwrap();
    }
    for (name, mode) in [
        ("node", 0o555),
        ("codex-acp.js", 0o444),
        ("codex", 0o555),
        ("codex-code-mode-host", 0o555),
    ] {
        fs::set_permissions(runtime.join(name), fs::Permissions::from_mode(mode)).unwrap();
    }
    let measured = measurement(&runtime);
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
    workspace::stage_manifest(
        config,
        &workspace::fixture_manifest_for(Some(&runtime), crate::registry::NetworkPolicy::Denied),
    );
    fs::write(root.join("codex-chain"), b"").unwrap();
    if stock.is_some() {
        fs::write(root.join("stock-codex"), b"").unwrap();
    }
}

#[test]
fn stock_runtime_rejects_a_wrong_registered_hash() {
    let Some(stock) = std::env::var_os("LOUISELM_STOCK_CODEX_DIR") else {
        return;
    };
    let stock = PathBuf::from(stock);
    let measured = measurement(&stock);
    let mut runtime = RuntimePackage {
        id: "runtime".into(),
        root: stock,
        executable: "node".into(),
        executable_sha256: measured.executable_sha256,
        adapters: measured.adapters,
        version: "pinned".into(),
        origin: "pinned".into(),
    };
    assert!(runtime.measure().is_ok());
    runtime.adapters[0].sha256 = "0".repeat(64);
    assert!(
        matches!(runtime.measure(), Err(RegistryError::RuntimeChanged { path, .. }) if path == "codex-acp.js")
    );
}

#[test]
fn stock_tools_keep_neovim_logs_outside_source() {
    use crate::launch_supervisor::tool_integration;

    let Some(stock) = std::env::var_os("LOUISELM_STOCK_CODEX_DIR") else {
        return;
    };
    assert!(
        rustix::process::geteuid().is_root(),
        "requires the disposable VM"
    );
    let stock = fs::canonicalize(stock).unwrap();
    // Unlike /tmp, this home is not writable inside Codex's workspace sandbox.
    // Run 6d48a722-a0cb-483e-be16-8448794fd0fd exposed Neovim's cwd fallback.
    let root = tempfile::Builder::new()
        .prefix("louiselm-nvim-log-")
        .tempdir_in("/var/tmp")
        .unwrap();
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    fs::create_dir(&home).unwrap();
    fs::create_dir(&workspace).unwrap();
    let (_, environment) =
        tool_integration::launch_values(tool_integration::Integration::CodexAcp, &stock, &home);
    let config: serde_json::Value = serde_json::from_str(&environment["CODEX_CONFIG"]).unwrap();
    let mut arguments = vec!["sandbox".into(), "-P".into(), ":workspace".into()];
    // Exercise the real shell environment policy, not a pre-injected test env.
    if let Some(settings) = config["shell_environment_policy"]["set"].as_object() {
        for (key, value) in settings {
            arguments.extend([
                "-c".into(),
                format!("shell_environment_policy.set.{key}={value}"),
            ]);
        }
    }
    arguments.extend([
        "--".into(),
        "/bin/sh".into(),
        "-c".into(),
        concat!(
            "set -eu\n",
            "if touch \"$HOME/forbidden\" 2>/dev/null; then exit 10; fi\n",
            "/usr/lib/louiselm/verifier/bin/nvim --headless --clean ",
            "-c \"lua local ok, err = pcall(vim.fn.serverstart, '/missing/log-probe.sock'); ",
            "assert(not ok and type(err) == 'string')\" -c 'qa!'\n",
            "test ! -e nvim.log && test -s \"$NVIM_LOG_FILE\"\n"
        )
        .into(),
    ]);
    // Own private /tmp and denied network, without touching the VM's installed
    // broker or replaying a paid Run. Codex supplies the real inner sandbox.
    let output = Command::new("/usr/bin/unshare")
        .args([
            "--mount",
            "--net",
            "--propagation",
            "private",
            "--",
            "/bin/sh",
            "-c",
            "mount -t tmpfs -o mode=1777 tmpfs /tmp && exec \"$@\"",
            "log-probe",
        ])
        .arg(stock.join("codex"))
        .args(arguments)
        .current_dir(&workspace)
        .env_clear()
        .envs(environment)
        .output()
        .unwrap();
    assert!(
        !workspace.join("nvim.log").exists(),
        "Neovim leaked its diagnostic log into candidate source"
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
