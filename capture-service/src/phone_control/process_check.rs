//! Verify the exact process descriptor retained by the editor.

use std::io;

#[cfg(target_os = "linux")]
pub(crate) fn verify_descriptor(
    file: &std::io::Stdin,
    status: &std::io::Stderr,
    pid: u32,
    editor_pid: u32,
    namespace: &str,
) -> io::Result<()> {
    use super::sleep_clock::{open_file, proc_mount_id, proc_root_mount};
    use rustix::fs::{Mode, OFlags, fstat, open, readlinkat};

    fn pid_namespace(path: &str, mount: u64) -> io::Result<String> {
        let link = open(
            path,
            OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        if proc_mount_id(&link)? != mount {
            return Err(io::Error::other("process namespace is unavailable"));
        }
        let value = readlinkat(&link, "", Vec::new())?;
        let value = value
            .to_str()
            .map_err(|_| io::Error::other("process namespace is unavailable"))?;
        if !value
            .strip_prefix("pid:[")
            .and_then(|v| v.strip_suffix(']'))
            .is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err(io::Error::other("process namespace is unavailable"));
        }
        Ok(value.to_owned())
    }

    let unavailable = || io::Error::other("process descriptor is unavailable");
    let mount = proc_root_mount()?;
    let expected_status = open_file(&format!("/proc/{editor_pid}/status"))?;
    let expected = open_file(&format!("/proc/{pid}/stat"))?;
    let actual_stat = fstat(file)?;
    let expected_stat = fstat(&expected)?;
    let actual_status = fstat(status)?;
    let expected_status_stat = fstat(&expected_status)?;
    if proc_mount_id(file)? != mount
        || proc_mount_id(&expected)? != mount
        || proc_mount_id(status)? != mount
        || proc_mount_id(&expected_status)? != mount
        || actual_stat.st_dev != expected_stat.st_dev
        || actual_stat.st_ino != expected_stat.st_ino
        || actual_stat.st_mode != expected_stat.st_mode
        || actual_status.st_dev != expected_status_stat.st_dev
        || actual_status.st_ino != expected_status_stat.st_ino
        || actual_status.st_mode != expected_status_stat.st_mode
    {
        return Err(unavailable());
    }
    let mut bytes = vec![0; 65537];
    let length = rustix::io::pread(status, &mut bytes, 0)?;
    if length > 65536 {
        return Err(unavailable());
    }
    let text = std::str::from_utf8(&bytes[..length]).map_err(|_| unavailable())?;
    let mut entries = text.lines().filter_map(|line| line.strip_prefix("NSpid:"));
    if entries.next().map(str::trim) != Some(editor_pid.to_string().as_str())
        || entries.next().is_some()
    {
        return Err(unavailable());
    }
    let editor_namespace = pid_namespace(&format!("/proc/{editor_pid}/ns/pid"), mount)?;
    let peer_namespace = pid_namespace(&format!("/proc/{pid}/ns/pid"), mount)?;
    if editor_namespace != namespace || peer_namespace != namespace {
        return Err(unavailable());
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn verify_descriptor(
    _file: &std::io::Stdin,
    _status: &std::io::Stderr,
    _pid: u32,
    _editor_pid: u32,
    _namespace: &str,
) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "phone authority requires Linux",
    ))
}
