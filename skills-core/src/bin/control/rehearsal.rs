//! One explicit, no-prompt startup rehearsal through the installed tools.
use louiselm_skills::broker::operator::InspectError;
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::Path,
    process::{Command, Stdio},
};

const COORDINATOR: &str = include_str!("../../../../scripts/bead-rehearse.lua");

pub(super) fn cli(verb: &str, arguments: &clap::ArgMatches) -> u8 {
    match run(verb, arguments) {
        Ok(code) => code,
        Err(error) => {
            if std::io::stderr()
                .lock()
                .write_all(&error.canonical_bytes())
                .is_err()
            {
                return InspectError::StatusUnavailable.exit_code();
            }
            error.exit_code()
        }
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "One CLI invocation validates and owns its private selection, input directory and headless child until completion."
)]
fn run(verb: &str, arguments: &clap::ArgMatches) -> Result<u8, InspectError> {
    let plugin = arguments
        .get_one::<String>("plugin")
        .ok_or(InspectError::InvalidRequest)?;
    if !Path::new(plugin).is_absolute() {
        return Err(InspectError::InvalidRequest);
    }
    let plugin_digest = louiselm_skills::cache::CacheBase::capture(&Path::new(plugin).join("lua"))
        .map_err(|_| InspectError::InvalidRequest)?
        .digest()
        .to_string();
    let selection_path = arguments
        .get_one::<String>("selection")
        .ok_or(InspectError::InvalidRequest)?;
    let selection_path = Path::new(selection_path);
    let file = fs::File::from(
        rustix::fs::open(
            selection_path,
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOFOLLOW,
            rustix::fs::Mode::empty(),
        )
        .map_err(|_| InspectError::InvalidRequest)?,
    );
    let metadata = file.metadata().map_err(|_| InspectError::InvalidRequest)?;
    if !selection_path.is_absolute()
        || !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.mode() & 0o7777 != 0o600
        || metadata.len() > 65536
    {
        return Err(InspectError::InvalidRequest);
    }
    let mut bytes = Vec::new();
    file.take(65537)
        .read_to_end(&mut bytes)
        .map_err(|_| InspectError::InvalidRequest)?;
    let selection: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| InspectError::InvalidRequest)?;
    let worktree = selection["worktree"]["path"]
        .as_str()
        .ok_or(InspectError::InvalidRequest)?;
    let parent = selection["snapshot_parent"]
        .as_str()
        .ok_or(InspectError::InvalidRequest)?;
    let group = selection["input_group"]
        .as_u64()
        .and_then(|gid| u32::try_from(gid).ok())
        .ok_or(InspectError::InvalidRequest)?;
    if selection["schema"] != "louiselm.operator.bead-run/2"
        || !Path::new(worktree).is_absolute()
        || !Path::new(parent).is_absolute()
    {
        return Err(InspectError::InvalidRequest);
    }
    let paths = super::installed_paths().map_err(|_| InspectError::BrokerUnavailable)?;
    let config = louiselm_skills::launcher_install::public_runtime_config(&paths)
        .map_err(|_| InspectError::BrokerUnavailable)?;
    if config.operator_uid != rustix::process::getuid().as_raw() {
        return Err(InspectError::AuthenticationRefused);
    }
    let scratch = tempfile::Builder::new()
        .prefix("rehearsal-")
        .tempdir_in(parent)
        .map_err(|_| InspectError::InvalidRequest)?;
    rustix::fs::chown(
        scratch.path(),
        None,
        Some(rustix::process::Gid::from_raw(group)),
    )
    .map_err(|_| InspectError::InvalidRequest)?;
    fs::set_permissions(scratch.path(), fs::Permissions::from_mode(0o750))
        .map_err(|_| InspectError::InvalidRequest)?;
    let private_selection = scratch.path().join("selection.json");
    fs::write(&private_selection, &bytes).map_err(|_| InspectError::StatusUnavailable)?;
    fs::set_permissions(&private_selection, fs::Permissions::from_mode(0o600))
        .map_err(|_| InspectError::StatusUnavailable)?;
    // The coordinator bytes come from this signed binary. Its Lua dependencies
    // are the exact selected source tree, also bound into the staged manifest.
    let mut command = Command::new("/usr/lib/louiselm/verifier/bin/nvim");
    command
        .args([
            "--headless",
            "--noplugin",
            "-u",
            "NONE",
            "-i",
            "NONE",
            "--cmd",
            "lua vim.opt.rtp:prepend(vim.env.LOUISELM_REHEARSAL_SOURCE)",
            "-c",
        ])
        .arg(format!("lua {COORDINATOR}"))
        .env_clear()
        .current_dir("/")
        .env(
            "PATH",
            "/usr/local/lib/louiselm/current/bin:/usr/local/lib/louiselm-tools:/usr/bin:/bin",
        )
        .env("LOUISELM_REHEARSAL_SOURCE", plugin)
        .env("LOUISELM_REHEARSAL_PLUGIN_DIGEST", plugin_digest)
        .env("LOUISELM_REHEARSAL_SELECTION", private_selection)
        .env("LOUISELM_REHEARSAL_PARENT", scratch.path())
        .env(
            "LOUISELM_REHEARSAL_CHECK",
            if verb == "rehearsal-check" { "1" } else { "0" },
        )
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    for name in ["HOME", "XDG_STATE_HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    let status = command
        .status()
        .map_err(|_| InspectError::StatusUnavailable)?;
    Ok(u8::from(!status.success()))
}
