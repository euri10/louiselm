//! Persistence faults use real files and the production publication checkpoints.
//! Signatures are irrelevant at this private storage boundary; the integration
//! tests exercise the public activation ceremony with real software signatures.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Isolated storage fixtures abort only the failing test."
)]

use super::*;
use crate::{
    admission,
    generation::{GENERATION_SCHEMA, GenerationPayload, RECORD_SCHEMA},
};

struct Fixture {
    _directory: tempfile::TempDir,
    store: Store,
    previous: GenerationRecord,
    candidate: GenerationRecord,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path()).unwrap();
        let previous = record(1, None, GenerationState::Current);
        let candidate = record(
            2,
            Some(previous.generation.clone()),
            GenerationState::PendingWitness,
        );
        admission::write_record(&store, &previous).unwrap();
        admission::write_record(&store, &candidate).unwrap();
        fs::write(
            store.root().join("pins.jsonl"),
            admission::pin_line(&previous).unwrap(),
        )
        .unwrap();
        Self {
            _directory: directory,
            store,
            previous,
            candidate,
        }
    }

    fn activated(&self) -> GenerationRecord {
        let mut current = self.candidate.clone();
        current.state = GenerationState::Current;
        current
    }

    fn assert_restored(&self) {
        assert_eq!(
            admission::current(&self.store).unwrap(),
            Some(self.previous.clone())
        );
        assert_eq!(
            admission::load(&self.store, &self.candidate.digest()).unwrap(),
            self.candidate
        );
        assert_eq!(
            fs::read_to_string(self.store.root().join("pins.jsonl")).unwrap(),
            admission::pin_line(&self.previous).unwrap()
        );
        assert!(!self.store.root().join(JOURNAL).exists());
    }
}

fn record(sequence: u64, predecessor: Option<String>, state: GenerationState) -> GenerationRecord {
    let payload = GenerationPayload::new(
        "storage-test",
        sequence,
        predecessor,
        &Digest::of(b"policy").to_string(),
        Vec::new(),
        std::collections::BTreeMap::new(),
    );
    assert_eq!(payload.schema, GENERATION_SCHEMA);
    GenerationRecord {
        schema: RECORD_SCHEMA.to_owned(),
        generation: payload.digest().to_string(),
        payload,
        signature: "storage fixture; not an admission signature".to_owned(),
        state,
        witness: None,
        admitted_at_ms: sequence,
        invalid_reason: None,
    }
}

fn injected_error() -> AdmissionError {
    io_error(
        Path::new("injected-publication-fault"),
        io::Error::other("fixture failure"),
    )
}

#[test]
fn every_precommit_failure_restores_supply_and_lineage() {
    for stop in [
        Checkpoint::JournalDurable,
        Checkpoint::PreviousWritten,
        Checkpoint::CandidateWritten,
        Checkpoint::PinsWritten,
    ] {
        let fixture = Fixture::new();
        let locked = lock(&fixture.store).unwrap();
        let result = activate_with(
            &fixture.store,
            Some(&fixture.previous),
            &fixture.activated(),
            |point| {
                if point == stop {
                    Err(injected_error())
                } else {
                    Ok(())
                }
            },
        );
        assert!(
            matches!(result, Err(AdmissionError::Io { .. })),
            "{stop:?}: {result:?}"
        );
        drop(locked);
        fixture.assert_restored();
    }
}

#[test]
fn a_torn_candidate_write_is_recovered_before_reading_supply() {
    let fixture = Fixture::new();
    let locked = lock(&fixture.store).unwrap();
    let result = activate_with(
        &fixture.store,
        Some(&fixture.previous),
        &fixture.activated(),
        |point| {
            if point == Checkpoint::PreviousWritten {
                fs::write(
                    admission::record_path(&fixture.store, &fixture.candidate.digest()),
                    b"{",
                )
                .unwrap();
                Err(injected_error())
            } else {
                Ok(())
            }
        },
    );
    assert!(matches!(result, Err(AdmissionError::Io { .. })));
    drop(locked);
    fixture.assert_restored();
}

