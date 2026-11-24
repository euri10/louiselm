//! Reapply exact saved path changes without touching either source tree.

use super::Changes;
use crate::workspace::{SourceFiles, WorkspaceError};

impl Changes {
    pub(crate) fn refresh(&self, mut base: SourceFiles) -> Result<Self, WorkspaceError> {
        let baseline = crate::workspace::entries(&base);
        let current: std::collections::BTreeMap<_, _> =
            baseline.iter().map(|f| (&f.path, f)).collect();
        let previous: std::collections::BTreeMap<_, _> =
            self.baseline.iter().map(|f| (&f.path, f)).collect();
        // Exact path changes only: no text merge or automatic conflict resolution.
        for path in self
            .preview
            .added
            .iter()
            .chain(&self.preview.modified)
            .chain(&self.preview.deleted)
        {
            if current.get(path) != previous.get(path) {
                return Err(WorkspaceError::Invalid(
                    "saved changes conflict with the new baseline",
                ));
            }
            match self.files.get(path) {
                Some(file) => {
                    base.insert(
                        path.clone(),
                        crate::workspace::SourceFile {
                            bytes: file.bytes.clone(),
                            executable: file.executable,
                        },
                    );
                }
                None => {
                    base.remove(path);
                }
            }
        }
        Self::new(baseline, base)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "Tests assert exact fixture outcomes.")]
    use super::*;
    use crate::workspace::{SourceFile, entries};

    fn files(values: &[(&str, &str)]) -> SourceFiles {
        values
            .iter()
            .map(|(path, bytes)| {
                (
                    (*path).into(),
                    SourceFile {
                        bytes: bytes.as_bytes().to_vec(),
                        executable: false,
                    },
                )
            })
            .collect()
    }

    #[test]
    fn refresh_preserves_new_base_and_exact_saved_changes() {
        let old = files(&[("modified", "old"), ("deleted", "old"), ("other", "old")]);
        let saved = Changes::new(
            entries(&old),
            files(&[("modified", "saved"), ("added", "saved"), ("other", "old")]),
        )
        .unwrap();
        let new = files(&[
            ("modified", "old"),
            ("deleted", "old"),
            ("other", "new"),
            ("new", "new"),
        ]);
        let refreshed = saved.refresh(new).unwrap();
        assert_eq!(refreshed.files["modified"].bytes, b"saved");
        assert_eq!(refreshed.files["other"].bytes, b"new");
        assert_eq!(refreshed.files["new"].bytes, b"new");
        assert_eq!(refreshed.files["added"].bytes, b"saved");
        assert!(!refreshed.files.contains_key("deleted"));
        assert_eq!(saved.files["other"].bytes, b"old");
        assert_eq!(saved.preview, refreshed.preview);
    }

    #[test]
    fn refresh_refuses_conflicting_bytes_modes_deletions_and_path_collisions() {
        let old = files(&[("modified", "old"), ("deleted", "old")]);
        let saved = Changes::new(
            entries(&old),
            files(&[("modified", "saved"), ("added", "saved")]),
        )
        .unwrap();
        for new in [
            files(&[("modified", "concurrent"), ("deleted", "old")]),
            files(&[("deleted", "old")]),
            files(&[("modified", "old"), ("deleted", "concurrent")]),
            files(&[
                ("modified", "old"),
                ("deleted", "old"),
                ("added", "concurrent"),
            ]),
            files(&[
                ("modified", "old"),
                ("deleted", "old"),
                ("added/child", "concurrent"),
            ]),
        ] {
            assert!(saved.refresh(new).is_err());
        }
        let mut new = files(&[("modified", "old"), ("deleted", "old")]);
        new.get_mut("modified").unwrap().executable = true;
        assert!(saved.refresh(new).is_err());
        assert_eq!(saved.files["modified"].bytes, b"saved");
    }
}
