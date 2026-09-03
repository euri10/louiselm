//! The human view of a Dossier.
//!
//! Every value printed here has already passed through escaping at the point
//! it was produced, so this module only lays out facts. That ordering matters:
//! escaping at render time would leave the same hostile bytes live in the
//! robot view, which is read by an Agent that will act on them.

use crate::{
    diff::{Change, LineKind},
    dossier::{AssessmentState, Dossier},
    inspect::FindingKind,
};

/// Renders a Dossier as reviewer-facing text.
pub fn human(dossier: &Dossier) -> String {
    let mut out = String::new();
    let package = &dossier.package;

    push(&mut out, &format!("Dossier {}", package.digest));
    push(
        &mut out,
        &format!(
            "  skill        {}",
            package.skill_name.as_deref().unwrap_or("(none declared)"),
        ),
    );
    if let Some(description) = &package.skill_description {
        push(&mut out, &format!("  description  {description}"));
    }
    push(
        &mut out,
        &format!(
            "  contents     {} file(s), {} byte(s)",
            package.entry_count, package.total_bytes
        ),
    );
    push(
        &mut out,
        &format!(
            "  policy       {} ({}), unicode profile {}",
            dossier.policy.version, dossier.policy.digest, dossier.policy.unicode_profile_version,
        ),
    );
    push(
        &mut out,
        &format!("  review depth {} (claimed)", dossier.review_depth.name()),
    );

    push(&mut out, "");
    if dossier.verification.intact {
        push(&mut out, "Verification: stored bytes match the digest.");
    } else {
        push(&mut out, "Verification: FAILED.");
        for failure in &dossier.verification.failures {
            push(&mut out, &format!("  - {failure}"));
        }
    }

    if !dossier.inspection.fatal.is_empty() {
        push(&mut out, "");
        push(&mut out, "Fatal:");
        for fatal in &dossier.inspection.fatal {
            push(
                &mut out,
                &format!(
                    "  - [{:?}] {}{}",
                    fatal.kind,
                    fatal.message,
                    fatal
                        .path
                        .as_ref()
                        .map(|path| format!(" ({path})"))
                        .unwrap_or_default(),
                ),
            );
        }
    }

    push(&mut out, "");
    let counts = dossier.inspection.counts_by_kind();
    if counts.is_empty() {
        push(&mut out, "Findings: none.");
    } else {
        push(&mut out, "Findings by kind:");
        for (kind, count) in &counts {
            push(&mut out, &format!("  {kind:<22} {count}"));
        }
        push(&mut out, "");
        for finding in &dossier.inspection.findings {
            push(
                &mut out,
                &format!(
                    "[{}] {} {} ({} occurrence(s))",
                    finding.id,
                    finding.kind.name(),
                    finding.path,
                    finding.occurrences,
                ),
            );
            push(&mut out, &format!("  {}", finding.message));
            for sample in &finding.samples {
                let location = if sample.line == 0 {
                    String::new()
                } else {
                    format!("line {}: ", sample.line)
                };
                push(
                    &mut out,
                    &format!("    {location}{} [{}]", sample.evidence, sample.detail),
                );
            }
        }
    }

    if !dossier.executables.is_empty() {
        push(&mut out, "");
        push(&mut out, "Executables:");
        for path in &dossier.executables {
            push(&mut out, &format!("  {path}"));
        }
    }

    if let Some(diff) = &dossier.diff {
        push(&mut out, "");
        push(
            &mut out,
            &format!(
                "Update from {}: {} added, {} removed, {} changed, {} mode-only",
                diff.base_digest,
                diff.count(Change::Added),
                diff.count(Change::Removed),
                diff.count(Change::ContentChanged),
                diff.count(Change::ModeChanged),
            ),
        );
        for entry in &diff.entries {
            push(&mut out, &format!("  {:?} {}", entry.change, entry.path));
            if let Some(note) = &entry.note {
                push(&mut out, &format!("    ({note})"));
            }
            for line in &entry.lines {
                let marker = match line.kind {
                    LineKind::Added => '+',
                    LineKind::Removed => '-',
                    LineKind::Context => ' ',
                };
                push(&mut out, &format!("    {marker}{}", line.text));
            }
        }
    }

    push(&mut out, "");
    push(&mut out, "Supply lineage (local, not portable):");
    if dossier.lineage.captures.is_empty() {
        push(&mut out, "  none recorded");
    }
    for capture in &dossier.lineage.captures {
        push(
            &mut out,
            &format!(
                "  captured at {} from {}",
                capture.captured_at_ms, capture.source_root
            ),
        );
        for link in &capture.links {
            push(
                &mut out,
                &format!(
                    "    link {} -> {}{}",
                    link.path,
                    link.resolved_target,
                    if link.escapes_root {
                        " (outside the candidate root)"
                    } else {
                        ""
                    },
                ),
            );
        }
    }

    push(&mut out, "");
    match dossier.assessment_state {
        AssessmentState::Absent => push(&mut out, "Assessment: none (advisory only when present)."),
        AssessmentState::Superseded => push(
            &mut out,
            "Assessment: recorded for other bytes, Model, or prompt; ignored.",
        ),
        AssessmentState::Current => {
            if let Some(assessment) = &dossier.assessment {
                push(
                    &mut out,
                    &format!(
                        "Assessment (advisory, no authority): {:?} by {} under {}",
                        assessment.verdict, assessment.key.model, assessment.key.prompt_version,
                    ),
                );
                push(&mut out, &format!("  {}", assessment.rationale));
            }
        }
    }

    push(&mut out, "");
    push(&mut out, "Next:");
    for action in &dossier.next_actions {
        push(&mut out, &format!("  [{}] {}", action.id, action.detail));
    }
    out
}

