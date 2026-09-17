//! Discoverable noninteractive CLI contract.

use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    version,
    about = "Query local agent usage without loading transcripts into model context"
)]
pub(crate) struct Cli {
    /// Private derived `SQLite` index; source stores are never modified.
    #[arg(long, global = true)]
    pub db: Option<PathBuf>,
    /// JSON is the agent contract; table is for interactive inspection.
    #[arg(long, global=true, default_value="json", value_parser=["json","table"])]
    pub format: String,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Discover known history stores and inspect indexing coverage.
    Sources(SourceQuery),
    /// Explicitly refresh recognized sources; no source history is changed.
    Index {
        /// Discover all recognized roots for the current user.
        #[arg(long, required_unless_present = "source")]
        all: bool,
        /// Additional local source in FORMAT=PATH form (repeatable).
        #[arg(long, required_unless_present = "all")]
        source: Vec<String>,
    },
    /// Rank tools or commands, with explicit coverage per measurement.
    Stats {
        #[arg(value_enum)]
        subject: Subject,
        #[command(flatten)]
        query: Query,
        /// Comma-separated dimensions (discover with schema).
        #[arg(long)]
        group_by: Option<String>,
    },
    /// List observed calls/executions and their evidence references.
    Calls(Query),
    /// Inspect one call, session, or turn by its stable ID.
    Show {
        kind: String,
        id: String,
        #[arg(long)]
        fields: Option<String>,
        #[arg(long, default_value_t=20, value_parser=clap::value_parser!(u32).range(1..=1000))]
        limit: u32,
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Discover query fields, metrics, semantics, and supported source formats.
    Schema {
        command: Option<String>,
        #[arg(long, default_value_t=20, value_parser=clap::value_parser!(u32).range(1..=1000))]
        limit: u32,
        #[arg(long)]
        cursor: Option<String>,
    },
}

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum Subject {
    Tools,
    Commands,
    Turns,
    Sessions,
    Requests,
}

#[derive(Args, Default, Serialize)]
pub(crate) struct Query {
    #[arg(long)]
    pub project: Option<String>,
    #[arg(long)]
    pub project_tree: Option<String>,
    #[arg(long)]
    pub session: Vec<String>,
    #[arg(long)]
    pub adapter: Vec<String>,
    #[arg(long)]
    pub agent: Vec<String>,
    #[arg(long)]
    pub provider: Vec<String>,
    #[arg(long)]
    pub model: Vec<String>,
    #[arg(long)]
    pub source_id: Vec<String>,
    #[arg(long)]
    pub parent_session: Option<String>,
    #[arg(long, default_value="all", value_parser=["all","root","child"])]
    pub children: String,
    #[arg(long = "option")]
    pub options: Vec<String>,
    #[arg(long)]
    pub command_key: Option<String>,
    #[arg(long)]
    pub family: Option<String>,
    #[arg(long)]
    pub tool: Option<String>,
    #[arg(long)]
    pub signature: Option<String>,
    #[arg(long)]
    pub wrapper: Option<String>,
    #[arg(long)]
    pub min_output_bytes: Option<u64>,
    #[arg(long)]
    pub min_duration_ms: Option<u64>,
    #[arg(long, default_value="exclude", value_parser=["exclude","include"])]
    pub overlap: String,
    #[arg(long)]
    pub status: Option<String>,
    #[arg(long)]
    pub since: Option<String>,
    #[arg(long)]
    pub until: Option<String>,
    #[arg(long, default_value = "all", value_parser = ["all", "fixed"])]
    pub cohort: String,
    #[arg(long, default_value = "leaf", value_parser = ["all", "leaf", "orchestrator"])]
    pub level: String,
    #[arg(long)]
    pub sort: Option<String>,
    #[arg(long)]
    pub fields: Option<String>,
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=1000))]
    pub limit: u32,
    #[arg(long)]
    pub cursor: Option<String>,
}

#[derive(Args)]
pub(crate) struct SourceQuery {
    /// Discover known roots without creating or refreshing an index.
    #[arg(long)]
    pub discover: bool,
    #[arg(long)]
    pub state: Option<String>,
    #[arg(long)]
    pub adapter: Option<String>,
    #[arg(long, default_value_t=20, value_parser=clap::value_parser!(u32).range(1..=1000))]
    pub limit: u32,
    #[arg(long)]
    pub cursor: Option<String>,
}
