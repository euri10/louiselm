use std::{
    collections::{HashMap, HashSet},
    ffi::{CStr, OsString},
    fs::{self, File, OpenOptions, TryLockError},
    io::{self, Read, Seek, SeekFrom, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{Mutex, atomic::Ordering},
};

use serde::{Deserialize, Serialize};

use super::{
    CommandInvocation, CommandRunner, LauncherConfig, LauncherError, LauncherFailure,
    LauncherPaths, SystemCommandRunner, TEMP_COUNTER, failure, hash_file, io_error,
    read_required_json, read_text, require_configured_release, require_secure_tool,
    require_system_config, require_system_state_dirs, sync_dir, tool_error, unique_path,
};

const SUBID_OWNER: &str = "0";
const MAX_IDENTITY_SLOTS: u32 = 4096;
static IDENTITY_VALIDATION: Mutex<()> = Mutex::new(());
const POISON_MARKER: &[u8] = b"louiselm.identity.cleanup-unproven/1\n";

#[cfg(test)]
std::thread_local! {
    static FAIL_NEXT_RELEASE_SYNC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// One fixed contiguous pool from which Session identities are leased.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityPool {
    /// First usable host UID.
    pub uid_start: u32,
    /// First usable host GID.
    pub gid_start: u32,
    /// Number of UID/GID pairs in the pool.
    pub slots: u32,
}

impl IdentityPool {
    fn validate(&self) -> Result<(), LauncherError> {
        if self.slots == 0 || self.slots > MAX_IDENTITY_SLOTS {
            return Err(LauncherError::Invalid(format!(
                "identity pool slots must be between 1 and {MAX_IDENTITY_SLOTS}"
            )));
        }
        if self.uid_start == 0 || self.gid_start == 0 {
            return Err(LauncherError::Invalid(
                "identity pool may not contain the root identity".to_owned(),
            ));
        }
        self.uid_start
            .checked_add(self.slots)
            .ok_or_else(|| LauncherError::Invalid("identity UID pool overflows u32".to_owned()))?;
        self.gid_start
            .checked_add(self.slots)
            .ok_or_else(|| LauncherError::Invalid("identity GID pool overflows u32".to_owned()))?;
        Ok(())
    }

    /// Resolves one bounded slot to its host identity.
    ///
    /// # Errors
    /// Rejects invalid/overflowing pools or a slot outside the installed bounds.
    pub fn identity(&self, slot: u32) -> Result<Identity, LauncherError> {
        self.validate()?;
        if slot >= self.slots {
            return Err(LauncherError::Invalid(format!(
                "identity slot {slot} is outside the installed pool"
            )));
        }
        Ok(Identity {
            slot,
            uid: self.uid_start + slot,
            gid: self.gid_start + slot,
        })
    }
}

/// One host identity selected from the installed pool.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    /// Zero-based pool slot.
    pub slot: u32,
    /// Host UID.
    pub uid: u32,
    /// Host GID.
    pub gid: u32,
}

/// An exclusive identity lease.
///
/// Acquisition durably marks the slot unsafe to reuse. Call [`Self::release`]
/// only after the owning process tree is proved empty; dropping or poisoning
/// the lease deliberately leaves that marker behind.
#[derive(Debug)]
pub struct IdentityLease {
    identity: Identity,
    lock: File,
}

impl IdentityLease {
    /// Returns the identity held for this lease's whole lifetime.
    #[must_use]
    pub fn identity(&self) -> Identity {
        self.identity
    }

    /// Releases this slot after its process tree is proved empty.
    ///
    /// # Errors
    /// Returns marker truncation/sync or unlock errors; failure preserves a fail-closed marker or retains the kernel lock.
    pub fn release(mut self) -> Result<(), LauncherError> {
        let released = self
            .lock
            .set_len(0)
            .and_then(|()| self.lock.seek(SeekFrom::Start(0)))
            .map(|_| ())
            .and_then(|()| sync_released_lock(&self.lock));
        if let Err(source) = released {
            return self.fail_release(io_error("identity lock", source));
        }
        if let Err(source) = self.lock.unlock() {
            return self.fail_release(io_error("identity lock", source));
        }
        Ok(())
    }

    fn fail_release(mut self, error: LauncherError) -> Result<(), LauncherError> {
        if mark_active(&mut self.lock).is_err() {
            // Neither a durable empty marker nor a durable poison marker was
            // established. Retain the kernel lock so this process cannot hand
            // the identity to another Session under ambiguous state.
            std::mem::forget(self);
        } else {
            // A forked child may share this open file description until exec.
            // Unlock explicitly so its inherited descriptor cannot extend the
            // lease after the durable marker has settled the slot's authority.
            let _ = self.lock.unlock();
        }
        Err(error)
    }

    /// Permanently withholds this slot after process-tree cleanup could not be
    /// proved.
    ///
    /// Repair requires an operator to prove the old tree gone and truncate the
    /// root-owned lock file; ordinary install and acquisition never clear it.
    ///
    /// # Errors
    /// Returns an unlock error. The durable fail-closed marker remains intact regardless.
    pub fn poison(self) -> Result<(), LauncherError> {
        // Acquisition already wrote and fsynced the fail-closed marker. The
        // important action here is *not* clearing it before unlocking.
        self.lock
            .unlock()
            .map_err(|source| io_error("identity lock", source))
    }
}

fn sync_released_lock(file: &File) -> io::Result<()> {
    #[cfg(test)]
    if FAIL_NEXT_RELEASE_SYNC.with(|failure| failure.replace(false)) {
        return Err(io::Error::other("injected identity release sync failure"));
    }
    file.sync_all()
}

