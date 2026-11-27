//! Linux boot-time samples for finite phone working periods.

use std::io;
#[cfg(target_os = "linux")]
use std::io::Read;

use super::{ABSOLUTE_LIFETIME, IDLE_LIFETIME, Refusal};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Sample {
    pub(super) centiseconds: u64,
    pub(super) namespace: String,
}

pub(super) struct Lease {
    namespace: String,
    last: u64,
    idle_deadline: u64,
    absolute_deadline: u64,
}

impl Lease {
    pub(super) fn new(now: &Sample) -> Result<Self, Refusal> {
        let idle = IDLE_LIFETIME
            .as_secs()
            .checked_mul(100)
            .ok_or(Refusal::Unavailable)?;
        let absolute = ABSOLUTE_LIFETIME
            .as_secs()
            .checked_mul(100)
            .ok_or(Refusal::Unavailable)?;
        Ok(Self {
            namespace: now.namespace.clone(),
            last: now.centiseconds,
            idle_deadline: now
                .centiseconds
                .checked_add(idle)
                .ok_or(Refusal::Unavailable)?,
            absolute_deadline: now
                .centiseconds
                .checked_add(absolute)
                .ok_or(Refusal::Unavailable)?,
        })
    }

    pub(super) fn check(&mut self, now: &Sample) -> Result<(), Refusal> {
        if now.namespace != self.namespace || now.centiseconds < self.last {
            return Err(Refusal::Unavailable);
        }
        self.last = now.centiseconds;
        // /proc/uptime truncates to centiseconds. Charge the missing fraction.
        let upper = now
            .centiseconds
            .checked_add(1)
            .ok_or(Refusal::Unavailable)?;
        if upper >= self.idle_deadline || upper >= self.absolute_deadline {
            return Err(Refusal::Expired);
        }
        Ok(())
    }

    pub(super) fn remaining_ms(&mut self, now: &Sample) -> Result<u64, Refusal> {
        self.check(now)?;
        self.idle_deadline
            .min(self.absolute_deadline)
            .checked_sub(
                now.centiseconds
                    .checked_add(1)
                    .ok_or(Refusal::Unavailable)?,
            )
            .and_then(|remaining| remaining.checked_mul(10))
            .ok_or(Refusal::Unavailable)
    }

    pub(super) fn renew_idle(&mut self, started: &Sample, now: &Sample) -> Result<(), Refusal> {
        if started.namespace != self.namespace || started.centiseconds > now.centiseconds {
            return Err(Refusal::Unavailable);
        }
        let idle = IDLE_LIFETIME
            .as_secs()
            .checked_mul(100)
            .ok_or(Refusal::Unavailable)?;
        let requested = started
            .centiseconds
            .checked_add(idle)
            .ok_or(Refusal::Unavailable)?;
        self.idle_deadline = self
            .idle_deadline
            .max(requested)
            .min(self.absolute_deadline);
        self.check(now)
    }
}

#[cfg(target_os = "linux")]
fn is_proc_mount(mounts: &str, mount_id: &str) -> bool {
    let mut matches = mounts
        .lines()
        .filter(|line| line.split_whitespace().next() == Some(mount_id));
    let Some((fields, filesystem)) = matches.next().and_then(|line| line.split_once(" - ")) else {
        return false;
    };
    let mut fields = fields.split_whitespace();
    matches.next().is_none()
        && fields.next() == Some(mount_id)
        && fields.nth(2) == Some("/")
        && fields.next() == Some("/proc")
        && filesystem.split_whitespace().next() == Some("proc")
}

#[cfg(target_os = "linux")]
fn parse_uptime(value: &str) -> io::Result<u64> {
    fn component(value: &str) -> Option<(&str, &str)> {
        let (seconds, fraction) = value.split_once('.')?;
        (!seconds.is_empty()
            && seconds.bytes().all(|byte| byte.is_ascii_digit())
            && fraction.len() == 2
            && fraction.bytes().all(|byte| byte.is_ascii_digit()))
        .then_some((seconds, fraction))
    }
    let mut fields = value.split_whitespace();
    let seconds = fields
        .next()
        .and_then(component)
        .ok_or_else(|| io::Error::other("boot clock is malformed"))?;
    if fields.next().and_then(component).is_none() || fields.next().is_some() {
        return Err(io::Error::other("boot clock is malformed"));
    }
    seconds
        .0
        .parse::<u64>()
        .ok()
        .and_then(|value| value.checked_mul(100))
        .and_then(|value| value.checked_add(seconds.1.parse::<u64>().ok()?))
        .ok_or_else(|| io::Error::other("boot clock is malformed"))
}

