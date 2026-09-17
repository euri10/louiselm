//! Private derived storage; sources are imported transactionally and stay read-only.

use crate::error::{Failure, Result};
use crate::model::Parsed;
use crate::sources::Source;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde_json::{Value, json};
use std::fs::{self, DirBuilder, OpenOptions};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::time::Duration;

const SCHEMA: &str = include_str!("schema.sql");

pub(crate) fn open(path: &Path, write: bool) -> Result<Connection> {
    if !write && !path.exists() {
        return Err(Failure {
            code: "index_missing",
            message: "Run louiselm-usage index --all first (or provide --source FORMAT=PATH)"
                .into(),
            exit: 3,
            cause: None,
        });
    }
    let new = !path.exists();
    if write && new {
        let parent = path
            .parent()
            .ok_or_else(|| Failure::query("Index requires a parent directory"))?;
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
        check_private(parent, true)?;
        OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(path)?;
    }
    check_private(path, false)?;
    if let Some(parent) = path.parent() {
        check_private(parent, true)?;
    }
    let flags = if write {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    } else {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    };
    let connection = Connection::open_with_flags(path, flags)?;
    connection.busy_timeout(Duration::from_secs(1))?;
    connection.execute_batch("PRAGMA foreign_keys=ON;")?;
    if new {
        connection.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=EXTRA;")?;
        connection.execute_batch(SCHEMA)?;
    }
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let application: i64 = connection.query_row("PRAGMA application_id", [], |row| row.get(0))?;
    if version != 3 || application != 1_280_136_533 {
        return Err(Failure::storage(
            "Unsupported index schema; use a new --db path to rebuild",
        ));
    }
    Ok(connection)
}

fn check_private(path: &Path, directory: bool) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.mode() & 0o077 != 0
        || (directory && !metadata.is_dir())
        || (!directory && (!metadata.is_file() || metadata.nlink() != 1))
    {
        return Err(Failure::storage(
            "Index must be an owned private regular file in a private directory (0700/0600)",
        ));
    }
    Ok(())
}

pub(crate) fn generation(connection: &Connection) -> Result<i64> {
    Ok(
        connection.query_row("SELECT value FROM meta WHERE key='generation'", [], |r| {
            r.get(0)
        })?,
    )
}

pub(crate) fn unchanged(connection: &Connection, source: &str, digest: &str) -> Result<bool> {
    Ok(connection
        .query_row(
            "SELECT digest=?2 AND state IN ('indexed','partial') FROM sources WHERE id=?1",
            params![source, digest],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(false))
}

pub(crate) fn unavailable(connection: &mut Connection, source: &Source, code: &str) -> Result<()> {
    let previous: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sources WHERE id=?1 AND digest!='')",
        [&source.id],
        |r| r.get(0),
    )?;
    let code = if code == "missing" && previous {
        "missing_retained_snapshot"
    } else {
        code
    };
    let same: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sources WHERE id=?1 AND state=?2)",
        params![source.id, code],
        |r| r.get(0),
    )?;
    if same {
        return Ok(());
    }
    let transaction = connection.savepoint()?;
    transaction.execute("INSERT INTO sources VALUES(?1,?2,?3,'',?4,?5,datetime('now')) ON CONFLICT(id) DO UPDATE SET state=excluded.state,diagnostics=excluded.diagnostics,observed_at=excluded.observed_at",
        params![source.id,source.format,source.path.to_string_lossy(),code,json!({code:1}).to_string()])?;
    transaction.execute("UPDATE meta SET value=value+1 WHERE key='generation'", [])?;
    transaction.commit()?;
    Ok(())
}

pub(crate) fn check_missing(connection: &mut Connection) -> Result<()> {
    let sources = connection
        .prepare("SELECT id,format,path FROM sources WHERE state IN ('indexed','partial')")?
        .query_map([], |row| {
            Ok(Source {
                id: row.get(0)?,
                format: row.get(1)?,
                path: row.get::<_, String>(2)?.into(),
                state: "missing".into(),
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for source in sources {
        if !source.path.exists() {
            unavailable(connection, &source, "missing_retained_snapshot")?;
        }
    }
    Ok(())
}

pub(crate) fn replace(
    connection: &mut Connection,
    source: &Source,
    digest: &str,
    facts: &Parsed,
) -> Result<()> {
    let transaction = connection.savepoint()?;
    let state = if facts.diagnostics.is_empty() {
        "indexed"
    } else {
        "partial"
    };
    transaction.execute("INSERT INTO sources VALUES(?1,?2,?3,?4,?5,?6,datetime('now')) ON CONFLICT(id) DO UPDATE SET digest=excluded.digest,state=excluded.state,diagnostics=excluded.diagnostics,observed_at=excluded.observed_at",
        params![source.id,source.format,source.path.to_string_lossy(),digest,state,serde_json::to_string(&facts.diagnostics)?])?;
    transaction.execute("DELETE FROM calls WHERE source_id=?1", [&source.id])?;
    transaction.execute("DELETE FROM sessions WHERE source_id=?1", [&source.id])?;
    transaction.execute("DELETE FROM turns WHERE source_id=?1", [&source.id])?;
    transaction.execute("DELETE FROM requests WHERE source_id=?1", [&source.id])?;
    for session in facts.sessions.values() {
        transaction.execute(
            "INSERT INTO sessions VALUES(?1,?2,?3)",
            params![source.id, session.id, serde_json::to_string(session)?],
        )?;
    }
    for call in facts.calls.values() {
        transaction.execute(
            "INSERT INTO calls VALUES(?1,?2,?3)",
            params![source.id, call.id, serde_json::to_string(call)?],
        )?;
    }
    for turn in facts.turns.values() {
        transaction.execute(
            "INSERT INTO turns VALUES(?1,?2,?3)",
            params![source.id, turn.id, serde_json::to_string(turn)?],
        )?;
    }
    for request in facts.requests.values() {
        transaction.execute(
            "INSERT INTO requests VALUES(?1,?2,?3)",
            params![source.id, request.id, serde_json::to_string(request)?],
        )?;
    }
    transaction.execute("UPDATE meta SET value=value+1 WHERE key='generation'", [])?;
    transaction.commit()?;
    Ok(())
}

pub(crate) fn coverage(connection: &Connection) -> Result<Value> {
    let counts: (i64, i64, Option<String>, Option<String>) = connection.query_row(
        "SELECT count(*),coalesce(sum(state!='indexed'),0),min(observed_at),max(observed_at) FROM sources",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    let calls: (i64,i64,i64) = connection.query_row("SELECT count(*),coalesce(sum(json_extract(data,'$.overlap')='possible_mirror'),0),coalesce(sum(json_extract(data,'$.conflicting_observations')),0) FROM call_facts",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    Ok(
        json!({"scope":"whole index; per-group measurement denominators accompany rows","known_sources":counts.0,"partial_or_unavailable_sources":counts.1,"oldest_observation":counts.2,"newest_observation":counts.3,"canonical_calls":calls.0,"possible_mirror_calls":calls.1,"conflicting_calls":calls.2,"measurement_boundary":"retained text; delivery and billing are not inferred","refresh":"explicit index; no source reads during queries; unavailable sources retain their last snapshot"}),
    )
}

pub(crate) fn resolve(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "DELETE FROM call_facts; INSERT INTO call_facts SELECT * FROM derived_calls;",
    )?;
    Ok(())
}