#[derive(Default)]
struct Accounts {
    operator_uids: HashMap<String, Vec<u32>>,
    uid_counts: HashMap<u32, usize>,
    uids: HashSet<u32>,
    gids: HashSet<u32>,
}

impl Accounts {
    fn operator_uid(&self, operator: &str) -> Result<u32, LauncherError> {
        let Some(uids) = self.operator_uids.get(operator) else {
            return Err(LauncherError::Invalid(format!(
                "operator '{operator}' is not a local account"
            )));
        };
        if uids.len() != 1 || uids[0] == 0 || self.uid_counts.get(&uids[0]).copied() != Some(1) {
            return Err(LauncherError::Invalid(format!(
                "operator '{operator}' must name exactly one non-root local account"
            )));
        }
        Ok(uids[0])
    }
}

#[derive(Clone)]
struct SubidRange {
    owner: String,
    start: u32,
    count: u32,
}

impl SubidRange {
    fn end(&self) -> Result<u32, LauncherError> {
        self.start.checked_add(self.count - 1).ok_or_else(|| {
            LauncherError::Malformed("subid identity range overflows u32".to_owned())
        })
    }

    fn overlaps(&self, other: &Self) -> Result<bool, LauncherError> {
        Ok(self.start <= other.end()? && other.start <= self.end()?)
    }
}

struct ShadowLock {
    path: PathBuf,
    dev: u64,
    ino: u64,
}

impl ShadowLock {
    fn acquire(database: &Path) -> Result<Self, LauncherError> {
        let lock = PathBuf::from(format!("{}.lock", database.display()));
        let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temporary = PathBuf::from(format!(
            "{}.{}.{counter}",
            database.display(),
            std::process::id()
        ));
        match fs::symlink_metadata(&temporary) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err(LauncherError::Invalid(format!(
                    "subid identity lock temporary '{}' is not a regular file",
                    temporary.display()
                )));
            }
            Ok(_) => fs::remove_file(&temporary)
                .map_err(|source| io_error(database.display().to_string(), source))?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(io_error(database.display().to_string(), source)),
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|source| io_error(database.display().to_string(), source))?;
        let mut linked = false;
        let write_result = (|| {
            write!(file, "{}\0", std::process::id())?;
            file.sync_all()?;
            match fs::hard_link(&temporary, &lock) {
                Ok(()) => linked = true,
                Err(error)
                    if error.kind() == io::ErrorKind::AlreadyExists
                        && reclaim_stale_shadow_lock(&lock)? =>
                {
                    fs::hard_link(&temporary, &lock)?;
                    linked = true;
                }
                Err(error) => return Err(error),
            }
            let metadata = fs::metadata(&temporary)?;
            if metadata.nlink() != 2 {
                return Err(io::Error::other("subid lock hard link was not established"));
            }
            Ok::<_, io::Error>((metadata.dev(), metadata.ino()))
        })();
        if write_result.is_err()
            && linked
            && let (Ok(source), Ok(target)) = (fs::metadata(&temporary), fs::metadata(&lock))
            && source.dev() == target.dev()
            && source.ino() == target.ino()
        {
            let _ = fs::remove_file(&lock);
        }
        let _ = fs::remove_file(&temporary);
        match write_result {
            Ok((dev, ino)) => Ok(Self {
                path: lock,
                dev,
                ino,
            }),
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
                Err(LauncherError::SubidBusy {
                    database: database.display().to_string(),
                })
            }
            Err(source) => Err(io_error(database.display().to_string(), source)),
        }
    }
}

fn reclaim_stale_shadow_lock(path: &Path) -> io::Result<bool> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(io::Error::other("subid lock is not a regular file"));
    }
    let bytes = fs::read(path)?;
    if bytes.len() > 32 {
        return Err(io::Error::other("subid lock PID is malformed"));
    }
    let raw = std::str::from_utf8(&bytes)
        .map_err(|_| io::Error::other("subid lock PID is malformed"))?
        .trim_matches(|character: char| character == '\0' || character.is_ascii_whitespace());
    let raw_pid: i32 = raw
        .parse()
        .map_err(|_| io::Error::other("subid lock PID is malformed"))?;
    let pid = rustix::process::Pid::from_raw(raw_pid)
        .ok_or_else(|| io::Error::other("subid lock PID is malformed"))?;
    match rustix::process::test_kill_process(pid) {
        Ok(()) | Err(rustix::io::Errno::PERM) => Ok(false),
        Err(rustix::io::Errno::SRCH) => {
            let found = fs::symlink_metadata(path)?;
            if found.dev() != metadata.dev() || found.ino() != metadata.ino() {
                return Ok(false);
            }
            fs::remove_file(path)?;
            Ok(true)
        }
        Err(error) => Err(io::Error::from(error)),
    }
}

impl Drop for ShadowLock {
    fn drop(&mut self) {
        let Ok(metadata) = fs::symlink_metadata(&self.path) else {
            return;
        };
        if metadata.dev() == self.dev && metadata.ino() == self.ino {
            let _ = fs::remove_file(&self.path);
        }
    }
}

pub(super) fn validate_pool(pool: &IdentityPool) -> Result<(), LauncherError> {
    pool.validate()
}

pub(super) fn validate_install_authority(
    paths: &LauncherPaths,
    runner: &impl CommandRunner,
    pool: &IdentityPool,
    operator: &str,
) -> Result<u32, LauncherError> {
    let accounts = read_accounts(paths)?;
    let operator_uid = accounts.operator_uid(operator)?;
    validate_identity_authority(paths, runner, pool, &accounts, false)?;
    Ok(operator_uid)
}

