//! Hold every idle identity during the conformance-only maintenance write.
use super::{
    File, LauncherError, LauncherPaths, MAX_IDENTITY_SLOTS, TryLockError, io_error,
    open_existing_regular, require_unpoisoned,
};

pub(in crate::launcher_install) struct IdlePool(Vec<File>);

impl Drop for IdlePool {
    fn drop(&mut self) {
        for file in &self.0 {
            // No process may be spawned while this guard is held. Unlock explicitly
            // even if another descriptor retains the open file description.
            // A failed unlock retains the lock until our last descriptor closes;
            // this maintenance guard never clears poison or grants an identity.
            let _ = file.unlock();
        }
    }
}

pub(in crate::launcher_install) fn hold(
    paths: &LauncherPaths,
    slots: u32,
) -> Result<IdlePool, LauncherError> {
    if !(1..=MAX_IDENTITY_SLOTS).contains(&slots) {
        return Err(LauncherError::Invalid("QA identity pool invalid".into()));
    }
    let mut held = IdlePool(Vec::new());
    for slot in 0..slots {
        let file = open_existing_regular(
            &paths.locks().join(format!("{slot}.lock")),
            *paths == LauncherPaths::system(),
        )?;
        match file.try_lock() {
            Ok(()) => {
                held.0.push(file);
                let index = held.0.len() - 1;
                require_unpoisoned(&mut held.0[index], slot)?;
            }
            Err(TryLockError::WouldBlock) => return Err(LauncherError::Occupied { slot }),
            Err(TryLockError::Error(source)) => return Err(io_error("QA identity lock", source)),
        }
    }
    Ok(held)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "Real advisory-lock fixture asserts acquisition and release."
)]
mod tests {
    use super::super::POISON_MARKER;
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt};

    #[test]
    fn all_slots_stay_locked_until_drop_even_with_a_cloned_descriptor_and_poison_refuses() {
        let root = tempfile::tempdir().unwrap();
        let paths = LauncherPaths {
            state_root: root.path().into(),
            ..LauncherPaths::system()
        };
        fs::create_dir(paths.locks()).unwrap();
        for slot in 0..2 {
            let path = paths.locks().join(format!("{slot}.lock"));
            fs::write(&path, b"").unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let held = hold(&paths, 2).unwrap();
        let retained = held.0[0].try_clone().unwrap();
        assert!(matches!(
            hold(&paths, 2),
            Err(LauncherError::Occupied { slot: 0 })
        ));
        drop(held);
        drop(hold(&paths, 2).unwrap());
        drop(retained);
        fs::write(paths.locks().join("1.lock"), POISON_MARKER).unwrap();
        assert!(matches!(
            hold(&paths, 2),
            Err(LauncherError::Poisoned { slot: 1 })
        ));
        // The earlier slot was explicitly released on the partial-acquisition error.
        drop(hold(&paths, 1).unwrap());
        assert_eq!(
            fs::read(paths.locks().join("1.lock")).unwrap(),
            POISON_MARKER
        );
    }
}