/// Renders the one-line summary used when listing packages.
pub fn summary_line(dossier: &Dossier) -> String {
    format!(
        "{} {} {} finding(s){}",
        dossier.package.digest,
        dossier.package.skill_name.as_deref().unwrap_or("(unnamed)"),
        dossier.inspection.count(FindingKind::Executable)
            + dossier.inspection.findings.len() as u64,
        if dossier.reviewable() {
            ""
        } else {
            " NOT REVIEWABLE"
        },
    )
}

fn push(out: &mut String, line: &str) {
    out.push_str(line);
    out.push('\n');
}

/// Renders Generation status as operator-facing text.
pub fn generation_status(status: &crate::admission::GenerationStatus) -> String {
    let mut out = String::new();
    match (&status.generation, status.state) {
        (Some(generation), Some(state)) => {
            push(
                &mut out,
                &format!(
                    "Generation {generation}\n  state      {}\n  sequence   {}",
                    state.name(),
                    status.sequence.unwrap_or_default(),
                ),
            );
            if let Some(predecessor) = &status.predecessor {
                push(&mut out, &format!("  follows    {predecessor}"));
            }
            if let Some(role) = &status.signer_role {
                push(&mut out, &format!("  signed by  {role} role"));
            }
            match &status.witness {
                Some(witness) => push(
                    &mut out,
                    &format!(
                        "  witnessed  {} branch {} commit {}",
                        witness.remote, witness.branch, witness.commit
                    ),
                ),
                None => push(&mut out, "  witnessed  no"),
            }
            push(
                &mut out,
                &format!("  members    {} in force", status.effective_members.len()),
            );
            for member in &status.excluded_members {
                push(&mut out, &format!("  quarantined {member}"));
            }
        }
        _ => push(&mut out, "No Skill Generation is in force."),
    }
    if !status.pending.is_empty() {
        push(&mut out, "");
        push(&mut out, "Signed but not in force:");
        for pending in &status.pending {
            push(&mut out, &format!("  {pending}"));
        }
    }
    if let Some(failure) = &status.failure {
        push(&mut out, &format!("Failure: {failure}"));
    }
    push(&mut out, "");
    push(
        &mut out,
        &format!(
            "Next: [{}] {}",
            status.next_action.id, status.next_action.detail
        ),
    );
    out
}