pub(super) fn reserve_install_authority(
    paths: &LauncherPaths,
    runner: &impl CommandRunner,
    pool: &IdentityPool,
) -> Result<(), LauncherError> {
    let _subuid_lock = ShadowLock::acquire(&paths.subuid)?;
    let _subgid_lock = ShadowLock::acquire(&paths.subgid)?;
    let accounts = read_accounts(paths)?;
    validate_identity_authority(paths, runner, pool, &accounts, false)?;
    reserve_subid(&paths.subuid, pool.uid_start, pool.slots)?;
    reserve_subid(&paths.subgid, pool.gid_start, pool.slots)
}

pub(super) fn validate_installed_authority(
    paths: &LauncherPaths,
    runner: &impl CommandRunner,
    config: &LauncherConfig,
) -> Result<(), LauncherError> {
    if *paths == LauncherPaths::system() {
        require_secure_tool(&config.getent_path, "getent")?;
    }
    require_system_files(paths)?;
    require_measured_tool(config)?;
    let _validation = IDENTITY_VALIDATION
        .lock()
        .map_err(|_| LauncherError::Invalid("identity validation lock was poisoned".to_owned()))?;
    let _subuid_lock = ShadowLock::acquire(&paths.subuid)?;
    let _subgid_lock = ShadowLock::acquire(&paths.subgid)?;
    let accounts = read_accounts(paths)?;
    if accounts.operator_uid(&config.operator)? != config.operator_uid {
        return Err(LauncherError::Invalid(
            "configured operator name no longer has its pinned UID".to_owned(),
        ));
    }
    validate_identity_authority(paths, runner, &config.pool, &accounts, true)
}

pub(super) fn require_system_install_context(paths: &LauncherPaths) -> Result<(), LauncherError> {
    if *paths == LauncherPaths::system() {
        require_secure_tool(&paths.getent, "getent")?;
    }
    require_system_files(paths)
}

fn require_system_files(paths: &LauncherPaths) -> Result<(), LauncherError> {
    if *paths != LauncherPaths::system() {
        return Ok(());
    }
    for (path, label) in [
        (&paths.passwd, "passwd"),
        (&paths.group, "group"),
        (&paths.subuid, "subuid"),
        (&paths.subgid, "subgid"),
        (&paths.nsswitch, "nsswitch"),
    ] {
        let metadata = fs::symlink_metadata(path).map_err(|source| io_error(label, source))?;
        if metadata.uid() != 0
            || !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.mode() & 0o022 != 0
        {
            return Err(LauncherError::Invalid(format!(
                "{label} identity authority is not a root-owned non-writable regular file"
            )));
        }
    }
    Ok(())
}

pub(super) fn acquire(paths: &LauncherPaths, slot: u32) -> Result<IdentityLease, LauncherError> {
    acquire_with_runner(paths, slot, &SystemCommandRunner)
}

pub(super) fn acquire_with_runner(
    paths: &LauncherPaths,
    slot: u32,
    runner: &impl CommandRunner,
) -> Result<IdentityLease, LauncherError> {
    super::validate_paths(paths)?;
    require_system_config(paths)?;
    let config: LauncherConfig = read_required_json(&paths.config())?;
    super::validate_config(&config, paths)?;
    require_configured_release(paths, &config)?;
    require_system_state_dirs(paths)?;
    if *paths == LauncherPaths::system() {
        require_secure_tool(&config.getent_path, "getent")?;
    }
    require_system_files(paths)?;
    require_measured_tool(&config)?;
    let identity = config.pool.identity(slot)?;
    let _validation = IDENTITY_VALIDATION
        .lock()
        .map_err(|_| LauncherError::Invalid("identity validation lock was poisoned".to_owned()))?;
    let _subuid_lock = ShadowLock::acquire(&paths.subuid)?;
    let _subgid_lock = ShadowLock::acquire(&paths.subgid)?;
    let accounts = read_accounts(paths)?;
    validate_identity_authority(paths, runner, &config.pool, &accounts, true)?;
    let path = paths.locks().join(format!("{slot}.lock"));
    let mut file = open_existing_regular(&path, *paths == LauncherPaths::system())?;
    match file.try_lock() {
        Ok(()) => {
            require_unpoisoned(&mut file, slot)?;
            mark_active(&mut file)?;
            Ok(IdentityLease {
                identity,
                lock: file,
            })
        }
        Err(TryLockError::WouldBlock) => Err(LauncherError::Occupied { slot }),
        Err(TryLockError::Error(source)) => Err(io_error("identity lock", source)),
    }
}

fn mark_active(file: &mut File) -> Result<(), LauncherError> {
    file.seek(SeekFrom::Start(0))
        .and_then(|_| file.write_all(POISON_MARKER))
        .and_then(|()| file.set_len(POISON_MARKER.len() as u64))
        .and_then(|()| file.sync_all())
        .map_err(|source| io_error("identity lock", source))
}

fn require_unpoisoned(file: &mut File, slot: u32) -> Result<(), LauncherError> {
    file.seek(SeekFrom::Start(0))
        .map_err(|source| io_error("identity lock", source))?;
    let mut marker = Vec::new();
    file.take((POISON_MARKER.len() + 1) as u64)
        .read_to_end(&mut marker)
        .map_err(|source| io_error("identity lock", source))?;
    if marker.is_empty() {
        Ok(())
    } else if marker == POISON_MARKER {
        Err(LauncherError::Poisoned { slot })
    } else {
        Err(LauncherError::Invalid(
            "identity lock contains an unknown persistent marker".to_owned(),
        ))
    }
}

