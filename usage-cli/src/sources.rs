//! Allowlisted source discovery; never recursively searches the user's home.

use crate::error::{Failure, Result};
use crate::model::digest;
use serde::Serialize;
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) const FORMATS: &[&str] = &[
    "codex", "claude", "opencode", "copilot", "gemini", "acp", "adapter", "louiselm",
];

#[derive(Clone, Serialize)]
pub(crate) struct Source {
    pub id: String,
    pub format: String,
    pub path: PathBuf,
    pub state: String,
}

pub(crate) fn state_root() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .map_or_else(|| home().map(|p| p.join(".local/state")), Ok)?;
    Ok(base.join("louiselm/usage-analysis"))
}

fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| Failure::storage("HOME is unset; supply explicit --db and --source paths"))
}

pub(crate) fn roots() -> Result<Vec<(String, PathBuf)>> {
    let home = home()?;
    let state = std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map_or_else(|| home.join(".local/state"), PathBuf::from);
    let data = std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map_or_else(|| home.join(".local/share"), PathBuf::from);
    let codex = std::env::var_os("CODEX_HOME")
        .filter(|v| !v.is_empty())
        .map_or_else(|| home.join(".codex"), PathBuf::from);
    let claude = std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|v| !v.is_empty())
        .map_or_else(|| home.join(".claude"), PathBuf::from);
    Ok(vec![
        ("codex".into(), codex.join("sessions")),
        ("codex".into(), codex.join("archived_sessions")),
        ("claude".into(), claude.join("projects")),
        ("opencode".into(), data.join("opencode/opencode.db")),
        ("copilot".into(), home.join(".copilot/session-state")),
        ("gemini".into(), home.join(".gemini/tmp")),
        ("adapter".into(), state.join("acp-llm-adapter/sessions")),
        ("acp".into(), state.join("acp-llm-adapter/proxy/sessions")),
        (
            "acp".into(),
            state.join("acp-llm-adapter/proxy/connections"),
        ),
        ("acp".into(), state.join("acp-llm-adapter/connections")),
        (
            "louiselm".into(),
            state.join("louiselm/usage/turns.sqlite3"),
        ),
    ])
}

pub(crate) fn discover(all: bool, explicit: &[String]) -> Result<Vec<Source>> {
    let mut roots = if all { roots()? } else { Vec::new() };
    for item in explicit {
        let (format, path) = item
            .split_once('=')
            .ok_or_else(|| Failure::query("Source must be FORMAT=PATH; see schema"))?;
        if !FORMATS.contains(&format) {
            return Err(Failure::query("Unsupported source format; see schema"));
        }
        roots.push((format.to_owned(), PathBuf::from(path)));
    }
    let mut found = Vec::new();
    for (format, path) in roots {
        match fs::canonicalize(&path) {
            Ok(path) => walk(&format, &path, &mut found, 0),
            Err(error) => found.push(source(
                &format,
                path,
                if error.kind() == std::io::ErrorKind::NotFound {
                    "missing"
                } else {
                    "inaccessible"
                },
            )),
        }
    }
    found.sort_by(|a, b| a.id.cmp(&b.id));
    found.dedup_by(|a, b| a.id == b.id);
    Ok(found)
}

pub(crate) fn report(options: &crate::cli::SourceQuery) -> Result<Value> {
    let discovered = discover(true, &[])?;
    let signature = digest(&serde_json::to_vec(&json!([
        discovered,
        options.state,
        options.adapter
    ]))?);
    let offset = if let Some(cursor) = &options.cursor {
        let (signature_given, offset) = cursor
            .split_once(':')
            .ok_or_else(|| Failure::query("Invalid discovery cursor"))?;
        if signature_given != signature {
            return Err(Failure::query(
                "Discovery changed; restart sources --discover",
            ));
        }
        offset
            .parse::<usize>()
            .map_err(|_| Failure::query("Invalid cursor offset"))?
    } else {
        0
    };
    let rows: Vec<_> = discovered
        .iter()
        .filter(|s| {
            options.state.as_ref().is_none_or(|v| *v == s.state)
                && options.adapter.as_ref().is_none_or(|v| *v == s.format)
        })
        .collect();
    let total = rows.len();
    let mut result: Vec<_> = rows
        .into_iter()
        .skip(offset)
        .take(options.limit as usize)
        .collect();
    loop {
        let next = (offset + result.len() < total)
            .then(|| format!("{signature}:{}", offset + result.len()));
        let output = json!({"schema_version":1,"rows":result,"discovered_sources":discovered.len(),"matching_sources":total,"next_cursor":next,"scope":{"mode":"discovery only; refresh explicitly with index","state":options.state,"adapter":options.adapter}});
        if serde_json::to_vec(&output)?.len() <= 32768 {
            return Ok(output);
        }
        if result.len() <= 1 {
            return Err(Failure::query("Source path exceeds response budget"));
        }
        result.pop();
    }
}

fn source(format: &str, path: PathBuf, state: &str) -> Source {
    let id = digest(format!("{format}:{}", path.display()).as_bytes());
    Source {
        id,
        format: format.to_owned(),
        path,
        state: state.to_owned(),
    }
}

fn walk(format: &str, path: &Path, found: &mut Vec<Source>, depth: u32) {
    if depth > 32 {
        found.push(source(format, path.into(), "depth_limit"));
        return;
    }
    let Ok(meta) = fs::symlink_metadata(path) else {
        found.push(source(format, path.into(), "inaccessible"));
        return;
    };
    if meta.file_type().is_symlink() {
        found.push(source(format, path.into(), "symlink_skipped"));
        return;
    }
    if meta.is_dir() {
        let Ok(entries) = fs::read_dir(path) else {
            found.push(source(format, path.into(), "inaccessible"));
            return;
        };
        for entry in entries {
            match entry {
                Ok(entry) => walk(format, &entry.path(), found, depth + 1),
                Err(_) => found.push(source(format, path.into(), "inaccessible")),
            }
        }
    } else if meta.is_file() && (depth == 0 || recognized(format, path)) {
        use std::os::unix::fs::MetadataExt;
        let state = if meta.uid() == rustix::process::getuid().as_raw() {
            "present"
        } else {
            "unowned"
        };
        found.push(source(format, path.to_path_buf(), state));
    }
}

fn recognized(format: &str, path: &Path) -> bool {
    let name = path.file_name().and_then(|p| p.to_str()).unwrap_or("");
    let extension = path.extension().and_then(|p| p.to_str()).unwrap_or("");
    match format {
        "codex" | "claude" | "acp" => extension == "jsonl",
        "adapter" => name == "history.jsonl",
        "copilot" => name == "events.jsonl",
        "gemini" => {
            path.components().any(|part| part.as_os_str() == "chats")
                && ["json", "jsonl"].contains(&extension)
                || name.starts_with("session-") && ["json", "jsonl"].contains(&extension)
        }
        "opencode" => extension == "db",
        "louiselm" => extension == "sqlite3",
        _ => false,
    }
}
