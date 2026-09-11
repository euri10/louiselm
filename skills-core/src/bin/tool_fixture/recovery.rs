//! Synthetic counter checkpoint protocol, deliberately not vendor ACP.
//! GS + `save ID` or `load ID` + newline returns GS + counter + newline.

use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::{self, BufRead, Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Checkpoint {
    schema: String,
    acp_session_id: String,
    counter: u64,
}

fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits().cast_signed())
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn read(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(
            (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK)
                .bits()
                .cast_signed(),
        )
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("fixture recovery requires regular files"));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(io::Error::other("oversized fixture checkpoint"));
    }
    Ok(bytes)
}

pub(super) fn handle(
    input: &mut impl BufRead,
    output: &mut impl Write,
    counter: &mut u64,
) -> io::Result<()> {
    let mut bytes = Vec::new();
    input.take(257).read_until(b'\n', &mut bytes)?;
    if bytes.len() > 256 || bytes.last() != Some(&b'\n') {
        return Err(io::Error::other("invalid fixture recovery frame"));
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| io::Error::other("invalid fixture recovery text"))?;
    let (action, id) = text
        .trim_end_matches('\n')
        .split_once(' ')
        .ok_or_else(|| io::Error::other("invalid fixture recovery operation"))?;
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err(io::Error::other("invalid fixture ACP identity"));
    }
    let cwd = std::env::current_dir()?;
    let home = cwd
        .parent()
        .ok_or_else(|| io::Error::other("missing fixture Session root"))?
        .join("home");
    let checkpoint_path = home.join("recovery.json");
    let workspace_path = cwd.join("recovery-counter.json");
    match action {
        "save" => {
            let checkpoint = Checkpoint {
                schema: "louiselm.test-recovery/1".into(),
                acp_session_id: id.into(),
                counter: *counter,
            };
            write(&workspace_path, counter.to_string().as_bytes())?;
            write(
                &checkpoint_path,
                &serde_json::to_vec(&checkpoint).map_err(io::Error::other)?,
            )?;
            File::open(&cwd)?.sync_all()?;
            File::open(&home)?.sync_all()?;
        }
        "load" => {
            let bytes = read(&checkpoint_path, 16 * 1024)?;
            let saved: Checkpoint = serde_json::from_slice(&bytes)
                .map_err(|_| io::Error::other("invalid fixture checkpoint"))?;
            if saved.schema != "louiselm.test-recovery/1"
                || saved.acp_session_id != id
                || read(&workspace_path, 20)? != saved.counter.to_string().as_bytes()
            {
                return Err(io::Error::other("fixture recovery mismatch"));
            }
            *counter = saved.counter;
        }
        _ => return Err(io::Error::other("unsupported fixture recovery operation")),
    }
    writeln!(output, "\x1d{counter}")?;
    output.flush()
}
