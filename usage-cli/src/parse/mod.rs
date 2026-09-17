//! Source-specific extraction of typed facts, never execution of source text.

mod acp;
mod adapter;
mod claude;
mod codex;
mod copilot;
mod gemini;
mod louiselm;
mod opencode;

use crate::error::Result;
use crate::model::Parsed;
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

const MAX_RECORD: u64 = 32 * 1024 * 1024;

pub(crate) fn read(path: &Path, format: &str, source: &str) -> Result<Parsed> {
    if format == "louiselm" {
        return louiselm::read(path, source);
    }
    if format == "opencode" {
        return opencode::read(path, source);
    }
    let mut parsed = Parsed::default();
    let mut codex = codex::State::default();
    let mut acp = acp::State::default();
    let mut copilot = copilot::State::default();
    let mut gemini = gemini::State::default();
    let adapter = if format == "adapter" {
        adapter::session(path, &mut parsed)?
    } else {
        None
    };
    let file = File::open(path)?;
    let length = file.metadata()?.len();
    let mut reader = BufReader::new(file.take(length));
    if format == "gemini" && path.extension().is_some_and(|e| e == "json") {
        let mut bytes = Vec::new();
        reader.take(MAX_RECORD + 1).read_to_end(&mut bytes)?;
        if length > MAX_RECORD {
            parsed.note("oversize_record");
        } else if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
            gemini.consume(&value, source, 1, &mut parsed);
        } else {
            parsed.note("malformed_record");
        }
        return Ok(parsed);
    }
    let mut line = Vec::new();
    let mut number = 0;
    loop {
        line.clear();
        let read = reader
            .by_ref()
            .take(MAX_RECORD + 1)
            .read_until(b'\n', &mut line)?;
        if read == 0 {
            break;
        }
        number += 1;
        if read as u64 > MAX_RECORD {
            parsed.note("oversize_record");
            if line.last() != Some(&b'\n') {
                reader.skip_until(b'\n')?;
            }
            continue;
        }
        if line.last() != Some(&b'\n') {
            parsed.note("incomplete_tail");
            break;
        }
        let Ok(value) = serde_json::from_slice::<Value>(&line) else {
            parsed.note("malformed_record");
            continue;
        };
        match format {
            "codex" => codex.consume(&value, source, number, &mut parsed),
            "acp" => acp.consume(&value, source, number, &mut parsed),
            "claude" => claude::consume(&value, source, number, &mut parsed),
            "copilot" => copilot.consume(&value, source, number, &mut parsed),
            "gemini" => gemini.consume(&value, source, number, &mut parsed),
            "adapter" => {
                if let Some(session) = &adapter {
                    adapter::consume(&value, session, source, number, &mut parsed);
                }
            }
            _ => parsed.note("unsupported_format"),
        }
    }
    Ok(parsed)
}