#[test]
fn rollback_failure_blocks_readers_until_the_fault_is_repaired() {
    let fixture = Fixture::new();
    let candidate_path = admission::record_path(&fixture.store, &fixture.candidate.digest());
    let locked = lock(&fixture.store).unwrap();
    let result = activate_with(
        &fixture.store,
        Some(&fixture.previous),
        &fixture.activated(),
        |point| {
            if point == Checkpoint::PreviousWritten {
                fs::remove_file(&candidate_path).unwrap();
                fs::create_dir(&candidate_path).unwrap();
            }
            Ok(())
        },
    );
    assert!(matches!(
        result,
        Err(AdmissionError::RecoveryRequired { .. })
    ));
    drop(locked);
    assert!(admission::current(&fixture.store).is_err());
    assert!(admission::load(&fixture.store, &fixture.previous.digest()).is_err());
    assert!(admission::list(&fixture.store).is_err());
    assert!(admission::status(&fixture.store).is_err());
    assert!(fixture.store.root().join(JOURNAL).exists());
    fs::remove_dir(&candidate_path).unwrap();
    fixture.assert_restored();
}

#[test]
fn final_sync_failure_reports_an_uncertain_commit_without_rolling_back() {
    let fixture = Fixture::new();
    let locked = lock(&fixture.store).unwrap();
    let result = activate_with(
        &fixture.store,
        Some(&fixture.previous),
        &fixture.activated(),
        |point| {
            if point == Checkpoint::JournalRemoved {
                Err(injected_error())
            } else {
                Ok(())
            }
        },
    );
    assert!(matches!(
        result,
        Err(AdmissionError::CommitUncertain { .. })
    ));
    drop(locked);
    assert_eq!(
        admission::current(&fixture.store).unwrap(),
        Some(fixture.activated())
    );
    assert_eq!(
        fs::read_to_string(fixture.store.root().join("pins.jsonl"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    assert!(!fixture.store.root().join(JOURNAL).exists());
    confirm(&fixture.store, &fixture.candidate.digest()).unwrap();
}

#[test]
fn crash_child() {
    let Ok(root) = std::env::var("LOUISELM_ACTIVATION_CRASH_ROOT") else {
        return;
    };
    let stop = std::env::var("LOUISELM_ACTIVATION_CRASH_POINT").unwrap();
    let store = Store::open(Path::new(&root)).unwrap();
    let records = admission::list(&store).unwrap();
    let previous = records
        .iter()
        .find(|record| record.state == GenerationState::Current);
    let mut candidate = records
        .iter()
        .find(|record| record.state == GenerationState::PendingWitness)
        .unwrap()
        .clone();
    candidate.state = GenerationState::Current;
    let _locked = lock(&store).unwrap();
    activate_with(&store, previous, &candidate, |point| {
        if format!("{point:?}") == stop {
            std::process::exit(77);
        }
        Ok(())
    })
    .unwrap();
    panic!("the requested crash checkpoint was not reached");
}

#[test]
fn restart_recovers_each_precommit_crash_and_preserves_a_completed_commit() {
    for stop in [
        Checkpoint::JournalDurable,
        Checkpoint::PreviousWritten,
        Checkpoint::CandidateWritten,
        Checkpoint::PinsWritten,
        Checkpoint::JournalRemoved,
    ] {
        let fixture = Fixture::new();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "admission::transaction::tests::crash_child",
                "--nocapture",
            ])
            .env("LOUISELM_ACTIVATION_CRASH_ROOT", fixture.store.root())
            .env("LOUISELM_ACTIVATION_CRASH_POINT", format!("{stop:?}"))
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(77), "{stop:?}: {output:?}");
        if stop == Checkpoint::JournalRemoved {
            assert_eq!(
                admission::current(&fixture.store).unwrap(),
                Some(fixture.activated())
            );
            assert_eq!(
                fs::read_to_string(fixture.store.root().join("pins.jsonl"))
                    .unwrap()
                    .lines()
                    .count(),
                2
            );
        } else {
            fixture.assert_restored();
        }
    }
}

