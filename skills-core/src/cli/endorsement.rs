//! `endorsement import|list|prune`: imported Endorsement evidence.
//!
//! Importing stores exact bytes and decides nothing. `list` shows, for every
//! stored record, the trust decision recomputed now from the effective
//! endorser set. A trusted record is evidence that may focus review; it never
//! admits anything. `prune` drops expired Endorsements and never a withdrawal.

use serde::Serialize;

use super::{CliError, Options, now_ms, report};
use crate::endorsement::{
    EndorserRole, Record, StatementKind, Subject,
    import::{self, ImportReport, RefusedFile},
    local,
    trust::{Distrust, TrustDecision},
};

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
        other => Err(CliError::Invalid(format!(
            "unknown endorsement verb '{other}'"
        ))),
    }
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
