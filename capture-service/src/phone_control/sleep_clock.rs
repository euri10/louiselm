//! Linux boot-time samples for finite phone working periods.

use std::io;

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
    mounts.lines().any(|line| {
        let Some((fields, filesystem)) = line.split_once(" - ") else {
            return false;
        };
        let mut fields = fields.split_whitespace();
        fields.next() == Some(mount_id)
            && fields.nth(2) == Some("/")
            && fields.next() == Some("/proc")
            && filesystem.split_whitespace().next() == Some("proc")
    })
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
pub(super) fn sample() -> io::Result<Sample> {
    use std::{fs::File, io::Read, os::fd::AsRawFd, path::Path};

    fn bounded(path: &Path, limit: u64) -> io::Result<String> {
        let mut value = String::new();
        File::open(path)?
            .take(limit + 1)
            .read_to_string(&mut value)?;
        if value.len() as u64 > limit {
            return Err(io::Error::other("boot clock source is too large"));
        }
        Ok(value)
    }

    // Linux fs/proc/uptime.c reports CLOCK_BOOTTIME in the calling thread's
    // time namespace, rounded down to centiseconds. Check the actual mount of
    // the opened file and keep comparisons within the same namespace.
    let namespace_path = Path::new("/proc/thread-self/ns/time");
    let namespace = std::fs::read_link(namespace_path)?;
    let namespace = namespace
        .to_str()
        .filter(|value| {
            value
                .strip_prefix("time:[")
                .and_then(|value| value.strip_suffix(']'))
                .is_some_and(|value| !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()))
        })
        .ok_or_else(|| io::Error::other("time namespace is unavailable"))?
        .to_owned();
    let mut uptime = File::open("/proc/uptime")?;
    let fdinfo = bounded(
        Path::new(&format!("/proc/thread-self/fdinfo/{}", uptime.as_raw_fd())),
        4096,
    )?;
    let mount_id = fdinfo
        .lines()
        .find_map(|line| line.strip_prefix("mnt_id:\t"))
        .ok_or_else(|| io::Error::other("boot clock mount is unavailable"))?;
    let mounts = bounded(Path::new("/proc/thread-self/mountinfo"), 1_048_576)?;
    if !is_proc_mount(&mounts, mount_id) {
        return Err(io::Error::other("boot clock is not on the proc filesystem"));
    }
    let mut value = String::new();
    uptime.by_ref().take(129).read_to_string(&mut value)?;
    if value.len() > 128 {
        return Err(io::Error::other("boot clock source is too large"));
    }
    let centiseconds = parse_uptime(&value)?;
    if std::fs::read_link(namespace_path)?.to_str() != Some(namespace.as_str()) {
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
    use super::{Lease, Refusal, Sample, is_proc_mount, parse_uptime, sample};

    #[test]
    fn proc_clock_is_available_and_does_not_rewind() {
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
    }
}