fn read_accounts(paths: &LauncherPaths) -> Result<Accounts, LauncherError> {
    let passwd = read_text(&paths.passwd)?;
    let group = read_text(&paths.group)?;
    let mut result = Accounts::default();
    for (line_number, line) in passwd.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        let fields = line.split(':').collect::<Vec<_>>();
        if fields.len() < 4 || fields[0].is_empty() {
            return Err(LauncherError::Malformed(format!(
                "passwd identity line {} is malformed",
                line_number + 1
            )));
        }
        let uid = parse_id(fields[2], "passwd UID", line_number + 1)?;
        let gid = parse_id(fields[3], "passwd GID", line_number + 1)?;
        result
            .operator_uids
            .entry(fields[0].to_owned())
            .or_default()
            .push(uid);
        *result.uid_counts.entry(uid).or_default() += 1;
        result.uids.insert(uid);
        result.gids.insert(gid);
    }
    for (line_number, line) in group.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        let fields = line.split(':').collect::<Vec<_>>();
        if fields.len() < 3 || fields[0].is_empty() {
            return Err(LauncherError::Malformed(format!(
                "group identity line {} is malformed",
                line_number + 1
            )));
        }
        result
            .gids
            .insert(parse_id(fields[2], "group GID", line_number + 1)?);
    }
    Ok(result)
}

fn parse_id(raw: &str, kind: &str, line: usize) -> Result<u32, LauncherError> {
    raw.parse().map_err(|_| {
        LauncherError::Malformed(format!("{kind} on identity line {line} is not a u32"))
    })
}

fn validate_identity_authority(
    paths: &LauncherPaths,
    runner: &impl CommandRunner,
    pool: &IdentityPool,
    accounts: &Accounts,
    reservation_required: bool,
) -> Result<(), LauncherError> {
    pool.validate()?;
    validate_subid_backend(&paths.nsswitch)?;
    let uid_ranges = parse_subids(&paths.subuid)?;
    let gid_ranges = parse_subids(&paths.subgid)?;
    let user_reservation = SubidRange {
        owner: SUBID_OWNER.to_owned(),
        start: pool.uid_start,
        count: pool.slots,
    };
    let group_reservation = SubidRange {
        owner: SUBID_OWNER.to_owned(),
        start: pool.gid_start,
        count: pool.slots,
    };
    validate_subid_ranges(&uid_ranges, &user_reservation, "UID", reservation_required)?;
    validate_subid_ranges(&gid_ranges, &group_reservation, "GID", reservation_required)?;
    validate_effective_identity_pool(paths, runner, pool)?;
    for offset in 0..pool.slots {
        if accounts.uids.contains(&(pool.uid_start + offset)) {
            return Err(LauncherError::Invalid(format!(
                "identity UID {} already belongs to a host account",
                pool.uid_start + offset
            )));
        }
        if accounts.gids.contains(&(pool.gid_start + offset)) {
            return Err(LauncherError::Invalid(format!(
                "identity GID {} already belongs to a host account or group",
                pool.gid_start + offset
            )));
        }
    }
    Ok(())
}

fn validate_effective_identity_pool(
    paths: &LauncherPaths,
    runner: &impl CommandRunner,
    pool: &IdentityPool,
) -> Result<(), LauncherError> {
    for (database, start, label) in [
        ("passwd", pool.uid_start, "UID"),
        ("group", pool.gid_start, "GID"),
    ] {
        let mut arguments = Vec::with_capacity(pool.slots as usize + 1);
        arguments.push(OsString::from(database));
        for offset in 0..pool.slots {
            arguments.push(OsString::from((start + offset).to_string()));
        }
        let output = runner
            .run(&CommandInvocation {
                program: paths.getent.clone(),
                arguments,
                stdin: Vec::new(),
                current_dir: None,
            })
            .map_err(|source| io_error("getent", source))?;
        if output.stdout.len() > 4 * 1024 * 1024 {
            return Err(LauncherError::Invalid(
                "effective identity lookup exceeded its output limit".to_owned(),
            ));
        }
        let found = std::str::from_utf8(&output.stdout).map_err(|_| {
            LauncherError::Malformed("getent returned non-UTF-8 identity data".to_owned())
        })?;
        if !found.trim().is_empty() {
            return Err(LauncherError::Invalid(format!(
                "identity {label} pool collides with an effective NSS principal"
            )));
        }
        if output.exit_code != Some(2) {
            return Err(tool_error("getent", &output.stderr));
        }
    }
    Ok(())
}

fn validate_subid_backend(path: &Path) -> Result<(), LauncherError> {
    let text = read_text(path)?;
    let mut seen = HashSet::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let Some((database, providers)) = line.split_once(':') else {
            return Err(LauncherError::Malformed(
                "nsswitch identity configuration is malformed".to_owned(),
            ));
        };
        let database = database.trim();
        if !matches!(database, "passwd" | "group" | "subid") {
            continue;
        }
        let providers = providers.split_whitespace().collect::<Vec<_>>();
        let unusable = match database {
            "subid" => providers != ["files"],
            _ => !providers.contains(&"files"),
        };
        if !seen.insert(database.to_owned()) || unusable {
            return Err(LauncherError::Invalid(format!(
                "{database} identity authority must include the local files backend, and subid must use it exclusively"
            )));
        }
    }
    if !seen.contains("passwd") || !seen.contains("group") {
        return Err(LauncherError::Invalid(
            "passwd and group identity authority must explicitly use the local files backend"
                .to_owned(),
        ));
    }
    // shadow-utils defaults subid to files when no subid line is configured.
    Ok(())
}

