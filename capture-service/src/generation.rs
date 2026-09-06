//! Crash-safe mediation of Beads issue generation.

use std::{io, path::PathBuf, process::Command};

use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

use crate::{GeneratedWorkReservation, ReserveResult, RunStore, RunStoreError};

/// One brokered Beads command from a Run-owned Agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenerateRequest {
    /// Admitted Run authorizing the mutation.
    pub run_id: String,
    /// Secret generation capability issued at Run admission.
    pub token: String,
    /// Stable UUID reused when retrying the same mutation.
    pub mutation_id: String,
    /// Allowed Beads mutation subcommand.
    pub command: String,
    /// Subcommand arguments, checked before execution.
    pub arguments: Vec<String>,
}

/// Captured subprocess result used by production and deterministic tests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandOutput {
    /// Whether the subprocess exited successfully.
    pub success: bool,
    /// Captured standard output.
    pub stdout: String,
    /// Captured standard error.
    pub stderr: String,
}

/// Confirmed generated issue plus the original Beads output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeneratedIssue {
    /// Confirmed Beads issue identifier.
    pub id: String,
    /// Original success output; empty for an already-confirmed retry.
    pub stdout: String,
}

/// Generated-work broker failure.
#[derive(Debug, Error)]
pub enum GenerationError {
    /// Run authorization, accounting, or persistence failed.
    #[error(transparent)]
    Run(#[from] RunStoreError),
    /// The request violates the constrained mutation contract.
    #[error("invalid generated-work command: {0}")]
    Invalid(String),
    /// No capacity remains; the Run has been Parked.
    #[error("generated-work budget is exhausted; the Run is Parked")]
    Exhausted,
    /// A prior attempt still holds this mutation's reservation.
    #[error("generated-work mutation is pending reconciliation")]
    Pending,
    /// Process execution failed before a mutation could be confirmed.
    #[error("could not start br: {0}")]
    Start(#[from] io::Error),
    /// Execution may have mutated Beads; retain capacity until reconciliation.
    #[error("br mutation outcome is ambiguous; capacity remains reserved: {0}")]
    Ambiguous(String),
}

/// Constrained adapter for one canonical Beads database.
#[derive(Clone, Debug)]
pub struct BeadsGenerator {
    executable: PathBuf,
    database: PathBuf,
}

impl BeadsGenerator {
    /// Bind the broker to an explicit executable and canonical database.
    #[must_use]
    pub fn new(executable: impl Into<PathBuf>, database: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            database: database.into(),
        }
    }

    /// Reconcile pending mutations, reserve capacity, execute Beads, and confirm success.
    ///
    /// # Errors
    /// Returns authorization, validation, capacity, storage, or subprocess errors.
    /// Ambiguous outcomes retain their reservation for reconciliation on retry.
    pub fn generate(
        &self,
        store: &RunStore,
        request: &GenerateRequest,
        now_ms: u64,
    ) -> Result<GeneratedIssue, GenerationError> {
        self.generate_with(store, request, now_ms, |arguments| {
            let output = Command::new(&self.executable).args(arguments).output()?;
            Ok(CommandOutput {
                success: output.status.success(),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            })
        })
    }

    /// Testable broker boundary with caller-supplied process execution.
    ///
    /// # Errors
    /// Returns the same failures as [`Self::generate`], including errors from `execute`.
    pub fn generate_with(
        &self,
        store: &RunStore,
        request: &GenerateRequest,
        now_ms: u64,
        mut execute: impl FnMut(&[String]) -> io::Result<CommandOutput>,
    ) -> Result<GeneratedIssue, GenerationError> {
        validate_request(request)?;
        self.reconcile_with(store, &request.run_id, &request.token, &mut execute)?;
        let actor = store.generation_actor(&request.run_id, &request.token)?;
        let reservation = GeneratedWorkReservation {
            run_id: request.run_id.clone(),
            token: request.token.clone(),
            mutation_id: request.mutation_id.clone(),
            kind: "beads_issue".to_owned(),
            units: 1,
        };
        match store.reserve_generated_work(reservation, now_ms)? {
            ReserveResult::Exhausted => return Err(GenerationError::Exhausted),
            ReserveResult::Pending => return Err(GenerationError::Pending),
            ReserveResult::Consumed => {
                let run = store.run(&request.run_id)?;
                let id = run.generated_issue(&request.mutation_id).ok_or_else(|| {
                    GenerationError::Invalid("confirmed issue is absent".to_owned())
                })?;
                return Ok(GeneratedIssue {
                    id: id.to_owned(),
                    stdout: String::new(),
                });
            }
            ReserveResult::Reserved => {}
        }

        let external_ref = mutation_external_ref(&request.run_id, &request.mutation_id);
        let arguments = generation_arguments(
            &request.command,
            &request.arguments,
            &actor,
            &external_ref,
            &self.database,
        )?;
        let output = match execute(&arguments) {
            Ok(output) => output,
            Err(error) => {
                store.release_generated_work(
                    &request.run_id,
                    &request.token,
                    &request.mutation_id,
                )?;
                return Err(GenerationError::Start(error));
            }
        };
        if !output.success {
            return Err(GenerationError::Ambiguous(nonempty(&output.stderr)));
        }
        let id = parse_issue_id(&output.stdout).ok_or_else(|| {
            GenerationError::Ambiguous("br returned malformed success data".to_owned())
        })?;
        store.confirm_generated_work(&request.run_id, &request.token, &request.mutation_id, &id)?;
        Ok(GeneratedIssue {
            id,
            stdout: output.stdout,
        })
    }

    fn reconcile_with(
        &self,
        store: &RunStore,
        run_id: &str,
        token: &str,
        execute: &mut impl FnMut(&[String]) -> io::Result<CommandOutput>,
    ) -> Result<(), GenerationError> {
        let pending = store.pending_generation_ids(run_id, token)?;
        if pending.is_empty() {
            return Ok(());
        }
        let arguments = vec![
            "list".to_owned(),
            "--status".to_owned(),
            "all".to_owned(),
            "--db".to_owned(),
            self.database.to_string_lossy().into_owned(),
            "--json".to_owned(),
        ];
        let output = execute(&arguments)?;
        if !output.success {
            return Err(GenerationError::Ambiguous(nonempty(&output.stderr)));
        }
        let issues: Vec<Value> = serde_json::from_str(&output.stdout).map_err(|_| {
            GenerationError::Ambiguous("br list returned malformed data".to_owned())
        })?;
        for mutation_id in pending {
            let expected = mutation_external_ref(run_id, &mutation_id);
            if let Some(id) = issues.iter().find_map(|issue| {
                (issue.get("external_ref").and_then(Value::as_str) == Some(expected.as_str()))
                    .then(|| issue.get("id").and_then(Value::as_str))
                    .flatten()
            }) {
                store.confirm_generated_work(run_id, token, &mutation_id, id)?;
            }
        }
        Ok(())
    }
}

/// Stable external reference used to reconcile a Run's Beads mutation.
#[must_use]
pub fn mutation_external_ref(run_id: &str, mutation_id: &str) -> String {
    format!("louiselm-run/{run_id}/mutation/{mutation_id}")
}

fn validate_request(request: &GenerateRequest) -> Result<(), GenerationError> {
    Uuid::parse_str(&request.run_id)
        .map_err(|_| GenerationError::Invalid("Run id must be a UUID".to_owned()))?;
    Uuid::parse_str(&request.mutation_id)
        .map_err(|_| GenerationError::Invalid("mutation id must be a UUID".to_owned()))?;
    if request.command != "create" && request.command != "q" {
        return Err(GenerationError::Invalid(
            "only br create and br q generate work".to_owned(),
        ));
    }
    Ok(())
}

fn generation_arguments(
    command: &str,
    supplied: &[String],
    actor: &str,
    external_ref: &str,
    database: &std::path::Path,
) -> Result<Vec<String>, GenerationError> {
    let mut arguments = if command == "q" {
        quick_to_create(supplied)?
    } else {
        reject_control_flags(supplied)?;
        let mut values = vec!["create".to_owned()];
        values.extend(
            supplied
                .iter()
                .filter(|value| value.as_str() != "--json")
                .cloned(),
        );
        values
    };
    arguments.extend([
        "--actor".to_owned(),
        actor.to_owned(),
        "--external-ref".to_owned(),
        external_ref.to_owned(),
        "--db".to_owned(),
        database.to_string_lossy().into_owned(),
        "--json".to_owned(),
    ]);
    Ok(arguments)
}

fn reject_control_flags(arguments: &[String]) -> Result<(), GenerationError> {
    const FORBIDDEN: &[&str] = &[
        "--actor",
        "--external-ref",
        "--db",
        "--no-db",
        "--no-auto-flush",
        "--no-auto-import",
        "--dry-run",
        "--silent",
    ];
    if let Some(flag) = arguments.iter().find(|argument| {
        FORBIDDEN.iter().any(|forbidden| {
            argument.as_str() == *forbidden || argument.starts_with(&format!("{forbidden}="))
        })
    }) {
        return Err(GenerationError::Invalid(format!(
            "broker owns control flag '{flag}'"
        )));
    }
    Ok(())
}

fn quick_to_create(arguments: &[String]) -> Result<Vec<String>, GenerationError> {
    reject_control_flags(arguments)?;
    let mut output = vec!["create".to_owned()];
    let mut title = Vec::new();
    let mut index = 0;
    while index < arguments.len() {
        let value = &arguments[index];
        if value == "--json" {
            index += 1;
            continue;
        }
        let takes_value = matches!(
            value.as_str(),
            "-p" | "--priority"
                | "-t"
                | "--type"
                | "-l"
                | "--labels"
                | "-d"
                | "--description"
                | "--body"
                | "--parent"
                | "-e"
                | "--estimate"
        );
        if takes_value {
            let next = arguments.get(index + 1).ok_or_else(|| {
                GenerationError::Invalid(format!("quick-capture option '{value}' has no value"))
            })?;
            output.push(value.clone());
            output.push(next.clone());
            index += 2;
        } else if value.starts_with('-') {
            return Err(GenerationError::Invalid(format!(
                "unsupported br q option '{value}'"
            )));
        } else {
            title.push(value.clone());
            index += 1;
        }
    }
    if title.is_empty() {
        return Err(GenerationError::Invalid("br q requires a title".to_owned()));
    }
    output.push("--title".to_owned());
    output.push(title.join(" "));
    Ok(output)
}

fn parse_issue_id(output: &str) -> Option<String> {
    serde_json::from_str::<Value>(output)
        .ok()?
        .get("id")?
        .as_str()
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}

fn nonempty(message: &str) -> String {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        "unknown br failure".to_owned()
    } else {
        trimmed.to_owned()
    }
}
