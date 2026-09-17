//! Fixed disposable-tracker file probes inside the measured Agent namespace.

use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    path::PathBuf,
};

pub(super) fn inspect(output: &mut impl Write) -> io::Result<()> {
    let canonical = PathBuf::from("/var/lib/louiselm/beads-project/.beads");
    let replica = std::env::var_os("BEADS_DIR").map(PathBuf::from);
    let paths = ["beads.db", "issues.jsonl"];
    let report = serde_json::json!({
        "canonical_read": paths.map(|name| File::open(canonical.join(name)).is_ok()),
        "canonical_write": paths.map(|name| OpenOptions::new().write(true).open(canonical.join(name)).is_ok()),
        "replica_read": paths.map(|name| replica.as_ref().is_some_and(|root| File::open(root.join(name)).is_ok())),
    });
    output.write_all(&[0x1c])?;
    serde_json::to_writer(&mut *output, &report).map_err(io::Error::other)?;
    output.write_all(b"\n")?;
    output.flush()
}