fn parse_subids(path: &Path) -> Result<Vec<SubidRange>, LauncherError> {
    let text = read_text(path)?;
    let mut ranges = Vec::new();
    for (line_number, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let fields = line.split(':').collect::<Vec<_>>();
        if fields.len() != 3 || fields[0].is_empty() {
            return Err(LauncherError::Malformed(format!(
                "subid identity line {} is malformed",
                line_number + 1
            )));
        }
        let start = parse_id(fields[1], "subid start", line_number + 1)?;
        let count = parse_id(fields[2], "subid count", line_number + 1)?;
        if count == 0 {
            return Err(LauncherError::Malformed(format!(
                "subid identity line {} has zero count",
                line_number + 1
            )));
        }
        let range = SubidRange {
            owner: fields[0].to_owned(),
            start,
            count,
        };
        range.end()?;
        ranges.push(range);
    }
    for left in 0..ranges.len() {
        for right in (left + 1)..ranges.len() {
            if ranges[left].overlaps(&ranges[right])? {
                return Err(LauncherError::Invalid(format!(
                    "subid identity ranges for '{}' and '{}' overlap",
                    ranges[left].owner, ranges[right].owner
                )));
            }
        }
    }
    Ok(ranges)
}

fn validate_subid_ranges(
    existing: &[SubidRange],
    requested: &SubidRange,
    kind: &str,
    reservation_required: bool,
) -> Result<(), LauncherError> {
    let mut exact = 0;
    for range in existing {
        if range.owner == SUBID_OWNER {
            if range.start == requested.start && range.count == requested.count {
                exact += 1;
                continue;
            }
            return Err(LauncherError::Invalid(format!(
                "installed root-owned subid {kind} identity range does not match the requested pool"
            )));
        }
        if range.overlaps(requested)? {
            return Err(LauncherError::Invalid(format!(
                "requested subid {kind} identity range overlaps owner '{}'",
                range.owner
            )));
        }
    }
    if exact > 1 {
        return Err(LauncherError::Invalid(format!(
            "subid {kind} identity reservation is duplicated"
        )));
    }
    if reservation_required && exact != 1 {
        return Err(LauncherError::Invalid(format!(
            "subid {kind} identity reservation is missing"
        )));
    }
    Ok(())
}

fn reserve_subid(path: &Path, start: u32, count: u32) -> Result<(), LauncherError> {
    let text = read_text(path)?;
    let ranges = parse_subids(path)?;
    if ranges
        .iter()
        .any(|range| range.owner == SUBID_OWNER && range.start == start && range.count == count)
    {
        return Ok(());
    }
    let mut next = text.into_bytes();
    if !next.is_empty() && !next.ends_with(b"\n") {
        next.push(b'\n');
    }
    next.extend_from_slice(format!("{SUBID_OWNER}:{start}:{count}\n").as_bytes());
    write_metadata_preserving_atomic(path, &next)
}

fn require_measured_tool(config: &LauncherConfig) -> Result<(), LauncherError> {
    let found = hash_file(&config.getent_path, "getent")?.to_string();
    if found != config.getent_digest {
        return Err(LauncherError::Invalid(
            "measured getent changed after installation".to_owned(),
        ));
    }
    Ok(())
}

pub(super) fn ensure_slot_files(paths: &LauncherPaths, slots: u32) -> Result<(), LauncherError> {
    for slot in 0..slots {
        let path = paths.locks().join(format!("{slot}.lock"));
        match fs::symlink_metadata(&path) {
            Ok(metadata)
                if !metadata.is_file()
                    || metadata.file_type().is_symlink()
                    || (*paths == LauncherPaths::system() && metadata.uid() != 0) =>
            {
                return Err(LauncherError::Invalid(
                    "identity lock is not a safely owned regular file".to_owned(),
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&path)
                    .map_err(|source| io_error("identity lock", source))?;
                file.sync_all()
                    .map_err(|source| io_error("identity lock", source))?;
            }
            Err(source) => return Err(io_error("identity lock", source)),
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .map_err(|source| io_error("identity lock", source))?;
    }
    sync_dir(&paths.locks())
}

pub(super) fn occupied_slots(
    paths: &LauncherPaths,
    slots: u32,
    failures: &mut Vec<LauncherFailure>,
) -> Vec<u32> {
    let mut occupied = Vec::new();
    for slot in 0..slots {
        let path = paths.locks().join(format!("{slot}.lock"));
        match open_existing_regular(&path, *paths == LauncherPaths::system()).and_then(
            |mut file| match file.try_lock() {
                Ok(()) => require_unpoisoned(&mut file, slot).map(|()| false),
                Err(TryLockError::WouldBlock) => Ok(true),
                Err(TryLockError::Error(source)) => Err(io_error("identity lock", source)),
            },
        ) {
            Ok(true) => occupied.push(slot),
            Ok(false) => {}
            Err(LauncherError::Poisoned { slot }) => failures.push(failure(
                "identity_slot_poisoned",
                format!("Identity slot {slot} is withheld after unproven process cleanup."),
                "Prove the old process tree gone, then clear the slot marker as root.",
            )),
            Err(error) => failures.push(failure(
                "identity_lock_unreadable",
                error.to_string(),
                "Repair the persistent identity lock files as root.",
            )),
        }
    }
    occupied
}

fn open_existing_regular(path: &Path, require_root: bool) -> Result<File, LauncherError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|source| io_error("identity lock", source))?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.mode() & 0o777 != 0o600
        || (require_root && metadata.uid() != 0)
    {
        return Err(LauncherError::Invalid(
            "identity lock is not a root-only regular file".to_owned(),
        ));
    }
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|source| io_error("identity lock", source))
}

