//! Command-line wiring for the trusted skill tool.
//!
//! Every command recomputes what it reports from stored bytes. Exit status is
//! part of the contract, because an unattended caller has to be able to tell
//! "reviewed and clean" from "reviewed and not admissible" without parsing
//! prose:
//!
//! * `0` — the command succeeded and what it examined is admissible.
//! * `1` — the command failed; nothing was published and nothing is claimed.
//! * `2` — the command succeeded and what it examined is **not** admissible:
//!   verification failed, or Inspection produced a fatal finding.

use std::{
    env,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use thiserror::Error;

use crate::{
    canonical::{Digest, DigestError},
    dossier::{Dossier, DossierError, DossierRequest, ReviewDepth},
    policy::{Policy, PolicyError},
    render, robot,
    store::{PublishOutcome, Store, StoreError},
};

/// Exit status for a command whose subject is not admissible.
pub const EXIT_NOT_ADMISSIBLE: i32 = 2;

/// A command that could not be completed.
#[derive(Debug, Error)]
pub enum CliError {
    /// The command line is not valid.
    #[error("{0}")]
    Invalid(String),
    /// A digest argument is malformed.
    #[error(transparent)]
    Digest(#[from] DigestError),
    /// The policy could not be loaded.
    #[error(transparent)]
    Policy(#[from] PolicyError),
    /// A store operation failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// A Dossier could not be built.
    #[error(transparent)]
    Dossier(#[from] DossierError),
    /// Robot output could not be serialized.
    #[error("cannot serialize output: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// What packaging a candidate produced.
#[derive(Debug, Serialize)]
struct PackageResult {
    schema: &'static str,
    digest: String,
    outcome: &'static str,
    entry_count: usize,
    total_bytes: u64,
    policy_digest: String,
}

/// What verifying a package produced.
#[derive(Debug, Serialize)]
struct VerifyResult {
    schema: &'static str,
    digest: String,
    recomputed_digest: String,
    intact: bool,
    failures: Vec<String>,
}

/// Runs the command named on the command line, returning its exit status.
pub fn run() -> Result<i32, CliError> {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    let Some(command) = arguments.first().map(String::as_str) else {
        print_help();
        return Ok(0);
    };
    if matches!(command, "help" | "--help" | "-h") {
        print_help();
        return Ok(0);
    }
    let options = Options::parse(&arguments[1..])?;

    match command {
        "package" => package(&options),
        "verify" => verify(&options),
        "inspect" => inspect(&options),
        "dossier" => dossier(&options),
        "list" => list(&options),
        "policy" => policy(&options),
        other => Err(CliError::Invalid(format!("unknown command '{other}'"))),
    }
}

struct Options {
    positional: Vec<String>,
    store: Option<PathBuf>,
    policy: Option<PathBuf>,
    policy_digest: Option<String>,
    against: Option<String>,
    review_depth: ReviewDepth,
    assessment_model: Option<String>,
    assessment_prompt: Option<String>,
    captured_at: Option<u64>,
    robot: bool,
    digest_only: bool,
}

impl Options {
    fn parse(arguments: &[String]) -> Result<Self, CliError> {
        let mut parsed = Self {
            positional: Vec::new(),
            store: None,
            policy: None,
            policy_digest: None,
            against: None,
            review_depth: ReviewDepth::Unstated,
            assessment_model: None,
            assessment_prompt: None,
            captured_at: None,
            robot: false,
            digest_only: false,
        };
        let mut index = 0;
        while index < arguments.len() {
            let argument = arguments[index].as_str();
            let value = |name: &str| -> Result<String, CliError> {
                arguments
                    .get(index + 1)
                    .cloned()
                    .ok_or_else(|| CliError::Invalid(format!("{name} needs a value")))
            };
            match argument {
                "--robot-json" => parsed.robot = true,
                "--digest" => parsed.digest_only = true,
                "--store" => {
                    parsed.store = Some(PathBuf::from(value("--store")?));
                    index += 1;
                }
                "--policy" => {
                    parsed.policy = Some(PathBuf::from(value("--policy")?));
                    index += 1;
                }
                "--policy-digest" => {
                    parsed.policy_digest = Some(value("--policy-digest")?);
                    index += 1;
                }
                "--against" => {
                    parsed.against = Some(value("--against")?);
                    index += 1;
                }
                "--review-depth" => {
                    let raw = value("--review-depth")?;
                    parsed.review_depth = ReviewDepth::parse(&raw).ok_or_else(|| {
                        CliError::Invalid(format!(
                            "--review-depth must be unstated, skimmed, read, or reproduced, not '{raw}'"
                        ))
                    })?;
                    index += 1;
                }
                "--assessment-model" => {
                    parsed.assessment_model = Some(value("--assessment-model")?);
                    index += 1;
                }
                "--assessment-prompt" => {
                    parsed.assessment_prompt = Some(value("--assessment-prompt")?);
                    index += 1;
                }
                "--captured-at" => {
                    let raw = value("--captured-at")?;
                    parsed.captured_at = Some(raw.parse().map_err(|_| {
                        CliError::Invalid(format!(
                            "--captured-at must be milliseconds, not '{raw}'"
                        ))
                    })?);
                    index += 1;
                }
                other if other.starts_with('-') => {
                    return Err(CliError::Invalid(format!("unknown option '{other}'")));
                }
                other => parsed.positional.push(other.to_owned()),
            }
            index += 1;
        }
        Ok(parsed)
    }

    fn subject(&self, command: &str) -> Result<&str, CliError> {
        self.positional
            .first()
            .map(String::as_str)
            .ok_or_else(|| CliError::Invalid(format!("{command} needs an argument")))
    }

    fn policy(&self) -> Result<Policy, CliError> {
        match (&self.policy, &self.policy_digest) {
            (None, _) => Ok(Policy::embedded()),
            (Some(_), None) => Err(CliError::Invalid(
                "--policy requires --policy-digest: a policy accepted because it parsed is a policy an attacker may rewrite".to_owned(),
            )),
            (Some(path), Some(digest)) => {
                Ok(Policy::load(path, &Digest::parse(digest)?)?)
            }
        }
    }

    fn store(&self) -> Result<Store, CliError> {
        let root = match &self.store {
            Some(path) => path.clone(),
            None => default_store_root()?,
        };
        Ok(Store::open(&root)?)
    }

    fn dossier_request<'a>(
        &'a self,
        digest: &'a Digest,
        base: Option<&'a Digest>,
    ) -> DossierRequest<'a> {
        let mut request = DossierRequest::new(digest).with_review_depth(self.review_depth);
        if let Some(base) = base {
            request = request.against(base);
        }
        if let (Some(model), Some(prompt)) = (&self.assessment_model, &self.assessment_prompt) {
            request = request.with_assessment_key(model, prompt);
        }
        request
    }
}

fn package(options: &Options) -> Result<i32, CliError> {
    let source = PathBuf::from(options.subject("package")?);
    let policy = options.policy()?;
    let store = options.store()?;
    let captured_at = match options.captured_at {
        Some(value) => value,
        None => now_ms(),
    };
    let (package, outcome) = store.capture(&source, &policy, captured_at)?;
    let result = PackageResult {
        schema: "louiselm.skills.package-result/1",
        digest: package.digest.to_string(),
        outcome: match outcome {
            PublishOutcome::Created => "created",
            PublishOutcome::Existing => "existing",
        },
        entry_count: package.manifest.entries.len(),
        total_bytes: package.manifest.total_size(),
        policy_digest: policy.digest().to_string(),
    };
    if options.robot {
        println!("{}", robot::payload(&result)?);
    } else {
        println!(
            "{} {} ({} file(s), {} byte(s))",
            result.outcome, result.digest, result.entry_count, result.total_bytes,
        );
    }
    Ok(0)
}

fn verify(options: &Options) -> Result<i32, CliError> {
    let digest = Digest::parse(options.subject("verify")?)?;
    let policy = options.policy()?;
    let report = options.store()?.verify(&digest, &policy)?;
    let result = VerifyResult {
        schema: "louiselm.skills.verify-result/1",
        digest: report.digest.to_string(),
        recomputed_digest: report.recomputed_digest.to_string(),
        intact: report.is_intact(),
        failures: report
            .failures
            .iter()
            .map(|failure| crate::scan::escape(&failure.summary()))
            .collect(),
    };
    if options.robot {
        println!("{}", robot::payload(&result)?);
    } else if result.intact {
        println!("{} verified against its stored bytes", result.digest);
    } else {
        println!("{} FAILED verification", result.digest);
        for failure in &result.failures {
            println!("  - {failure}");
        }
    }
    Ok(if result.intact {
        0
    } else {
        EXIT_NOT_ADMISSIBLE
    })
}

fn inspect(options: &Options) -> Result<i32, CliError> {
    let digest = Digest::parse(options.subject("inspect")?)?;
    let policy = options.policy()?;
    let store = options.store()?;
    let package = store.open_package(&digest, &policy)?;
    let inspection = crate::inspect::Inspection::run(&package, &policy)?;
    if options.robot {
        println!("{}", robot::payload(&inspection)?);
    } else {
        for (kind, count) in inspection.counts_by_kind() {
            println!("{kind:<22} {count}");
        }
        for fatal in &inspection.fatal {
            println!("fatal: {}", fatal.message);
        }
    }
    Ok(if inspection.is_fatal() {
        EXIT_NOT_ADMISSIBLE
    } else {
        0
    })
}

fn dossier(options: &Options) -> Result<i32, CliError> {
    let digest = Digest::parse(options.subject("dossier")?)?;
    let base = options.against.as_deref().map(Digest::parse).transpose()?;
    let policy = options.policy()?;
    let store = options.store()?;
    let dossier = Dossier::build(
        &store,
        &policy,
        &options.dossier_request(&digest, base.as_ref()),
    )?;
    if options.robot {
        println!("{}", robot::json(&dossier)?);
    } else {
        print!("{}", render::human(&dossier));
    }
    Ok(if dossier.reviewable() {
        0
    } else {
        EXIT_NOT_ADMISSIBLE
    })
}

fn list(options: &Options) -> Result<i32, CliError> {
    let store = options.store()?;
    let digests = store.list()?;
    if options.robot {
        let rendered = digests.iter().map(Digest::to_string).collect::<Vec<_>>();
        println!("{}", robot::payload(&rendered)?);
    } else {
        for digest in &digests {
            println!("{digest}");
        }
    }
    Ok(0)
}

fn policy(options: &Options) -> Result<i32, CliError> {
    let policy = options.policy()?;
    if options.digest_only {
        println!("{}", policy.digest());
    } else if options.policy.is_some() {
        println!("{}", robot::payload(policy.document())?);
    } else {
        println!(
            "{}",
            String::from_utf8_lossy(Policy::embedded_bytes()).trim_end()
        );
    }
    Ok(0)
}

fn default_store_root() -> Result<PathBuf, CliError> {
    if let Ok(explicit) = env::var("LOUISELM_SKILLS_STORE") {
        return Ok(PathBuf::from(explicit));
    }
    if let Ok(state) = env::var("XDG_STATE_HOME") {
        return Ok(Path::new(&state).join("louiselm/skills"));
    }
    let home = env::var("HOME").map_err(|_| {
        CliError::Invalid(
            "no store location: set LOUISELM_SKILLS_STORE, XDG_STATE_HOME, or HOME".to_owned(),
        )
    })?;
    Ok(Path::new(&home).join(".local/state/louiselm/skills"))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or_default()
}

fn print_help() {
    println!(
        "louiselm-skills — package Skill candidates and render canonical Dossiers

Usage:
  louiselm-skills package <candidate-dir> [--captured-at <ms>]
  louiselm-skills verify <digest>
  louiselm-skills inspect <digest>
  louiselm-skills dossier <digest> [--against <digest>] [--review-depth <depth>]
                                   [--assessment-model <m> --assessment-prompt <p>]
  louiselm-skills list
  louiselm-skills policy [--digest]

Options:
  --store <dir>          Store root; defaults to $LOUISELM_SKILLS_STORE, then
                         $XDG_STATE_HOME/louiselm/skills, then ~/.local/state/louiselm/skills.
  --policy <file>        Replacement Inspection policy. Requires --policy-digest.
  --policy-digest <d>    The digest the replacement policy must have.
  --review-depth <d>     unstated | skimmed | read | reproduced. A recorded claim, not a proof.
  --robot-json           Emit the machine-readable view instead of the human one.

Exit status:
  0  succeeded; the subject is admissible
  1  failed; nothing was published and nothing is claimed
  2  succeeded; the subject is NOT admissible (verification failed or a fatal finding)"
    );
}
