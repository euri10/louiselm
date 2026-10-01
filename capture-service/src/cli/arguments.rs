//! Closed capture command grammar; parsing has no storage or service effects.

use clap::{Args, Parser, Subcommand};
use std::{net::SocketAddr, path::PathBuf};

#[derive(Parser)]
#[command(name = "louiselm-capture", version, about)]
pub(super) struct Cli {
    #[arg(long)]
    pub require_interface: Option<u32>,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub(super) enum Command {
    /// Print release and interface metadata as JSON.
    Metadata,
    /// Ingest a local audio file into the durable inbox.
    IngestLocal {
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        id: Option<String>,
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        recorded_at_ms: u64,
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        duration_ms: u64,
        #[arg(long)]
        mime: String,
    },
    /// List captures as JSON.
    List,
    /// Report capture and delivery status as JSON.
    Status,
    /// Inspect durable Attention.
    Attention {
        #[command(subcommand)]
        command: Attention,
    },
    /// Retry a capture's transcription.
    Retry { id: String },
    /// Process currently ready transcriptions once.
    TranscribeOnce,
    /// Manage durable Runs and generated-work reservations.
    Run {
        #[command(subcommand)]
        command: Run,
    },
    /// Configure one private receiver network profile.
    ConfigureNetwork {
        #[arg(long, value_parser = ["lan", "overlay", "private"])]
        profile: String,
        #[arg(long)]
        bind: SocketAddr,
        #[arg(long)]
        url: String,
    },
    /// Pair a phone, optionally writing a new private SVG file.
    Pair {
        #[arg(long)]
        svg: Option<PathBuf>,
    },
    /// Revoke a paired device.
    RevokeDevice { id: String },
    /// Retry failed notification deliveries.
    RetryNotifications,
    /// Start the configured capture service.
    Serve,
}

#[derive(Clone, Copy, Subcommand)]
pub(super) enum Attention {
    /// List unresolved items.
    List,
    /// Summarize unresolved items.
    Status,
}

#[derive(Args)]
pub(super) struct SessionArgs {
    #[arg(long)]
    pub id: String,
    #[arg(long)]
    pub session_id: String,
    #[arg(long)]
    pub agent: String,
    #[arg(long)]
    pub acp_session_id: String,
    #[arg(long)]
    pub cwd: String,
    #[arg(long, action = clap::ArgAction::Set)]
    pub load_session: bool,
}

#[derive(Subcommand)]
pub(super) enum Run {
    /// Admit a Run with an explicit generated-work ceiling.
    Admit {
        #[arg(long)]
        id: String,
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        generated_work_max: u64,
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        park_ttl_ms: u64,
    },
    /// Attach a Session to an active Run.
    Attach(SessionArgs),
    /// Generate Beads work using credentials from the environment.
    Generate {
        #[arg(long)]
        command: String,
        #[arg(last = true, required = true, num_args = 1..)]
        arguments: Vec<String>,
    },
    /// List resumable Runs.
    List,
    /// Persist a cold Park of a Run.
    Park {
        #[command(flatten)]
        session: SessionArgs,
        #[arg(long)]
        claims: String,
    },
    /// Reserve generated-work budget.
    Reserve {
        #[arg(long)]
        mutation_id: String,
        #[arg(long)]
        kind: String,
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        units: u64,
    },
    /// Confirm a generated-work reservation.
    Confirm {
        #[arg(long)]
        mutation_id: String,
        #[arg(long)]
        issue_id: String,
    },
    /// Release a generated-work reservation.
    Release {
        #[arg(long)]
        mutation_id: String,
    },
}
