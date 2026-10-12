//! Broker systemd units shipped inside the signed release.
//!
//! The broker's descriptor handover is a contract between `louiselm-control`
//! and its units, so both come from the same release. Refreshing the launcher
//! after a release install therefore also refreshes the units; a hand-installed
//! older unit cannot silently start a newer broker (louiselm-v3gl4). The
//! operator reloads systemd and restarts the broker explicitly, as before; this
//! module never touches running services or site drop-ins.

use super::{LauncherError, LauncherFailure, LauncherPaths, failure, io_error, write_atomic};
use std::{fs, io};

const UNITS: [(&str, &str); 2] = [
    (
        "louiselm-broker.socket",
        include_str!("../../contrib/systemd/louiselm-broker.socket"),
    ),
    (
        "louiselm-broker.service",
        include_str!("../../contrib/systemd/louiselm-broker.service"),
    ),
];

fn installed(paths: &LauncherPaths, name: &str) -> io::Result<Option<Vec<u8>>> {
    match fs::read(paths.systemd_units.join(name)) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Atomically writes every unit whose bytes differ from this release.
pub(super) fn install(paths: &LauncherPaths) -> Result<(), LauncherError> {
    for (name, bytes) in UNITS {
        let current = installed(paths, name).map_err(|source| io_error("broker unit", source))?;
        if current.as_deref() != Some(bytes.as_bytes()) {
            write_atomic(&paths.systemd_units.join(name), bytes.as_bytes(), 0o644)?;
        }
    }
    Ok(())
}

/// Reports units that are missing or differ from the current release.
pub(super) fn check(paths: &LauncherPaths, failures: &mut Vec<LauncherFailure>) {
    for (name, bytes) in UNITS {
        match installed(paths, name) {
            Ok(Some(current)) if current == bytes.as_bytes() => {}
            Ok(_) => failures.push(failure(
                "broker_units_stale",
                format!("{name} is missing or differs from the installed release."),
                "Rerun the root launcher installer from the current release, then run systemctl daemon-reload and restart louiselm-broker.service.",
            )),
            Err(error) => failures.push(failure(
                "broker_units_unreadable",
                format!("{name}: {error}"),
                "Restore readable root-owned broker units, then rerun the root launcher installer.",
            )),
        }
    }
}
