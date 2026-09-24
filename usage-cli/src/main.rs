//! Local, evidence-linked analysis of agent session histories.

mod cli;
mod command;
mod error;
mod index;
mod model;
mod parse;
mod query;
mod sources;
mod store;

use clap::Parser;
use clap::error::{ContextKind, ContextValue};
use cli::{Cli, Command};
use error::{Failure, Result};
use serde_json::Value;
use std::io::{self, Write};
use std::process::ExitCode;

fn execute(cli: Cli) -> Result<(Value, u8)> {
    if let Command::Schema {
        command, subject, ..
    } = &cli.command
        && command.as_deref() != Some("options")
    {
        return Ok((query::schema(command.as_deref(), subject.as_deref())?, 0));
    }
    let db = cli.db.map_or_else(
        || sources::state_root().map(|p| p.join("index.sqlite3")),
        Ok,
    )?;
    if let Command::Sources(options) = &cli.command
        && (options.discover || !db.exists())
    {
        return Ok((sources::report(options)?, 0));
    }
    if let Command::Index { all, source } = cli.command {
        return index::run(&db, all, &source);
    }
    let connection = store::open(&db, false)?;
    connection.execute_batch("BEGIN")?;
    let value = match cli.command {
        Command::Stats {
            subject,
            query,
            group_by,
        } => query::stats(&connection, subject, &query, group_by.as_deref())?,
        Command::Calls(query) => query::calls(&connection, &query)?,
        Command::Show {
            kind,
            id,
            fields,
            limit,
            cursor,
        } => query::show(
            &connection,
            &kind,
            &id,
            fields.as_deref(),
            limit,
            cursor.as_deref(),
        )?,
        Command::Sources(options) => query::sources(&connection, &options)?,
        Command::Schema {
            subject,
            limit,
            cursor,
            ..
        } => query::options(&connection, subject.as_deref(), limit, cursor)?,
        Command::Index { .. } => {
            return Err(Failure::query("Invalid command state"));
        }
    };
    connection.execute_batch("COMMIT")?;
    Ok((value, 0))
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error)
            if matches!(
                error.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) =>
        {
            return if error.print().is_ok() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(3)
            };
        }
        Err(error) => return fail(&Failure::query(argument_error(&error))),
    };
    let table = cli.format == "table";
    match execute(cli) {
        Ok((value, exit)) => {
            let bytes = match serde_json::to_vec(&value) {
                Ok(bytes) => bytes,
                Err(error) => return fail(&error.into()),
            };
            if bytes.len() > 32768 {
                return fail(&Failure::query(
                    "Response exceeds 32 KiB; select fewer fields or rows",
                ));
            }
            let bytes = if table { render_table(&value) } else { bytes };
            let mut output = io::stdout().lock();
            if let Err(error) = output
                .write_all(&bytes)
                .and_then(|()| output.write_all(b"\n"))
                && error.kind() != io::ErrorKind::BrokenPipe
            {
                return fail(&error.into());
            }
            ExitCode::from(exit)
        }
        Err(error) => fail(&error),
    }
}

/// Names the failing argument from clap's context, never the supplied value.
fn argument_error(error: &clap::Error) -> String {
    let kind = error
        .kind()
        .as_str()
        .unwrap_or("Invalid command or argument");
    match error.get(ContextKind::InvalidArg) {
        Some(ContextValue::String(arg)) => format!("{kind}: {arg}; use --help"),
        Some(ContextValue::Strings(args)) => format!("{kind}: {}; use --help", args.join("|")),
        _ => format!("{kind}; use --help"),
    }
}

fn render_table(value: &Value) -> Vec<u8> {
    let Some(rows) = value["rows"].as_array() else {
        return value.to_string().into_bytes();
    };
    let Some(first) = rows.first().and_then(Value::as_object) else {
        return b"(no rows)".to_vec();
    };
    let keys: Vec<_> = first.keys().map(String::as_str).collect();
    let mut lines = vec![keys.join("\t")];
    lines.extend(rows.iter().map(|row| {
        keys.iter()
            .map(|key| row[*key].to_string())
            .collect::<Vec<_>>()
            .join("\t")
    }));
    lines.push(format!(
        "coverage={} next_cursor={}",
        value["coverage"], value["next_cursor"]
    ));
    lines.join("\n").into_bytes()
}

fn fail(error: &Failure) -> ExitCode {
    eprintln!("{}", error.json());
    ExitCode::from(error.exit)
}
