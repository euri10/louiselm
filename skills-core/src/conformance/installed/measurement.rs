//! Fixed Debian 13 `x86_64` glibc measurement profile, with no library discovery policy.

use super::{CertificationError, HostSnapshot, PROFILE, storage::trusted_directory};
use crate::{
    Digest,
    launcher_install::{CommandInvocation, LauncherConfig, LauncherPaths, run_signing_command},
};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    time::Instant,
};

const LOADER: &str = "/usr/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2";
// These are the only accepted DT_NEEDED results for the fixed helper programs.
// No RPATH/plugin directory, alternate loader, preload or unlisted library is accepted.
const LIBRARIES: &[&str] = &[
    "libc.so.6",
    "libgcc_s.so.1",
    "libselinux.so.1",
    "libcap.so.2",
    "libpcre2-8.so.0",
    "libunwind-ptrace.so.0",
    "libunwind-x86_64.so.8",
    "libunwind.so.8",
    "liblzma.so.5",
    "libdw.so.1",
    "libelf.so.1",
    "libz.so.1",
    "libzstd.so.1",
    "libbpf.so.1",
    "libmnl.so.0",
    "libtinfo.so.6",
    "libm.so.6",
    "libcrypto.so.3",
    "libseccomp.so.2",
];

/// Measure the supported installed containment stack, not arbitrary Agent libraries.
/// Performs blocking bounded filesystem/helper I/O until `deadline`.
/// Configuration must come from the installed launcher authority.
///
/// # Errors
/// Refuses unsupported OS/architecture, unknown loader/library layouts,
/// untrusted files, unavailable required measurements and expired deadlines.
pub fn measure(
    paths: &LauncherPaths,
    config: &LauncherConfig,
    deadline: Instant,
) -> Result<HostSnapshot, CertificationError> {
    if std::env::consts::ARCH != "x86_64" {
        return Err(CertificationError::Unsupported);
    }
    let os = fs::read_to_string("/usr/lib/os-release")?;
    if !os.lines().any(|line| line == "ID=debian")
        || !os.lines().any(|line| line == "VERSION_ID=\"13\"")
    {
        return Err(CertificationError::Unsupported);
    }
    let mut inputs = BTreeMap::new();
    // A Session measures its immutable release, not the mutable current symlink.
    let launcher = paths
        .release_prefix
        .join("releases")
        .join(&config.release_id)
        .join("bin/louiselm-launch");
    for (name, path) in [
        ("launcher", launcher.as_path()),
        ("backend", config.bwrap_path.as_path()),
        ("loader", Path::new(LOADER)),
        ("os-release", Path::new("/usr/lib/os-release")),
        ("getent", paths.getent.as_path()),
        ("ssh-keygen", paths.ssh_keygen.as_path()),
        ("strace", Path::new("/usr/bin/strace")),
        ("ip", Path::new("/usr/sbin/ip")),
        ("bash", Path::new("/usr/bin/bash")),
        ("sleep", Path::new("/usr/bin/sleep")),
        ("unshare", Path::new("/usr/bin/unshare")),
    ] {
        check_deadline(deadline)?;
        inputs.insert(name.into(), file_digest(path)?);
    }
    if inputs.get("launcher") != Some(&config.launcher_digest)
        || inputs.get("backend") != Some(&config.bwrap_digest)
    {
        return Err(CertificationError::Invalid);
    }
    inputs.insert(
        "policy".into(),
        Digest::of(&serde_json::to_vec(config).map_err(|_| CertificationError::Invalid)?)
            .to_string(),
    );
    inputs.insert(
        "isolation-contract".into(),
        Digest::of(crate::isolation::CONTRACT_VERSION.as_bytes()).to_string(),
    );
    kernel_inputs(&mut inputs, deadline)?;
    loader_configuration(&mut inputs, deadline)?;
    for executable in [
        launcher.as_path(),
        config.bwrap_path.as_path(),
        paths.getent.as_path(),
        paths.ssh_keygen.as_path(),
        Path::new("/usr/bin/strace"),
        Path::new("/usr/sbin/ip"),
        Path::new("/usr/bin/bash"),
        Path::new("/usr/bin/sleep"),
        Path::new("/usr/bin/unshare"),
    ] {
        dependencies(executable, deadline, &mut inputs)?;
    }
    let machine = read_bounded(Path::new("/etc/machine-id"), 128)?;
    if machine.len() < 32 {
        return Err(CertificationError::Unsupported);
    }
    let boot = read_bounded(Path::new("/proc/sys/kernel/random/boot_id"), 128)?;
    let result = HostSnapshot {
        profile: PROFILE.into(),
        machine_digest: Digest::of(&machine).to_string(),
        boot_id: std::str::from_utf8(&boot)
            .map_err(|_| CertificationError::Unsupported)?
            .trim()
            .into(),
        release_digest: config.release_id.clone(),
        inputs,
    };
    result.validate()?;
    check_deadline(deadline)?;
    Ok(result)
}

