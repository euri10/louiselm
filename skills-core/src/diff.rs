//! Update diffs between two packages.
//!
//! A reviewer approving an update is approving a change, not a tree, so the
//! Dossier shows what moved. Diff text passes through the same escaping as
//! every other reviewer-facing surface: a hostile line in an incoming update is
//! exactly the line most likely to be crafted to act on a terminal.

use std::collections::BTreeSet;

use serde::Serialize;

use crate::{
    scan,
    store::{Package, StoreError},
};

/// Longest text file, in lines, compared line by line.
///
/// Beyond this the diff reports a whole-file replacement and says so, rather
/// than silently showing less than it claims.
pub const MAX_DIFF_LINES: usize = 2000;

/// Lines of unchanged context kept around each change.
pub const CONTEXT_LINES: usize = 3;

/// How one path differs between two packages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Change {
    /// The path exists only in the target package.
    Added,
    /// The path exists only in the base package.
    Removed,
    /// The path exists in both with different content.
    ContentChanged,
    /// The path exists in both with the same content and a different mode.
    ModeChanged,
}

/// Whether a diff line is context or a change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LineKind {
    /// Present in both packages.
    Context,
    /// Present only in the target package.
    Added,
    /// Present only in the base package.
    Removed,
}

/// One line of a rendered diff.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DiffLine {
    /// Whether the line is context, added, or removed.
    pub kind: LineKind,
    /// One-based line number in the base package, when the line is in it.
    pub base_line: Option<u64>,
    /// One-based line number in the target package, when the line is in it.
    pub target_line: Option<u64>,
    /// Escaped line text.
    pub text: String,
}

/// How one path changed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DiffEntry {
    /// Package-relative path.
    pub path: String,
    /// The kind of change.
    pub change: Change,
    /// Whether the change gave the file the executable bit.
    pub became_executable: bool,
    /// Line-level changes, empty when lines cannot honestly be shown.
    pub lines: Vec<DiffLine>,
    /// Why no lines are shown, when none are.
    pub note: Option<String>,
}

/// Every difference between two packages, ordered by path.
#[derive(Clone, Debug, Serialize)]
pub struct PackageDiff {
    /// Digest of the package being replaced.
    pub base_digest: String,
    /// Digest of the package under review.
    pub target_digest: String,
    /// One entry per changed path.
    pub entries: Vec<DiffEntry>,
}

impl PackageDiff {
    /// Compares `base` against `target`, reading both packages' bytes.
    ///
    /// # Errors
    /// Returns a package-read error when content needed for the diff is unavailable.
    pub fn between(base: &Package, target: &Package) -> Result<Self, StoreError> {
        let paths = base
            .manifest
            .entries
            .iter()
            .chain(target.manifest.entries.iter())
            .map(|entry| entry.path.clone())
            .collect::<BTreeSet<_>>();

        let mut entries = Vec::new();
        for path in paths {
            let before = base.manifest.entry(&path);
            let after = target.manifest.entry(&path);
            let entry = match (before, after) {
                (None, Some(after)) => DiffEntry {
                    path: path.clone(),
                    change: Change::Added,
                    became_executable: after.executable,
                    lines: Vec::new(),
                    note: Some("added".to_owned()),
                },
                (Some(_), None) => DiffEntry {
                    path: path.clone(),
                    change: Change::Removed,
                    became_executable: false,
                    lines: Vec::new(),
                    note: Some("removed".to_owned()),
                },
                (Some(before), Some(after)) => {
                    if before.sha256 == after.sha256 {
                        if before.executable == after.executable {
                            continue;
                        }
                        DiffEntry {
                            path: path.clone(),
                            change: Change::ModeChanged,
                            became_executable: after.executable && !before.executable,
                            lines: Vec::new(),
                            note: Some("mode only".to_owned()),
                        }
                    } else {
                        let mut entry = DiffEntry {
                            path: path.clone(),
                            change: Change::ContentChanged,
                            became_executable: after.executable && !before.executable,
                            lines: Vec::new(),
                            note: None,
                        };
                        let old = base.read(&path)?;
                        let new = target.read(&path)?;
                        match (as_text(&old), as_text(&new)) {
                            (Some(old), Some(new)) => {
                                let old_lines = old.lines().collect::<Vec<_>>();
                                let new_lines = new.lines().collect::<Vec<_>>();
                                if old_lines.len() > MAX_DIFF_LINES
                                    || new_lines.len() > MAX_DIFF_LINES
                                {
                                    entry.note = Some(format!(
                                        "content replaced; over {MAX_DIFF_LINES} lines, not compared line by line"
                                    ));
                                } else {
                                    entry.lines = render(&old_lines, &new_lines);
                                }
                            }
                            _ => entry.note = Some("binary content".to_owned()),
                        }
                        entry
                    }
                }
                (None, None) => continue,
            };
            entries.push(entry);
        }

        Ok(Self {
            base_digest: base.digest.to_string(),
            target_digest: target.digest.to_string(),
            entries,
        })
    }