fn write_metadata_preserving_atomic(path: &Path, bytes: &[u8]) -> Result<(), LauncherError> {
    let parent = path
        .parent()
        .ok_or_else(|| LauncherError::Invalid(format!("'{}' has no parent", path.display())))?;
    let source = File::open(path).map_err(|error| io_error(path.display().to_string(), error))?;
    let source_metadata = source
        .metadata()
        .map_err(|error| io_error(path.display().to_string(), error))?;
    if !source_metadata.is_file() {
        return Err(LauncherError::Invalid(format!(
            "'{}' is not a regular identity database",
            path.display()
        )));
    }
    let temporary = unique_path(parent, ".pending");
    let result = (|| {
        let mut pending = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(source_metadata.mode() & 0o777)
            .open(&temporary)
            .map_err(|error| io_error(path.display().to_string(), error))?;
        pending
            .write_all(bytes)
            .map_err(|error| io_error(path.display().to_string(), error))?;
        let pending_metadata = pending
            .metadata()
            .map_err(|error| io_error(path.display().to_string(), error))?;
        if pending_metadata.uid() != source_metadata.uid()
            || pending_metadata.gid() != source_metadata.gid()
        {
            rustix::fs::fchown(
                &pending,
                Some(rustix::process::Uid::from_raw(source_metadata.uid())),
                Some(rustix::process::Gid::from_raw(source_metadata.gid())),
            )
            .map_err(|error| io_error(path.display().to_string(), io::Error::from(error)))?;
        }
        pending
            .set_permissions(fs::Permissions::from_mode(source_metadata.mode() & 0o777))
            .map_err(|error| io_error(path.display().to_string(), error))?;
        copy_xattrs(&source, &pending, path)?;
        pending
            .sync_all()
            .map_err(|error| io_error(path.display().to_string(), error))?;
        fs::rename(&temporary, path)
            .map_err(|error| io_error(path.display().to_string(), error))?;
        sync_dir(parent)
    })();
    let _ = fs::remove_file(&temporary);
    result
}

fn copy_xattrs(source: &File, target: &File, path: &Path) -> Result<(), LauncherError> {
    let mut names = vec![0; 65_536];
    let names_length = match rustix::fs::flistxattr(source, &mut names) {
        Ok(length) => length,
        Err(rustix::io::Errno::NOTSUP) => return Ok(()),
        Err(error) => {
            return Err(io_error(path.display().to_string(), io::Error::from(error)));
        }
    };
    names.truncate(names_length);
    for raw_name in names.split_inclusive(|byte| *byte == 0) {
        let name = CStr::from_bytes_with_nul(raw_name).map_err(|_| {
            LauncherError::Malformed("identity database has malformed xattr names".to_owned())
        })?;
        let name_bytes = name.to_bytes();
        if name_bytes != b"security.selinux"
            && name_bytes != b"system.posix_acl_access"
            && !name_bytes.starts_with(b"user.")
        {
            // Integrity labels such as security.ima and security.evm bind the
            // old file contents and must never be copied onto rewritten bytes.
            continue;
        }
        let mut value = vec![0; 65_536];
        let value_length = rustix::fs::fgetxattr(source, name, &mut value)
            .map_err(|error| io_error(path.display().to_string(), io::Error::from(error)))?;
        value.truncate(value_length);
        rustix::fs::fsetxattr(target, name, &value, rustix::fs::XattrFlags::empty())
            .map_err(|error| io_error(path.display().to_string(), io::Error::from(error)))?;
    }
    Ok(())
}

