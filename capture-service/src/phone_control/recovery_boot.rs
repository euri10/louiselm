//! Boot identity evidence for explicitly armed phone recovery.

use std::io;

fn parse_boot_id(value: &str) -> io::Result<String> {
    let bytes = value.as_bytes();
    if bytes.len() != 37 || bytes[36] != b'\n' {
        return Err(io::Error::other("boot identity is malformed"));
    }
    for (index, byte) in bytes[..36].iter().copied().enumerate() {
        let valid = match index {
            8 | 13 | 18 | 23 => byte == b'-',
            14 => byte == b'4',
            19 => matches!(byte, b'8' | b'9' | b'a' | b'b'),
            _ => byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte),
        };
        if !valid {
            return Err(io::Error::other("boot identity is malformed"));
        }
    }
    Ok(value[..36].to_owned())
}

fn is_proc_mount(mounts: &str, mount_id: &str) -> bool {
    let mut matches = mounts.lines().filter(|line| {
        let Some((fields, filesystem)) = line.split_once(" - ") else {
            return false;
        };
        let mut fields = fields.split_whitespace();
        fields.next() == Some(mount_id)
            && fields.nth(2) == Some("/")
            && fields.next() == Some("/proc")
            && filesystem.split_whitespace().next() == Some("proc")
    });
    matches.next().is_some() && matches.next().is_none()
}

/// Read the current kernel's boot identifier from its full proc mount.
///
/// This proves neither registrar scope nor that a recovery marker is intact;
/// callers must establish those independently before committing recovery.
#[cfg(target_os = "linux")]
pub(super) fn boot_id() -> io::Result<String> {
    use std::{fs::File, io::Read, os::unix::fs::MetadataExt};

    use rustix::fs::{AtFlags, PROC_SUPER_MAGIC, StatxFlags, fstatfs, statx};

    fn bounded(file: impl Read, limit: usize) -> io::Result<String> {
        let mut value = String::new();
        file.take(u64::try_from(limit).map_err(io::Error::other)? + 1)
            .read_to_string(&mut value)?;
        if value.len() > limit {
            return Err(io::Error::other("boot identity source is too large"));
        }
        Ok(value)
    }

    fn mount_id(file: &File) -> io::Result<String> {
        let info =
            statx(file, "", AtFlags::EMPTY_PATH, StatxFlags::MNT_ID).map_err(io::Error::from)?;
        if info.stx_mask & StatxFlags::MNT_ID.bits() == 0
            || info.stx_mnt_id == 0
            || fstatfs(file).map_err(io::Error::from)?.f_type != PROC_SUPER_MAGIC
        {
            return Err(io::Error::other("boot identity mount is unavailable"));
        }
        Ok(info.stx_mnt_id.to_string())
    }

    let file = File::open("/proc/sys/kernel/random/boot_id")?;
    let mounts = File::open("/proc/thread-self/mountinfo")?;
    let id = mount_id(&file)?;
    if mount_id(&mounts)? != id || !is_proc_mount(&bounded(&mounts, 1_048_576)?, &id) {
        return Err(io::Error::other(
            "boot identity is not on the full proc mount",
        ));
    }
    let identity = parse_boot_id(&bounded(&file, 128)?)?;
    let current = File::open("/proc/sys/kernel/random/boot_id")?;
    let before = file.metadata()?;
    let after = current.metadata()?;
    if before.dev() != after.dev()
        || before.ino() != after.ino()
        || mount_id(&current)? != id
        || parse_boot_id(&bounded(&current, 128)?)? != identity
    {
        return Err(io::Error::other("boot identity source changed"));
    }
    Ok(identity)
}

#[cfg(not(target_os = "linux"))]
pub(super) fn boot_id() -> io::Result<String> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "phone recovery requires Linux",
    ))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "boot evidence test assertions")]
mod tests {
    use super::{boot_id, is_proc_mount, parse_boot_id};

    #[test]
    fn boot_id_requires_a_canonical_kernel_uuid() {
        let valid = "4db1e35f-8ade-4f98-a6ee-0cea1221ac9e";
        assert_eq!(parse_boot_id(&format!("{valid}\n")).unwrap(), valid);
        for invalid in [
            "",
            "4db1e35f-8ade-4f98-a6ee-0cea1221ac9e",
            "4db1e35f-8ade-4f98-a6ee-0cea1221ac9e\nextra\n",
            "4DB1E35F-8ADE-4F98-A6EE-0CEA1221AC9E\n",
            "4db1e35f-8ade-1f98-a6ee-0cea1221ac9e\n",
            "4db1e35f-8ade-4f98-06ee-0cea1221ac9e\n",
            "00000000-0000-0000-0000-000000000000\n",
        ] {
            assert!(parse_boot_id(invalid).is_err(), "accepted {invalid:?}");
        }
    }

    #[test]
    fn only_the_opened_full_proc_mount_is_accepted() {
        let proc = "29 36 0:26 / /proc rw - proc proc rw\n";
        assert!(is_proc_mount(proc, "29"));
        assert!(!is_proc_mount(proc, "30"));
        assert!(!is_proc_mount(
            "29 36 0:26 /sys /proc/sys rw - proc proc rw\n",
            "29"
        ));
        assert!(!is_proc_mount(
            "29 36 0:26 / /proc rw - tmpfs tmpfs rw\n",
            "29"
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn current_kernel_identity_is_stable_within_one_boot() {
        assert_eq!(boot_id().unwrap(), boot_id().unwrap());
    }
}