    /// Reports whether anything changed at all.
    #[must_use]
    pub fn has_changes(&self) -> bool {
        !self.entries.is_empty()
    }

    /// Counts entries of one change kind.
    #[must_use]
    pub fn count(&self, change: Change) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.change == change)
            .count()
    }
}

fn as_text(bytes: &[u8]) -> Option<&str> {
    if scan::contains_nul(bytes) {
        return None;
    }
    std::str::from_utf8(bytes).ok()
}

#[expect(
    clippy::expect_used,
    reason = "This function builds the script itself: additions have a target index; context and removals have a base index."
)]
fn render(old: &[&str], new: &[&str]) -> Vec<DiffLine> {
    let table = longest_common_subsequence(old, new);
    let mut script = Vec::new();
    let (mut left, mut right) = (0_usize, 0_usize);
    while left < old.len() && right < new.len() {
        if old[left] == new[right] {
            script.push((LineKind::Context, Some(left), Some(right)));
            left += 1;
            right += 1;
        } else if table[left + 1][right] >= table[left][right + 1] {
            script.push((LineKind::Removed, Some(left), None));
            left += 1;
        } else {
            script.push((LineKind::Added, None, Some(right)));
            right += 1;
        }
    }
    while left < old.len() {
        script.push((LineKind::Removed, Some(left), None));
        left += 1;
    }
    while right < new.len() {
        script.push((LineKind::Added, None, Some(right)));
        right += 1;
    }

    let keep = context_window(&script);
    script
        .into_iter()
        .enumerate()
        .filter(|(index, _)| keep.contains(index))
        .map(|(_, (kind, left, right))| DiffLine {
            kind,
            base_line: left.map(|index| index as u64 + 1),
            target_line: right.map(|index| index as u64 + 1),
            text: scan::escape(match kind {
                LineKind::Added => new[right.expect("added lines have a target index")],
                _ => old[left.expect("context and removed lines have a base index")],
            }),
        })
        .collect()
}

fn context_window(script: &[(LineKind, Option<usize>, Option<usize>)]) -> BTreeSet<usize> {
    let mut keep = BTreeSet::new();
    for (index, (kind, _, _)) in script.iter().enumerate() {
        if *kind == LineKind::Context {
            continue;
        }
        let start = index.saturating_sub(CONTEXT_LINES);
        let end = (index + CONTEXT_LINES).min(script.len().saturating_sub(1));
        for position in start..=end {
            keep.insert(position);
        }
    }
    keep
}

fn longest_common_subsequence(old: &[&str], new: &[&str]) -> Vec<Vec<u32>> {
    let mut table = vec![vec![0_u32; new.len() + 1]; old.len() + 1];
    for left in (0..old.len()).rev() {
        for right in (0..new.len()).rev() {
            table[left][right] = if old[left] == new[right] {
                table[left + 1][right + 1] + 1
            } else {
                table[left + 1][right].max(table[left][right + 1])
            };
        }
    }
    table
}
