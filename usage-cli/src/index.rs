//! Explicit, source-granular refresh with deterministic replacement of facts.

use crate::{error::Result, model::digest, parse, sources, store};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;
use std::path::Path;

fn fingerprint(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut bytes = [0; 8192];
    loop {
        let count = file.read(&mut bytes)?;
        if count == 0 {
            break;
        }
        hash.update(&bytes[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub(crate) fn run(db: &Path, all: bool, explicit: &[String]) -> Result<(Value, u8)> {
    let discovered = sources::discover(all, explicit)?;
    let mut connection = store::open(db, true)?;
    // One published generation: queries continue to see the previous complete
    // snapshot while individual imports and canonical joins are prepared.
    connection.execute_batch("BEGIN IMMEDIATE")?;
    store::check_missing(&mut connection)?;
    let mut indexed = 0;
    let mut skipped = 0;
    let mut diagnostics = Vec::new();
    for source in &discovered {
        if source.state != "present" {
            store::unavailable(&mut connection, source, &source.state)?;
            diagnostics.push(json!({"source_id":source.id,"code":source.state}));
            continue;
        }
        match refresh(&mut connection, source) {
            Ok(true) => indexed += 1,
            Ok(false) => {
                connection.execute(
                    "UPDATE sources SET observed_at=datetime('now') WHERE id=?1",
                    [&source.id],
                )?;
                skipped += 1;
            }
            Err(error) => {
                store::unavailable(&mut connection, source, error.code)?;
                diagnostics.push(json!({"source_id":source.id,"code":error.code}));
            }
        }
    }
    if indexed > 0 {
        store::resolve(&connection)?;
    }
    let gaps: i64 = connection.query_row(
        "SELECT count(*) FROM sources WHERE state NOT IN ('indexed','missing')",
        [],
        |row| row.get(0),
    )?;
    let exit = if gaps > 0 { 4 } else { 0 };
    let diagnostics_total = diagnostics.len();
    diagnostics.truncate(20);
    let result = (
        json!({"schema_version":1,"generation":store::generation(&connection)?,"indexed_sources":indexed,"unchanged_sources":skipped,"discovered_sources":discovered.len(),"coverage":store::coverage(&connection)?,"diagnostics":diagnostics,"diagnostics_total":diagnostics_total,"gaps_detail":"sources --state partial; sources --state storage_error"}),
        exit,
    );
    connection.execute_batch("COMMIT")?;
    Ok(result)
}

fn refresh(connection: &mut rusqlite::Connection, source: &sources::Source) -> Result<bool> {
    let snapshot = ["louiselm", "opencode", "adapter"].contains(&source.format.as_str());
    let before = if snapshot {
        String::new()
    } else {
        fingerprint(&source.path)?
    };
    let version = if source.format == "claude" { 5 } else { 4 };
    let versioned = digest(format!("{version}:{before}").as_bytes());
    if !snapshot && store::unchanged(connection, &source.id, &versioned)? {
        return Ok(false);
    }
    let parsed = parse::read(&source.path, &source.format, &source.id)?;
    let versioned = if snapshot {
        digest(&serde_json::to_vec(&parsed)?)
    } else {
        versioned
    };
    if snapshot && store::unchanged(connection, &source.id, &versioned)? {
        return Ok(false);
    }
    if !snapshot && fingerprint(&source.path)? != before {
        return Err(crate::error::Failure {
            code: "source_changed_retry",
            message: "Source changed while being read; refresh again".into(),
            exit: 4,
            cause: None,
        });
    }
    store::replace(connection, source, &versioned, &parsed)?;
    Ok(true)
}
