//! Private, explicitly configured backup of the last changed admitted profile.

use std::{
    fs::File,
    io::Read,
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    path::{Component, Path, PathBuf},
    process::Command,
};

use rustix::fs::{AtFlags, FileType, Mode, OFlags};
use serde::{Deserialize, Serialize};

use super::{AdmissionError, GenerationRecord, Store, TrustStore, transaction};
use crate::{Digest, Policy, inspect::PackageKind, instruction_view::profile};

const CONFIG: &str = "operator-profile-mirror.json";
const DIRECTORY: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const READ: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::NONBLOCK)
    .union(OFlags::CLOEXEC);

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Config {
    version: u8,
    path: PathBuf,
    generation: Option<String>,
    profile: Option<String>,
    pending: bool,
}

#[derive(Serialize)]
pub(crate) struct Status {
    pub(crate) state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    generation: Option<String>,
}

fn refused(reason: &str) -> AdmissionError {
    AdmissionError::Mirror {
        reason: reason.to_owned(),
    }
}

fn config(store: &Store) -> Result<Option<Config>, AdmissionError> {
    let path = store.root().join(CONFIG);
    let file = match rustix::fs::open(&path, READ, Mode::empty()) {
        Ok(file) => File::from(file),
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(_) => return Err(refused("mirror_config_unreadable")),
    };
    let metadata = file
        .metadata()
        .map_err(|_| refused("mirror_config_unreadable"))?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || ![0, rustix::process::geteuid().as_raw()].contains(&metadata.uid())
        || metadata.mode() & 0o077 != 0
        || metadata.len() > 16 * 1024
    {
        return Err(refused("mirror_config_unsafe"));
    }
    let mut bytes = Vec::new();
    file.take(16 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| refused("mirror_config_unreadable"))?;
    let config: Config =
        serde_json::from_slice(&bytes).map_err(|_| refused("mirror_config_invalid"))?;
    if config.version != 1
        || (config.profile.is_some() || config.pending) && config.generation.is_none()
        || config
            .generation
            .as_ref()
            .is_some_and(|value| Digest::parse(value).is_err())
        || config
            .profile
            .as_ref()
            .is_some_and(|value| Digest::parse(value).is_err())
    {
        return Err(refused("mirror_config_invalid"));
    }
    validate_path(store, &config.path)?;
    Ok(Some(config))
}

fn save(store: &Store, config: &Config) -> Result<(), AdmissionError> {
    let path = store.root().join(CONFIG);
    let directory = File::open(store.root()).map_err(|_| refused("mirror_config_unwritable"))?;
    let bytes = serde_json::to_vec(config).map_err(|_| refused("mirror_config_invalid"))?;
    profile::replace_in(&path, &directory, &bytes).map_err(|_| refused("mirror_config_unwritable"))
}

fn validate_path(store: &Store, path: &Path) -> Result<(), AdmissionError> {
    let text = path
        .to_str()
        .ok_or_else(|| refused("mirror_path_invalid"))?;
    if !path.is_absolute()
        || text.chars().any(char::is_control)
        || text.split('/').skip(1).any(|part| {
            part.is_empty() || part == "." || part == ".." || part.eq_ignore_ascii_case(".git")
        })
    {
        return Err(refused("mirror_path_invalid"));
    }
    let root =
        std::fs::canonicalize(store.root()).map_err(|_| refused("mirror_store_unavailable"))?;
    if path.starts_with(root) {
        return Err(refused("mirror_inside_store"));
    }
    Ok(())
}

fn check_directory(directory: &File) -> Result<(), AdmissionError> {
    let metadata = directory
        .metadata()
        .map_err(|_| refused("mirror_unsafe_directory"))?;
    let uid = rustix::process::geteuid().as_raw();
    let sticky_root = metadata.uid() == 0 && metadata.mode() & 0o1000 != 0;
    if !metadata.is_dir()
        || ![0, uid].contains(&metadata.uid())
        || (metadata.mode() & 0o022 != 0 && !sticky_root)
    {
        return Err(refused("mirror_unsafe_directory"));
    }
    Ok(())
}

fn check_checkout(directory: &File) -> Result<(), AdmissionError> {
    let git = match rustix::fs::openat(directory, ".git", DIRECTORY, Mode::empty()) {
        Ok(git) => File::from(git),
        Err(rustix::io::Errno::NOENT) => return Ok(()),
        Err(_) => return Err(refused("mirror_unsafe_checkout")),
    };
    check_directory(&git)?;
    if git
        .metadata()
        .map_err(|_| refused("mirror_unsafe_checkout"))?
        .mode()
        & 0o022
        != 0
    {
        return Err(refused("mirror_unsafe_checkout"));
    }
    let file = File::from(
        rustix::fs::openat(&git, "config", READ, Mode::empty())
            .map_err(|_| refused("mirror_unsafe_checkout"))?,
    );
    check_git_config(&file)?;
    check_ambient_git_configs()
}