#[cfg(target_os = "linux")]
fn proc_mount_id(file: impl std::os::fd::AsFd) -> io::Result<u64> {
    use rustix::fs::{AtFlags, PROC_SUPER_MAGIC, StatxFlags, fstatfs, statx};

    let info =
        statx(&file, "", AtFlags::EMPTY_PATH, StatxFlags::MNT_ID).map_err(io::Error::other)?;
    if info.stx_mask & StatxFlags::MNT_ID.bits() == 0
        || info.stx_mnt_id == 0
        || fstatfs(&file).map_err(io::Error::other)?.f_type != PROC_SUPER_MAGIC
    {
        return Err(io::Error::other("boot clock mount is unavailable"));
    }
    Ok(info.stx_mnt_id)
}

#[cfg(target_os = "linux")]
fn open_file(path: &str) -> io::Result<std::fs::File> {
    use rustix::fs::{Mode, OFlags, open};

    open(
        path,
        OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(std::fs::File::from)
    .map_err(io::Error::from)
}

#[cfg(target_os = "linux")]
fn bounded(file: impl Read, limit: u64) -> io::Result<String> {
    let mut value = String::new();
    file.take(limit + 1).read_to_string(&mut value)?;
    if value.len() as u64 > limit {
        return Err(io::Error::other("boot clock source is too large"));
    }
    Ok(value)
}

#[cfg(target_os = "linux")]
fn proc_root_mount() -> io::Result<u64> {
    let mounts = open_file("/proc/thread-self/mountinfo")?;
    let mount_id = proc_mount_id(&mounts)?;
    if !is_proc_mount(&bounded(&mounts, 1_048_576)?, &mount_id.to_string()) {
        return Err(io::Error::other("boot clock is not on the proc filesystem"));
    }
    Ok(mount_id)
}

#[cfg(target_os = "linux")]
pub(crate) fn verify_descriptor(file: &std::io::Stdin) -> io::Result<()> {
    use rustix::fs::fstat;

    let mount_id = proc_root_mount()?;
    let uptime = open_file("/proc/uptime")?;
    let actual = fstat(file).map_err(io::Error::from)?;
    let expected = fstat(&uptime).map_err(io::Error::from)?;
    if proc_mount_id(file)? != mount_id
        || proc_mount_id(&uptime)? != mount_id
        || actual.st_dev != expected.st_dev
        || actual.st_ino != expected.st_ino
        || actual.st_mode != expected.st_mode
    {
        return Err(io::Error::other("boot clock descriptor is unavailable"));
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn verify_descriptor(_file: &std::io::Stdin) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "sleep-aware phone authority requires Linux",
    ))
}

#[cfg(target_os = "linux")]
fn parse_namespace(namespace: &str) -> io::Result<String> {
    namespace
        .strip_prefix("time:[")
        .and_then(|value| value.strip_suffix(']'))
        .filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .ok_or_else(|| io::Error::other("time namespace is unavailable"))?;
    Ok(namespace.to_owned())
}

#[cfg(target_os = "linux")]
fn read_namespace(path: &std::path::Path, mount_id: u64) -> io::Result<String> {
    use rustix::fs::{Mode, OFlags, open, readlinkat};

    let link = open(
        path,
        OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(io::Error::from)?;
    if proc_mount_id(&link)? != mount_id {
        return Err(io::Error::other("time namespace mount changed"));
    }
    let namespace = readlinkat(&link, "", Vec::new()).map_err(io::Error::from)?;
    parse_namespace(
        namespace
            .to_str()
            .map_err(|_| io::Error::other("time namespace is unavailable"))?,
    )
}

#[cfg(target_os = "linux")]
pub(super) fn sample() -> io::Result<Sample> {
    use std::path::Path;

    // Linux fs/proc/uptime.c reports CLOCK_BOOTTIME in the calling thread's
    // time namespace, rounded down to centiseconds. Descriptor metadata binds
    // every source to the same full proc mount without trusting fdinfo text.
    let mount_id = proc_root_mount()?;
    let namespace_path = Path::new("/proc/thread-self/ns/time");
    let namespace = read_namespace(namespace_path, mount_id)?;
    let uptime = open_file("/proc/uptime")?;
    if proc_mount_id(&uptime)? != mount_id {
        return Err(io::Error::other("boot clock mount changed"));
    }
    let centiseconds = parse_uptime(&bounded(&uptime, 128)?)?;
    if read_namespace(namespace_path, mount_id)? != namespace {
        return Err(io::Error::other("time namespace changed"));
    }
    Ok(Sample {
        centiseconds,
        namespace,
    })
}

#[cfg(not(target_os = "linux"))]
pub(super) fn sample() -> io::Result<Sample> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "sleep-aware phone authority requires Linux",
    ))
}

