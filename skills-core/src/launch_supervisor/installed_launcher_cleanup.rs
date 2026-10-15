//! Terminal receipts do not prove that the outer privileged launchers exited.

use std::{
    fs, io,
    os::unix::fs::MetadataExt,
    path::Path,
    thread,
    time::{Duration, Instant},
};

fn identity(path: &Path) -> io::Result<(u64, u64)> {
    fs::metadata(path).map(|metadata| (metadata.dev(), metadata.ino()))
}

pub(super) fn assert_exited(previous_maps: &[u32]) {
    let launcher = identity(Path::new(
        "/usr/local/lib/louiselm/current/bin/louiselm-launch",
    ))
    .expect("fixture launcher is installed");
    let sudo = identity(Path::new("/usr/bin/sudo")).expect("fixture sudo is installed");
    let state =
        identity(Path::new("/var/lib/louiselm/broker")).expect("fixture broker state is present");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let mut retained = Vec::new();
        for entry in fs::read_dir("/proc").unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name();
            let Ok(pid) = name.to_string_lossy().parse::<u32>() else {
                continue;
            };
            let comm = match fs::read_to_string(entry.path().join("comm")) {
                Ok(comm) => comm,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => panic!("fixture process name unavailable for {pid}: {error}"),
            };
            if !matches!(comm.trim_end(), "louiselm-launch" | "sudo") {
                continue;
            }
            // Each fixture has private installation and state mounts. A system
            // sudo executable alone must never select another fixture's process.
            let belongs: io::Result<bool> = (|| {
                let executable = identity(&entry.path().join("exe"))?;
                if executable != launcher && executable != sudo {
                    return Ok(false);
                }
                Ok(identity(&entry.path().join("root/var/lib/louiselm/broker"))? == state)
            })();
            match belongs {
                Ok(true) => retained.push(pid),
                Ok(false) => (),
                Err(error) if error.kind() == io::ErrorKind::NotFound => (),
                // A relevant process in a credential transition is unresolved,
                // not dead. Keep probing; never count denied inspection as exit.
                Err(error) if error.kind() == io::ErrorKind::PermissionDenied => retained.push(pid),
                Err(error) => panic!("fixture process identity unavailable for {pid}: {error}"),
            }
        }
        if retained.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "fixture returned terminal receipts with live launcher wrappers: {retained:?}"
        );
        thread::sleep(Duration::from_millis(5));
    }
    let maps = super::guard_cleanup::policy_maps()
        .into_iter()
        .filter(|id| !previous_maps.contains(id))
        .collect::<Vec<_>>();
    super::guard_cleanup::assert_released(&maps);
}