fn check_git_config(file: &File) -> Result<(), AdmissionError> {
    let metadata = file
        .metadata()
        .map_err(|_| refused("mirror_unsafe_checkout"))?;
    let uid = rustix::process::geteuid().as_raw();
    if !metadata.is_file()
        || metadata.nlink() != 1
        || ![0, uid].contains(&metadata.uid())
        || metadata.mode() & 0o022 != 0
        || metadata.len() > 64 * 1024
    {
        return Err(refused("mirror_unsafe_checkout"));
    }
    // Parse key names only; refuse includes and rewrites instead of following
    // them or trying to infer a remote's visibility from its URL.
    let config_path = format!("/proc/{}/fd/{}", std::process::id(), file.as_raw_fd());
    let output = Command::new("git")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args([
            "config",
            "--file",
            &config_path,
            "--no-includes",
            "--name-only",
            "--null",
            "--get-regexp",
            r"^(remote\.|include\.|includeif\.|url\.|branch\..*\.(remote|pushremote)$|extensions\.worktreeconfig$|core\.worktree$)",
        ])
        .output()
        .map_err(|_| refused("mirror_checkout_uninspectable"))?;
    match output.status.code() {
        Some(1) => Ok(()),
        Some(0) => Err(refused("mirror_checkout_has_remote")),
        _ => Err(refused("mirror_checkout_uninspectable")),
    }
}

fn optional_git_config(path: &Path) -> Result<(), AdmissionError> {
    if path == Path::new("/dev/null") {
        return Ok(());
    }
    let file = match rustix::fs::open(path, READ, Mode::empty()) {
        Ok(file) => File::from(file),
        Err(rustix::io::Errno::NOENT) => return Ok(()),
        Err(_) => return Err(refused("mirror_checkout_uninspectable")),
    };
    check_git_config(&file)
}

fn check_ambient_git_configs() -> Result<(), AdmissionError> {
    if ["GIT_CONFIG", "GIT_CONFIG_PARAMETERS"]
        .iter()
        .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
        || std::env::var_os("GIT_CONFIG_COUNT").is_some_and(|value| value != "0")
    {
        return Err(refused("mirror_checkout_uninspectable"));
    }
    let system = std::env::var_os("GIT_CONFIG_SYSTEM")
        .map_or_else(|| PathBuf::from("/etc/gitconfig"), PathBuf::from);
    optional_git_config(&system)?;
    if let Some(global) = std::env::var_os("GIT_CONFIG_GLOBAL") {
        return optional_git_config(Path::new(&global));
    }
    let home = std::env::var_os("HOME").ok_or_else(|| refused("mirror_checkout_uninspectable"))?;
    let home = PathBuf::from(home);
    if !home.is_absolute() {
        return Err(refused("mirror_checkout_uninspectable"));
    }
    let xdg =
        std::env::var_os("XDG_CONFIG_HOME").map_or_else(|| home.join(".config"), PathBuf::from);
    if !xdg.is_absolute() {
        return Err(refused("mirror_checkout_uninspectable"));
    }
    optional_git_config(&xdg.join("git/config"))?;
    optional_git_config(&home.join(".gitconfig"))
}

fn destination(store: &Store, path: &Path) -> Result<File, AdmissionError> {
    validate_path(store, path)?;
    let mut directory = File::from(
        rustix::fs::open("/", DIRECTORY, Mode::empty())
            .map_err(|_| refused("mirror_unsafe_directory"))?,
    );
    check_directory(&directory)?;
    check_checkout(&directory)?;
    let parent = path
        .parent()
        .ok_or_else(|| refused("mirror_path_invalid"))?;
    for component in parent.components() {
        let Component::Normal(name) = component else {
            continue;
        };
        directory = File::from(
            rustix::fs::openat(&directory, name, DIRECTORY, Mode::empty())
                .map_err(|_| refused("mirror_unsafe_directory"))?,
        );
        check_directory(&directory)?;
        check_checkout(&directory)?;
    }
    let name = path
        .file_name()
        .ok_or_else(|| refused("mirror_path_invalid"))?;
    match rustix::fs::statat(&directory, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat)
            if FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile
                && stat.st_nlink == 1
                && stat.st_uid == rustix::process::geteuid().as_raw()
                && stat.st_mode.trailing_zeros() >= 6 => {}
        Err(rustix::io::Errno::NOENT) => (),
        _ => return Err(refused("mirror_unsafe_file")),
    }
    Ok(directory)
}

fn profile_for_record(
    store: &Store,
    policy: &Policy,
    record: &GenerationRecord,
) -> Result<Option<(String, Vec<u8>)>, AdmissionError> {
    if record.payload.policy_digest != policy.digest().to_string() {
        return Err(refused("mirror_policy_mismatch"));
    }
    let packages = crate::instruction_view::checked_package_contents(store, policy, record)
        .map_err(|_| refused("mirror_package_invalid"))?;
    let mut found = None;
    for member in &record.payload.members {
        let package = &packages[member.package_digest.as_str()];
        if PackageKind::of(&package.manifest) == PackageKind::OperatorProfile {
            if found.is_some() {
                return Err(refused("mirror_multiple_profiles"));
            }
            let bytes = profile::bytes(package).map_err(|_| refused("mirror_profile_invalid"))?;
            found = Some((member.package_digest.clone(), bytes));
        }
    }
    Ok(found)
}