pub(super) fn check_secure_tool(path: &Path, failures: &mut Vec<LauncherFailure>) -> bool {
    match fs::symlink_metadata(path) {
        Ok(metadata)
            if metadata.is_file()
                && !metadata.file_type().is_symlink()
                && metadata.uid() == 0
                && metadata.mode() & 0o022 == 0
                && metadata.mode() & 0o111 != 0 =>
        {
            true
        }
        Ok(_) => {
            failures.push(failure(
                "getent_permissions",
                "The measured getent is not a root-owned, non-writable executable.",
                "Restore the system identity resolver before leasing host identities.",
            ));
            false
        }
        Err(error) => {
            failures.push(failure(
                "getent_unreadable",
                error.to_string(),
                "Restore the measured getent executable.",
            ));
            false
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]
mod tests {
    use std::{
        os::unix::fs::{PermissionsExt, symlink},
        time::{Duration, Instant},
    };

    use crate::{
        Digest,
        install::{InstalledState, STATE_SCHEMA as RELEASE_STATE_SCHEMA},
        release::{
            Component, MANIFEST_SCHEMA, PolicyIdentity, ReleaseManifest, SourceIdentity,
            ToolchainIdentity,
        },
    };

    use super::super::{
        CONFIG_SCHEMA, acquire_identity, acquire_identity_with_deadline,
        runtime_config_with_deadline,
    };
    use super::*;

    static DEADLINE_TESTS: Mutex<()> = Mutex::new(());
    const DEADLINE_TEST_TIMEOUT: Duration = Duration::from_secs(1);

    struct DeadlineIdentityFixture {
        _root: tempfile::TempDir,
        paths: LauncherPaths,
        hold_path: PathBuf,
        pid_path: PathBuf,
    }

    impl DeadlineIdentityFixture {
        #[expect(
            clippy::too_many_lines,
            reason = "The fixture materializes one coherent measured installation and identity environment."
        )]
        fn new() -> Self {
            let root = tempfile::tempdir().expect("temporary launcher root");
            let root_path = root.path();
            let release_prefix = root_path.join("release");
            let paths = LauncherPaths {
                state_root: release_prefix.join("launcher"),
                release_prefix,
                sudoers: root_path.join("sudoers"),
                subuid: root_path.join("subuid"),
                subgid: root_path.join("subgid"),
                passwd: root_path.join("passwd"),
                group: root_path.join("group"),
                nsswitch: root_path.join("nsswitch"),
                ssh_keygen: root_path.join("ssh-keygen"),
                getent: root_path.join("getent"),
                visudo: root_path.join("visudo"),
                bwrap: PathBuf::from("/usr/bin/true"),
                broker_socket: root_path.join("control.sock"),
            };
            fs::create_dir_all(paths.state_root.join("locks")).expect("launcher locks directory");
            fs::write(&paths.subuid, "0:200000:1\n").expect("subuid fixture");
            fs::write(&paths.subgid, "0:300000:1\n").expect("subgid fixture");
            fs::write(
                &paths.passwd,
                "root:x:0:0:root:/root:/bin/sh\nlouise:x:1000:1000::/home/louise:/bin/sh\n",
            )
            .expect("passwd fixture");
            fs::write(&paths.group, "root:x:0:\nlouise:x:1000:\n").expect("group fixture");
            fs::write(
                &paths.nsswitch,
                "passwd: files\ngroup: files\nsubid: files\n",
            )
            .expect("nsswitch fixture");
            fs::write(
                &paths.getent,
                "#!/bin/sh\nif [ -e \"$0.hold\" ]; then printf '%s\\n' \"$$\" > \"$0.pid\"; exec /usr/bin/sleep 60; fi\nexit 2\n",
            )
            .expect("getent fixture");
            fs::set_permissions(&paths.getent, fs::Permissions::from_mode(0o755))
                .expect("getent fixture mode");

            let launcher = b"measured launcher\n";
            let launcher_digest = Digest::of(launcher).to_string();
            let mut manifest = ReleaseManifest {
                schema: MANIFEST_SCHEMA.to_owned(),
                release_id: String::new(),
                version: "0.1.0".to_owned(),
                built_at_ms: 1,
                source: SourceIdentity {
                    commit: "0123456789abcdef0123456789abcdef01234567".to_owned(),
                    clean: true,
                    describe: "test".to_owned(),
                    dependencies_digest: Digest::of(b"Cargo.lock").to_string(),
                },
                toolchain: ToolchainIdentity {
                    rustc: "1.97.1".to_owned(),
                    cargo: "1.97.1".to_owned(),
                    target: "x86_64-linux".to_owned(),
                },
                policy: PolicyIdentity {
                    version: "1".to_owned(),
                    digest: Digest::of(b"policy").to_string(),
                },
                schemas: Vec::new(),
                components: vec![Component {
                    name: "louiselm-launch".to_owned(),
                    path: "bin/louiselm-launch".to_owned(),
                    sha256: Digest::of(launcher).hex().to_owned(),
                    size: launcher.len() as u64,
                    executable: true,
                }],
            };
            manifest.release_id = manifest.digest().to_string();
            let release_root = paths
                .release_prefix
                .join("releases")
                .join(&manifest.release_id);
            fs::create_dir_all(release_root.join("bin")).expect("release directory");
            let launcher_path = release_root.join("bin/louiselm-launch");
            fs::write(&launcher_path, launcher).expect("launcher fixture");
            fs::set_permissions(&launcher_path, fs::Permissions::from_mode(0o555))
                .expect("launcher fixture mode");
            fs::write(
                release_root.join("manifest.json"),
                serde_json::to_vec(&manifest).expect("manifest serializes"),
            )
            .expect("manifest fixture");
            symlink(
                Path::new("releases").join(&manifest.release_id),
                paths.release_prefix.join("current"),
            )
            .expect("current release link");
            fs::write(
                paths.release_prefix.join("state.json"),
                serde_json::to_vec(&InstalledState {
                    schema: RELEASE_STATE_SCHEMA.to_owned(),
                    release_id: manifest.release_id.clone(),
                    built_at_ms: manifest.built_at_ms,
                    installed_at_ms: 2,
                    source_commit: manifest.source.commit.clone(),
                    policy_version: manifest.policy.version.clone(),
                })
                .expect("installed state serializes"),
            )
            .expect("installed state fixture");
            fs::write(
                paths.state_root.join("config.json"),
                serde_json::to_vec(&LauncherConfig {
                    schema: CONFIG_SCHEMA.to_owned(),
                    operator: "louise".to_owned(),
                    operator_uid: 1_000,
                    broker_uid: 1_500,
                    broker_gid: 1_500,
                    broker_socket_path: paths.broker_socket.clone(),
                    release_id: manifest.release_id,
                    launcher_digest,
                    launcher_path: paths.launcher(),
                    ssh_keygen_path: paths.ssh_keygen.clone(),
                    ssh_keygen_digest: Digest::of(b"ssh-keygen").to_string(),
                    getent_path: paths.getent.clone(),
                    getent_digest: hash_file(&paths.getent, "getent")
                        .expect("getent fixture is readable")
                        .to_string(),
                    bwrap_path: paths.bwrap.clone(),
                    bwrap_digest: hash_file(&paths.bwrap, "bubblewrap")
                        .expect("bubblewrap fixture is readable")
                        .to_string(),
                    pool: IdentityPool {
                        uid_start: 200_000,
                        gid_start: 300_000,
                        slots: 1,
                    },
                })
                .expect("launcher config serializes"),
            )
            .expect("launcher config fixture");
            ensure_slot_files(&paths, 1).expect("identity slot fixture");

            Self {
                hold_path: paths.getent.with_extension("hold"),
                pid_path: paths.getent.with_extension("pid"),
                _root: root,
                paths,
            }
        }
    }

    #[test]
    fn identity_deadline_reaps_getent_without_acquiring_or_poisoning_the_slot() {
        let _serial = DEADLINE_TESTS.lock().expect("deadline test lock");
        let fixture = DeadlineIdentityFixture::new();
        fs::write(&fixture.hold_path, b"hold").expect("getent hold marker");
        let started = Instant::now();

        let error = acquire_identity_with_deadline(
            &fixture.paths,
            0,
            Instant::now() + DEADLINE_TEST_TIMEOUT,
        )
        .expect_err("hung NSS lookup must observe the operation deadline");

        match &error {
            LauncherError::Io { source, .. } => {
                assert_eq!(source.kind(), io::ErrorKind::TimedOut);
            }
            _ => panic!("deadline must remain an I/O timeout: {error}"),
        }
        assert!(started.elapsed() < Duration::from_secs(2));
        let pid = fs::read_to_string(&fixture.pid_path)
            .expect("getent helper records its pid")
            .trim()
            .parse::<i32>()
            .expect("getent helper records a numeric pid");
        let pid = rustix::process::Pid::from_raw(pid).expect("getent helper has a positive pid");
        assert_eq!(
            rustix::process::test_kill_process(pid),
            Err(rustix::io::Errno::SRCH),
            "the deadline must reap the held getent helper before returning"
        );
        assert_eq!(
            fs::metadata(fixture.paths.locks().join("0.lock"))
                .expect("identity lock remains")
                .len(),
            0,
            "NSS failure must happen before the slot is marked active"
        );

        fs::remove_file(&fixture.hold_path).expect("getent hold marker is removable");
        acquire_identity(&fixture.paths, 0)
            .expect("the failed bounded lookup left the slot reusable")
            .release()
            .expect("reacquired slot releases cleanly");
    }

    #[test]
    fn runtime_config_deadline_reaps_held_getent_validation() {
        let _serial = DEADLINE_TESTS.lock().expect("deadline test lock");
        let fixture = DeadlineIdentityFixture::new();
        fs::write(&fixture.hold_path, b"hold").expect("getent hold marker");
        let started = Instant::now();

        let error =
            runtime_config_with_deadline(&fixture.paths, Instant::now() + DEADLINE_TEST_TIMEOUT)
                .expect_err("hung runtime NSS validation must observe the operation deadline");

        match &error {
            LauncherError::Io { source, .. } => {
                assert_eq!(source.kind(), io::ErrorKind::TimedOut);
            }
            _ => panic!("deadline must remain an I/O timeout: {error}"),
        }
        assert!(started.elapsed() < Duration::from_secs(2));
        let pid = fs::read_to_string(&fixture.pid_path)
            .expect("getent helper records its pid")
            .trim()
            .parse::<i32>()
            .expect("getent helper records a numeric pid");
        let pid = rustix::process::Pid::from_raw(pid).expect("getent helper has a positive pid");
        assert_eq!(
            rustix::process::test_kill_process(pid),
            Err(rustix::io::Errno::SRCH),
            "runtime validation must reap the held getent helper before returning"
        );
    }

    #[test]
    fn successful_release_unlocks_a_fork_equivalent_descriptor() {
        let directory = tempfile::tempdir().expect("temporary identity directory");
        let path = directory.path().join("6.lock");
        let mut lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .expect("identity lock");
        mark_active(&mut lock).expect("initial poison marker");
        lock.try_lock().expect("exclusive lease");
        let lease = IdentityLease {
            identity: Identity {
                slot: 6,
                uid: 200_006,
                gid: 200_006,
            },
            lock,
        };
        let _inherited_lock = lease
            .lock
            .try_clone()
            .expect("a fork-equivalent descriptor can inherit the lease");

        lease.release().expect("clean lease release");

        let mut reopened = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("reopened identity lock");
        reopened.try_lock().expect("released lease unlocked");
        require_unpoisoned(&mut reopened, 6).expect("clean release cleared the poison marker");
    }

    #[test]
    fn release_sync_failure_keeps_the_slot_poisoned_after_unlock() {
        let directory = tempfile::tempdir().expect("temporary identity directory");
        let path = directory.path().join("7.lock");
        let mut lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .expect("identity lock");
        mark_active(&mut lock).expect("initial poison marker");
        lock.try_lock().expect("exclusive lease");
        let lease = IdentityLease {
            identity: Identity {
                slot: 7,
                uid: 200_007,
                gid: 200_007,
            },
            lock,
        };
        let _inherited_lock = lease
            .lock
            .try_clone()
            .expect("a fork-equivalent descriptor can inherit the lease");

        FAIL_NEXT_RELEASE_SYNC.with(|failure| failure.set(true));
        assert!(lease.release().is_err());

        let mut reopened = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("reopened identity lock");
        reopened.try_lock().expect("failed lease unlocked");
        assert!(matches!(
            require_unpoisoned(&mut reopened, 7),
            Err(LauncherError::Poisoned { slot: 7 })
        ));
    }
}