fn kernel_inputs(
    inputs: &mut BTreeMap<String, String>,
    deadline: Instant,
) -> Result<(), CertificationError> {
    inputs.insert(
        "kernel".into(),
        Digest::of(&read_bounded(Path::new("/sys/kernel/notes"), 1024 * 1024)?).to_string(),
    );
    for path in [
        "/proc/sys/kernel/osrelease",
        "/proc/sys/kernel/version",
        "/proc/sys/kernel/tainted",
        "/proc/cmdline",
        "/proc/sys/kernel/yama/ptrace_scope",
        "/proc/sys/kernel/unprivileged_userns_clone",
        "/proc/sys/kernel/modules_disabled",
        "/proc/sys/kernel/apparmor_restrict_unprivileged_userns",
        "/proc/sys/kernel/apparmor_restrict_unprivileged_unconfined",
        "/proc/sys/user/max_user_namespaces",
        "/sys/fs/cgroup/cgroup.controllers",
        "/proc/sys/fs/suid_dumpable",
        "/sys/kernel/security/lsm",
    ] {
        check_deadline(deadline)?;
        inputs.insert(path.into(), optional_digest(Path::new(path))?);
    }
    // Module names, not volatile reference counts or addresses. These are
    // running-host measurements, not running-kernel/module byte attestation.
    let modules = read_bounded(Path::new("/proc/modules"), 1024 * 1024)?;
    let text = std::str::from_utf8(&modules).map_err(|_| CertificationError::Unsupported)?;
    let mut names: Vec<_> = text
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .collect();
    names.sort_unstable();
    inputs.insert(
        "kernel-modules".into(),
        Digest::of(names.join("\n").as_bytes()).to_string(),
    );
    Ok(())
}