#[cfg(all(test, target_os = "linux"))]
#[allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "clock test assertions"
)]
mod tests {
    use super::{
        Lease, Refusal, Sample, is_proc_mount, parse_namespace, parse_uptime, proc_mount_id,
        read_namespace, sample,
    };

    #[test]
    fn proc_clock_is_available_and_does_not_rewind() {
        let mounts = std::fs::File::open("/proc/thread-self/mountinfo").unwrap();
        let mount_id = proc_mount_id(&mounts).unwrap();
        let other_mount_id = if mount_id == 1 { 2 } else { 1 };
        assert_eq!(
            read_namespace(
                std::path::Path::new("/proc/thread-self/ns/time"),
                other_mount_id,
            )
            .unwrap_err()
            .to_string(),
            "time namespace mount changed"
        );
        let first = sample().unwrap();
        let second = sample().unwrap();
        assert_eq!(first.namespace, second.namespace);
        assert!(second.centiseconds >= first.centiseconds);
    }

    #[test]
    fn truncated_samples_expire_conservatively_at_the_boundary() {
        let first = Sample {
            centiseconds: 0,
            namespace: "time:[1]".into(),
        };
        let mut lease = Lease::new(&first).unwrap();
        let mut next = first.clone();
        next.centiseconds = 29_998;
        assert_eq!(lease.check(&next), Ok(()));
        next.centiseconds = 29_999;
        assert_eq!(lease.check(&next), Err(Refusal::Expired));
    }

    #[test]
    fn remaining_time_charges_the_truncated_fraction_and_refuses_rewind() {
        let mut sample = Sample {
            centiseconds: 0,
            namespace: "time:[1]".into(),
        };
        let mut lease = Lease::new(&sample).unwrap();
        assert_eq!(lease.remaining_ms(&sample), Ok(299_990));
        sample.centiseconds = 29_998;
        assert_eq!(lease.remaining_ms(&sample), Ok(10));
        sample.centiseconds = 29_999;
        assert_eq!(lease.remaining_ms(&sample), Err(Refusal::Expired));
        sample.centiseconds = 1;
        assert_eq!(lease.remaining_ms(&sample), Err(Refusal::Unavailable));
    }

    #[test]
    fn malformed_clock_and_other_mounts_are_rejected() {
        assert_eq!(parse_uptime("123.04 5.00\n").unwrap(), 12_304);
        for value in [
            "",
            "-1.00 0.00",
            "+1.00 0.00",
            "1.0 0.00",
            "1.001 0.00",
            "1.xx 0.00",
            "1.00",
            "1.00 0.00 extra",
            "18446744073709551615.00 0.00",
        ] {
            assert!(parse_uptime(value).is_err());
        }
        let proc_mount = "29 36 0:26 / /proc rw - proc proc rw\n";
        assert!(is_proc_mount(proc_mount, "29"));
        assert!(!is_proc_mount(proc_mount, "30"));
        assert!(!is_proc_mount(
            "29 36 0:26 /uptime /proc/uptime rw - proc proc rw",
            "29"
        ));
        assert!(!is_proc_mount(
            "29 36 0:26 / /proc rw - tmpfs tmpfs rw",
            "29"
        ));
        assert!(!is_proc_mount(
            "29 36 0:26 / /proc rw - proc proc rw\n29 36 0:26 / /proc rw - proc proc rw\n",
            "29"
        ));
        assert!(!is_proc_mount(
            "29 36 0:26 / /proc rw - proc proc rw\n29 36 0:27 / /other rw - tmpfs tmpfs rw\n",
            "29"
        ));
    }

    #[test]
    fn namespace_and_descriptor_evidence_must_be_available_and_valid() {
        use std::{fs::File, os::unix::fs::symlink};

        assert_eq!(parse_namespace("time:[123]").unwrap(), "time:[123]");
        for value in ["", "time:[]", "time:[-1]", "time:[x]", "time:[1] extra"] {
            assert_eq!(
                parse_namespace(value).unwrap_err().to_string(),
                "time namespace is unavailable"
            );
        }
        let temporary = tempfile::tempdir().unwrap();
        let missing = temporary.path().join("missing");
        assert_eq!(
            read_namespace(&missing, 1).unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
        let ordinary = temporary.path().join("ordinary");
        File::create(&ordinary).unwrap();
        assert_eq!(
            proc_mount_id(File::open(&ordinary).unwrap())
                .unwrap_err()
                .to_string(),
            "boot clock mount is unavailable"
        );
        let link = temporary.path().join("namespace");
        symlink("time:[123]", &link).unwrap();
        assert_eq!(
            read_namespace(&link, 1).unwrap_err().to_string(),
            "boot clock mount is unavailable"
        );
    }
}
