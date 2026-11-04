//! `endorsement sign-batch|withdraw|export|import|list|prune`.
//!
//! Signing recomputes every package's Dossier from the store and signs one
//! batch with one touch; Agent-supplied digests are never signed. The
//! endorser key must not be any key enrolled for local authority. Exporting
//! copies exactly one stored record. Importing stores exact bytes and decides
//! nothing. `list` shows, for every stored record, the trust decision
//! recomputed now. A trusted record is evidence that may focus review; it
//! never admits anything. `prune` drops expired Endorsements and never a
//! withdrawal.

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

use serde::Serialize;

use super::{CliError, Options, endorser::endorser_key, now_ms, report};
use crate::{
    canonical::Digest,
    dossier::{Dossier, DossierRequest},
    endorsement::{
        self, Batch, EndorsedPackage, EndorserRole, Record, Signing, StatedAssessment,
        StatementKind, Subject,
        import::{self, ImportReport, RefusedFile},
        local,
        trust::{Distrust, TrustDecision},
    },
    signer::SshKeygenSigner,
    store::Store,
    trust::TrustStore,
};

const DAY_MS: u64 = 24 * 60 * 60 * 1000;
const DEFAULT_DAYS: u64 = 90;
const MAX_DAYS: u64 = 180;

#[derive(Serialize)]
struct Listed {
    digest: String,
    kind: StatementKind,
    package_digest: String,
    signer_role: EndorserRole,
    signer_key: String,
    issued_at_ms: u64,
    expires_at_ms: u64,
    trusted: bool,
    trust: String,
}

#[derive(Serialize)]
struct Listing {
    records: Vec<Listed>,
    unreadable: Vec<RefusedFile>,
}

/// What `package` reports when it imported a sibling `endorsements/` tree.
#[derive(Debug, Serialize)]
pub(super) struct ImportSummary {
    imported: usize,
    existing: usize,
    refused: Vec<RefusedFile>,
}

impl ImportSummary {
    pub(super) fn human(&self) -> String {
        let mut lines = vec![format!(
            "endorsements: {} imported, {} already present",
            self.imported, self.existing
        )];
        for file in &self.refused {
            lines.push(format!(
                "endorsement refused {}: {}",
                file.path, file.reason
            ));
        }
        lines.join("\n")
    }
}

impl From<&ImportReport> for ImportSummary {
    fn from(report: &ImportReport) -> Self {
        let imported = report.newly_imported();
        Self {
            imported,
            existing: report.imported.len() - imported,
            refused: report.refused.clone(),
        }
    }
}

pub(super) fn run(options: &Options) -> Result<i32, CliError> {
    let store = options.store()?;
    match options.subject("endorsement")? {
        "import" => {
            let path = options.positional.get(1).ok_or_else(|| {
                CliError::Invalid("endorsement import needs a file or directory".into())
            })?;
            let report_value = import::import_path(&store, std::path::Path::new(path))?;
            let refused = !report_value.refused.is_empty();
            report(options, &report_value, |report| {
                let mut lines = vec![format!(
                    "{} imported, {} already present; trust is decided at use, never here",
                    report.newly_imported(),
                    report.imported.len() - report.newly_imported()
                )];
                for file in &report.refused {
                    lines.push(format!("refused {}: {}", file.path, file.reason));
                }
                lines.join("\n")
            })?;
            Ok(i32::from(refused))
        }
        "list" => {
            let stored = import::records(&store)?;
            let (endorsers, _) = local::effective(&store)?;
            let now = now_ms();
            let records = stored
                .records
                .iter()
                .map(|record| listed(record, &endorsers.decide(record, now)))
                .collect();
            let listing = Listing {
                records,
                unreadable: stored.unreadable,
            };
            report(options, &listing, human_listing)
        }
        "prune" => {
            let pruned: Vec<String> = import::prune_expired(&store, now_ms())?
                .iter()
                .map(ToString::to_string)
                .collect();
            report(options, &pruned, |pruned| {
                format!(
                    "{} expired Endorsement(s) pruned; withdrawals are kept",
                    pruned.len()
                )
            })
        }
        verb @ ("sign-batch" | "withdraw") => {
            let signing = signing(options, &store)?;
            let batch = if verb == "sign-batch" {
                Batch::Endorsements(packages(options, &store)?)
            } else {
                Batch::Withdrawals(members(options)?)
            };
            let records = endorsement::sign_batch(
                &signing,
                &batch,
                &SshKeygenSigner::new(&options.signing_key()?),
            )?;
            let written = write_records(options, &records)?;
            report(options, &written, |written| {
                format!(
                    "{} record(s) signed with one signature and written; publish them beside the skills",
                    written.len()
                )
            })
        }
        "export" => {
            let digest = Digest::parse(options.positional.get(1).ok_or_else(|| {
                CliError::Invalid("endorsement export needs a record digest".into())
            })?)?;
            let output = options
                .output
                .as_deref()
                .ok_or_else(|| CliError::Invalid("--output is required".into()))?;
            let record = import::records(&store)?
                .records
                .into_iter()
                .find(|record| record.digest() == digest)
                .ok_or_else(|| CliError::Invalid(format!("no stored Endorsement {digest}")))?;
            write_new(output, &record.to_bytes())?;
            report(options, &digest.to_string(), |digest| {
                format!("exported {digest}; nothing else about this store is in the file")
            })
        }
        other => Err(CliError::Invalid(format!(
            "unknown endorsement verb '{other}'"
        ))),
    }
}