fn approved_record(store: &Store, generation: &str) -> Result<GenerationRecord, AdmissionError> {
    let trust = TrustStore::load(store)?.ok_or_else(|| refused("mirror_trust_unavailable"))?;
    let record = super::load_unlocked(store, &Digest::parse(generation)?)?;
    super::verify_record(store, &record, &trust)?;
    if !trust.approved_admissions.contains(generation) {
        return Err(refused("mirror_admission_unsettled"));
    }
    Ok(record)
}

pub(super) fn prepare(
    store: &Store,
    record: &GenerationRecord,
    profile: Option<&str>,
) -> Result<(), AdmissionError> {
    let Some(mut config) = config(store)? else {
        return Ok(());
    };
    if config.profile.as_deref() == profile && !config.pending {
        return Ok(());
    }
    if profile.is_some() {
        destination(store, &config.path)?;
    }
    config.generation = Some(record.generation.clone());
    config.profile = profile.map(str::to_owned);
    config.pending = true;
    save(store, &config)
}

pub(super) fn preflight(store: &Store, profile: Option<&str>) -> Result<(), AdmissionError> {
    if let Some(config) = config(store)?
        && profile.is_some()
        && (config.profile.as_deref() != profile || config.pending)
    {
        destination(store, &config.path)?;
    }
    Ok(())
}

pub(super) fn finish(
    store: &Store,
    policy: &Policy,
    record: &GenerationRecord,
) -> Result<(), AdmissionError> {
    let Some(mut config) = config(store)? else {
        return Ok(());
    };
    if !config.pending || config.generation.as_deref() != Some(&record.generation) {
        return Ok(());
    }
    let admitted = approved_record(store, &record.generation)?;
    let selected = profile_for_record(store, policy, &admitted)?;
    if selected.as_ref().map(|(digest, _)| digest.as_str()) != config.profile.as_deref() {
        return Err(refused("mirror_profile_mismatch"));
    }
    if let Some((_, bytes)) = selected {
        let directory = destination(store, &config.path)?;
        profile::replace_in(&config.path, &directory, &bytes)
            .map_err(|_| refused("mirror_write_failed"))?;
    }
    config.pending = false;
    save(store, &config)
}

pub(crate) fn configure(store: &Store, policy: &Policy, path: &Path) -> Result<(), AdmissionError> {
    let _locked = transaction::lock(store)?;
    destination(store, path)?;
    let existing = config(store)?;
    let record = match existing
        .as_ref()
        .and_then(|config| config.generation.as_deref())
    {
        Some(generation) => Some(approved_record(store, generation)?),
        None => super::current_unlocked(store)?,
    };
    let selected = record
        .as_ref()
        .map(|record| {
            let admitted = approved_record(store, &record.generation)?;
            profile_for_record(store, policy, &admitted)
        })
        .transpose()?
        .flatten();
    let config = Config {
        version: 1,
        path: path.to_owned(),
        generation: record.as_ref().map(|record| record.generation.clone()),
        profile: selected.as_ref().map(|(digest, _)| digest.clone()),
        pending: selected.is_some(),
    };
    save(store, &config)?;
    if let Some(record) = record {
        finish(store, policy, &record)?;
    }
    Ok(())
}

pub(crate) fn status(store: &Store, policy: &Policy) -> Result<Status, AdmissionError> {
    let _locked = transaction::lock(store)?;
    let Some(config) = config(store)? else {
        return Ok(Status {
            state: "unconfigured",
            generation: None,
        });
    };
    let generation = config.generation.clone();
    let directory = match destination(store, &config.path) {
        Ok(directory) => directory,
        Err(AdmissionError::Mirror { .. }) => {
            return Ok(Status {
                state: "unsafe",
                generation,
            });
        }
        Err(error) => return Err(error),
    };
    let Some(generation_id) = config.generation.as_deref() else {
        return Ok(Status {
            state: "no_profile",
            generation,
        });
    };
    let record = match approved_record(store, generation_id) {
        Ok(record) => record,
        Err(_) if config.pending => {
            return Ok(Status {
                state: "unsettled",
                generation,
            });
        }
        Err(error) => return Err(error),
    };
    let selected = profile_for_record(store, policy, &record)?;
    if selected.as_ref().map(|(digest, _)| digest.as_str()) != config.profile.as_deref() {
        return Err(refused("mirror_profile_mismatch"));
    }
    let Some((_, bytes)) = selected else {
        return Ok(Status {
            state: if config.pending {
                "unsettled"
            } else {
                "no_profile"
            },
            generation,
        });
    };
    let same = profile::matches_in(&config.path, &directory, &bytes)
        .map_err(|_| refused("mirror_read_failed"))?;
    Ok(Status {
        state: if same && !config.pending {
            "current"
        } else {
            "drift"
        },
        generation,
    })
}