#[test]
fn first_activation_rollback_restores_absent_lineage() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path()).unwrap();
    let candidate = record(1, None, GenerationState::PendingWitness);
    admission::write_record(&store, &candidate).unwrap();
    let mut current = candidate.clone();
    current.state = GenerationState::Current;
    let locked = lock(&store).unwrap();
    let result = activate_with(&store, None, &current, |point| {
        if point == Checkpoint::PinsWritten {
            Err(injected_error())
        } else {
            Ok(())
        }
    });
    assert!(matches!(result, Err(AdmissionError::Io { .. })));
    drop(locked);
    assert_eq!(admission::current(&store).unwrap(), None);
    assert_eq!(
        admission::load(&store, &candidate.digest()).unwrap(),
        candidate
    );
    assert!(!store.root().join("pins.jsonl").exists());
}

#[test]
fn malformed_journal_refuses_readers_without_rewriting_any_record() {
    let fixture = Fixture::new();
    fs::write(fixture.store.root().join(JOURNAL), "{").unwrap();
    assert!(matches!(
        admission::current(&fixture.store),
        Err(AdmissionError::Malformed(_))
    ));
    assert_eq!(
        fs::read(admission::record_path(
            &fixture.store,
            &fixture.previous.digest()
        ))
        .unwrap(),
        serde_json::to_vec(&fixture.previous).unwrap()
    );
    assert_eq!(
        fs::read_to_string(fixture.store.root().join(JOURNAL)).unwrap(),
        "{"
    );
}

#[test]
fn abandoned_atomic_staging_files_are_not_generation_records() {
    let fixture = Fixture::new();
    let path = fixture
        .store
        .root()
        .join("generations/.admission-interrupted");
    fs::write(&path, "{").unwrap();
    fixture.assert_restored();
    assert_eq!(admission::list(&fixture.store).unwrap().len(), 2);
    assert_eq!(fs::read_to_string(path).unwrap(), "{");
}

#[test]
fn aliased_lineage_is_refused_before_changing_supply() {
    for hard_link in [false, true] {
        let fixture = Fixture::new();
        let path = fixture.store.root().join("pins.jsonl");
        let original = fixture.store.root().join("original-pins.jsonl");
        fs::rename(&path, &original).unwrap();
        if hard_link {
            fs::hard_link(&original, &path).unwrap();
        } else {
            std::os::unix::fs::symlink(&original, &path).unwrap();
        }
        let locked = lock(&fixture.store).unwrap();
        assert!(matches!(
            activate(
                &fixture.store,
                Some(&fixture.previous),
                &fixture.activated()
            ),
            Err(AdmissionError::Io { .. })
        ));
        drop(locked);
        fixture.assert_restored();
        assert_eq!(
            fs::read_to_string(original).unwrap(),
            admission::pin_line(&fixture.previous).unwrap()
        );
    }
}

#[test]
fn invalid_backup_identities_never_become_recovery_write_paths() {
    for damage in ["version", "path", "identity", "duplicate", "unknown_field"] {
        let fixture = Fixture::new();
        let journal = Journal {
            version: 1,
            previous: Some(Backup::capture(&fixture.store, &fixture.previous.digest()).unwrap()),
            candidate: Backup::capture(&fixture.store, &fixture.candidate.digest()).unwrap(),
            pins: read_optional(&fixture.store.root().join("pins.jsonl")).unwrap(),
        };
        let mut value = serde_json::to_value(&journal).unwrap();
        match damage {
            "version" => value["version"] = 2.into(),
            "path" => value["candidate"]["generation"] = "../outside".into(),
            "identity" => {
                value["candidate"]["generation"] = fixture.previous.generation.clone().into();
            }
            "duplicate" => value["candidate"] = value["previous"].clone(),
            "unknown_field" => value["outside"] = true.into(),
            _ => unreachable!(),
        }
        let path = fixture.store.root().join(JOURNAL);
        let bytes = serde_json::to_vec(&value).unwrap();
        fs::write(&path, &bytes).unwrap();
        assert!(admission::list(&fixture.store).is_err(), "{damage}");
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(
            fs::read(admission::record_path(
                &fixture.store,
                &fixture.previous.digest()
            ))
            .unwrap(),
            serde_json::to_vec(&fixture.previous).unwrap()
        );
        assert_eq!(
            fs::read(admission::record_path(
                &fixture.store,
                &fixture.candidate.digest()
            ))
            .unwrap(),
            serde_json::to_vec(&fixture.candidate).unwrap()
        );
    }
}