fn signing(options: &Options, store: &Store) -> Result<Signing, CliError> {
    let signer_key = endorser_key(options)?;
    if let Some(trust) = TrustStore::load(store)?
        && trust.keys.iter().any(|key| {
            key.public_key
                .split_whitespace()
                .take(2)
                .collect::<Vec<_>>()
                .join(" ")
                == signer_key
        })
    {
        return Err(CliError::Invalid(
            "the endorser key is enrolled for local authority; use a separate publisher or reviewer credential".into(),
        ));
    }
    let signer_role = match options.roles.as_slice() {
        [role] if role == "publisher" => EndorserRole::Publisher,
        [role] if role == "reviewer" => EndorserRole::Reviewer,
        _ => {
            return Err(CliError::Invalid(
                "give exactly one --role: publisher or reviewer".into(),
            ));
        }
    };
    let days = options.days.unwrap_or(DEFAULT_DAYS);
    if !(1..=MAX_DAYS).contains(&days) {
        return Err(CliError::Invalid("--days must be 1 to 180".into()));
    }
    let issued_at_ms = now_ms();
    Ok(Signing {
        signer_role,
        signer_key,
        issued_at_ms,
        expires_at_ms: issued_at_ms.saturating_add(days * DAY_MS),
    })
}

fn members(options: &Options) -> Result<Vec<Digest>, CliError> {
    if options.members.is_empty() {
        return Err(CliError::Invalid(
            "give at least one --member DIGEST".into(),
        ));
    }
    options
        .members
        .iter()
        .map(|member| Digest::parse(member).map_err(CliError::from))
        .collect()
}

fn packages(options: &Options, store: &Store) -> Result<Vec<EndorsedPackage>, CliError> {
    let policy = options.policy()?;
    members(options)?
        .into_iter()
        .map(|digest| {
            let mut request = DossierRequest::new(&digest).with_review_depth(options.review_depth);
            if let (Some(model), Some(prompt)) =
                (&options.assessment_model, &options.assessment_prompt)
            {
                request = request.with_assessment_key(model, prompt);
            }
            let dossier = Dossier::build(store, &policy, &request)?;
            if !dossier.reviewable() {
                return Err(CliError::Invalid(format!(
                    "{digest} is not reviewable here; it cannot be endorsed"
                )));
            }
            let assessment = match (&options.assessment_model, dossier.assessment.as_ref()) {
                (None, _) => None,
                (Some(_), None) => {
                    return Err(CliError::Invalid(format!(
                        "no stored Assessment of {digest} matches the stated model and prompt"
                    )));
                }
                (Some(model), Some(_)) => Some(StatedAssessment {
                    model: model.clone(),
                    prompt_version: options.assessment_prompt.clone().unwrap_or_default(),
                }),
            };
            Ok(EndorsedPackage {
                package_digest: digest.clone(),
                policy_digest: policy.digest().clone(),
                dossier_digest: Some(dossier.portable_digest()),
                assessment,
                review_depth: options.review_depth,
                assumptions: options.assumptions.clone(),
            })
        })
        .collect()
}

fn write_records(options: &Options, records: &[Vec<u8>]) -> Result<Vec<String>, CliError> {
    let output = options
        .output
        .as_deref()
        .ok_or_else(|| CliError::Invalid("--output is required".into()))?;
    fs::create_dir_all(output).map_err(|error| {
        CliError::Invalid(format!("cannot create {}: {error}", output.display()))
    })?;
    records
        .iter()
        .map(|bytes| {
            let digest = Digest::of(bytes);
            write_new(
                &output.join(format!("{}.json", digest.directory_name())),
                bytes,
            )?;
            Ok(digest.to_string())
        })
        .collect()
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), CliError> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .and_then(|mut file| {
            file.write_all(bytes)?;
            file.sync_all()
        })
        .map_err(|error| CliError::Invalid(format!("cannot write {}: {error}", path.display())))
}

fn listed(record: &Record, decision: &TrustDecision) -> Listed {
    let signing = record.signing();
    let package_digest = match record.subject() {
        Subject::Endorsed(package) => package.package_digest.to_string(),
        Subject::Withdrawn(digest) => digest.to_string(),
    };
    Listed {
        digest: record.digest().to_string(),
        kind: record.kind(),
        package_digest,
        signer_role: signing.signer_role,
        signer_key: signing.signer_key.clone(),
        issued_at_ms: signing.issued_at_ms,
        expires_at_ms: signing.expires_at_ms,
        trusted: matches!(decision, TrustDecision::Trusted { .. }),
        trust: describe(decision),
    }
}

fn describe(decision: &TrustDecision) -> String {
    match decision {
        TrustDecision::Trusted { label } => format!("trusted endorser {label}"),
        TrustDecision::Untrusted(Distrust::UnknownSigner) => "untrusted: unknown signer".into(),
        TrustDecision::Untrusted(Distrust::RoleNotAllowed { role }) => {
            format!("untrusted: key not trusted as {role:?}").to_lowercase()
        }
        TrustDecision::Untrusted(Distrust::LifetimeExceedsCap {
            lifetime_ms,
            cap_ms,
        }) => format!("untrusted: lifetime {lifetime_ms} ms exceeds cap {cap_ms} ms"),
        TrustDecision::Untrusted(Distrust::Refused(error)) => format!("untrusted: {error}"),
    }
}

fn human_listing(listing: &Listing) -> String {
    let mut lines = Vec::new();
    if listing.records.is_empty() {
        lines.push("no imported Endorsements".to_owned());
    }
    for record in &listing.records {
        lines.push(format!(
            "{:?} of {} by {:?} — {}",
            record.kind, record.package_digest, record.signer_role, record.trust
        ));
    }
    for file in &listing.unreadable {
        lines.push(format!("unreadable {}: {}", file.path, file.reason));
    }
    lines.join("\n")
}