fn loader_configuration(
    inputs: &mut BTreeMap<String, String>,
    deadline: Instant,
) -> Result<(), CertificationError> {
    if Path::new("/etc/ld.so.preload").exists()
        && !read_bounded(Path::new("/etc/ld.so.preload"), 4096)?.is_empty()
    {
        return Err(CertificationError::Unsupported);
    }
    for path in [
        "/etc/ld.so.preload",
        "/etc/ld.so.conf",
        "/etc/ld.so.cache",
        "/etc/nsswitch.conf",
    ] {
        match fs::symlink_metadata(path) {
            Ok(_) => {
                file_digest(Path::new(path))?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
        inputs.insert(path.into(), optional_digest(Path::new(path))?);
    }
    trusted_directory(Path::new("/etc/ld.so.conf.d"), 0)?;
    for (index, entry) in fs::read_dir("/etc/ld.so.conf.d")?.enumerate() {
        if index >= 32 {
            return Err(CertificationError::Unsupported);
        }
        let entry = entry?;
        let path = entry.path();
        inputs.insert(
            path.to_str().ok_or(CertificationError::Unsupported)?.into(),
            file_digest(&path)?,
        );
    }
    // NSS helpers are used for identity validation. Unknown dynamic backends
    // cannot be treated as part of a closed DT_NEEDED dependency set.
    let nss = fs::read_to_string("/etc/nsswitch.conf")?;
    for line in nss.lines().filter(|line| {
        ["passwd:", "group:", "subid:"]
            .iter()
            .any(|key| line.starts_with(key))
    }) {
        let backends = line
            .split('#')
            .next()
            .ok_or(CertificationError::Unsupported)?;
        if backends
            .split_whitespace()
            .skip(1)
            .any(|value| !["files", "systemd"].contains(&value))
        {
            return Err(CertificationError::Unsupported);
        }
        if backends.split_whitespace().any(|value| value == "systemd") {
            let plugin = Path::new("/usr/lib/x86_64-linux-gnu/libnss_systemd.so.2");
            inputs.insert("nss-systemd".into(), file_digest(plugin)?);
            dependencies(plugin, deadline, inputs)?;
        }
    }
    Ok(())
}

fn dependencies(
    executable: &Path,
    deadline: Instant,
    inputs: &mut BTreeMap<String, String>,
) -> Result<(), CertificationError> {
    let invocation = CommandInvocation {
        program: LOADER.into(),
        arguments: vec!["--list".into(), executable.as_os_str().into()],
        stdin: Vec::new(),
        current_dir: Some("/".into()),
    };
    let output = run_signing_command(&invocation, deadline, None)?;
    if !output.success || output.stdout.len() > 32 * 1024 || !output.stderr.is_empty() {
        return Err(CertificationError::Unsupported);
    }
    let text = std::str::from_utf8(&output.stdout).map_err(|_| CertificationError::Unsupported)?;
    let mut count = 0;
    for line in text.lines() {
        let words: Vec<_> = line.split_whitespace().collect();
        if words.first() == Some(&"linux-vdso.so.1") {
            continue;
        }
        if words
            .first()
            .is_some_and(|value| value.ends_with("/ld-linux-x86-64.so.2"))
        {
            if fs::canonicalize(words[0])? != Path::new(LOADER) {
                return Err(CertificationError::Unsupported);
            }
            continue;
        }
        if words.len() != 4 || words[1] != "=>" || !LIBRARIES.contains(&words[0]) {
            return Err(CertificationError::Unsupported);
        }
        let expected = PathBuf::from("/usr/lib/x86_64-linux-gnu").join(words[0]);
        let actual = fs::canonicalize(words[2])?;
        if actual != fs::canonicalize(&expected)?
            || actual.parent() != Some(Path::new("/usr/lib/x86_64-linux-gnu"))
        {
            return Err(CertificationError::Unsupported);
        }
        inputs.insert(format!("library/{}", words[0]), file_digest(&expected)?);
        count += 1;
        if count > LIBRARIES.len() {
            return Err(CertificationError::Unsupported);
        }
    }
    if count == 0 {
        return Err(CertificationError::Unsupported);
    }
    Ok(())
}

fn optional_digest(path: &Path) -> Result<String, CertificationError> {
    match read_bounded(path, 4 * 1024 * 1024) {
        Ok(bytes) => {
            let mut present = b"present\0".to_vec();
            present.extend_from_slice(&bytes);
            Ok(Digest::of(&present).to_string())
        }
        Err(CertificationError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(Digest::of(b"absent").to_string())
        }
        Err(error) => Err(error),
    }
}

fn file_digest(path: &Path) -> Result<String, CertificationError> {
    let path = fs::canonicalize(path)?;
    trusted_directory(path.parent().ok_or(CertificationError::Invalid)?, 0)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NONBLOCK.bits().cast_signed())
        .open(&path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        return Err(CertificationError::Invalid);
    }
    Ok(Digest::of(&read_file(file, 128 * 1024 * 1024)?).to_string())
}

fn read_bounded(path: &Path, maximum: usize) -> Result<Vec<u8>, CertificationError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NONBLOCK.bits().cast_signed())
        .open(path)?;
    read_file(file, maximum)
}

fn read_file(file: File, maximum: usize) -> Result<Vec<u8>, CertificationError> {
    let mut bytes = Vec::new();
    if !file.metadata()?.is_file() {
        return Err(CertificationError::Unsupported);
    }
    file.take(maximum as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err(CertificationError::Unsupported);
    }
    Ok(bytes)
}

fn check_deadline(deadline: Instant) -> Result<(), CertificationError> {
    if Instant::now() >= deadline {
        return Err(std::io::Error::from(std::io::ErrorKind::TimedOut).into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    #[allow(clippy::unwrap_used, reason = "Owned measurement regression fixture.")]
    fn missing_optional_input_is_not_literal_absent_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("input");
        let missing = super::optional_digest(&path).unwrap();
        std::fs::write(&path, b"absent").unwrap();
        let present = super::optional_digest(&path).unwrap();
        assert_ne!(missing, present, "presence must be part of the measurement");
        std::fs::write(&path, b"changed").unwrap();
        assert_ne!(present, super::optional_digest(&path).unwrap());
        assert!(super::optional_digest(directory.path()).is_err());
        assert!(super::optional_digest(&path.join("not-a-directory")).is_err());
    }
}
