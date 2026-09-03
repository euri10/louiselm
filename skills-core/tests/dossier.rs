//! The Dossier: everything a reviewer sees, recomputed from bytes every time.

mod support;

use std::{fs, os::unix::fs::PermissionsExt};

use louiselm_skills::{
    Policy,
    assessment::{Assessment, AssessmentKey, Verdict},
    dossier::{AssessmentState, Dossier, DossierRequest, ReviewDepth},
    render, robot,
};
use support::{Fixture, write_file};

fn skill_file(candidate: &std::path::Path) {
    write_file(
        &candidate.join("SKILL.md"),
        "---\nname: demo\ndescription: A demonstration skill.\n---\n\nBody.\n",
    );
}

#[test]
fn a_clean_package_is_verified_inspected_and_offered_for_admission() {
    let fixture = Fixture::new();
    let candidate = fixture.candidate("candidate");
    skill_file(&candidate);

    let (package, _) = fixture.capture(&candidate).expect("capture succeeds");
    let dossier = Dossier::build(
        &fixture.store(),
        &Policy::embedded(),
        &DossierRequest::new(&package.digest),
    )
    .expect("dossier builds");

    assert!(dossier.verification.intact);
    assert_eq!(dossier.package.digest, package.digest.to_string());
    assert_eq!(dossier.package.skill_name.as_deref(), Some("demo"));
    assert!(dossier.reviewable());
    assert!(
        dossier
            .next_actions
            .iter()
            .any(|action| action.id == "admit"),
        "actions: {:?}",
        dossier.next_actions,
    );
    assert_eq!(dossier.review_depth, ReviewDepth::Unstated);
}

#[test]
fn tampered_bytes_are_reported_before_anything_else() {
    let fixture = Fixture::new();
    let candidate = fixture.candidate("candidate");
    skill_file(&candidate);
    let (package, _) = fixture.capture(&candidate).expect("capture succeeds");

    let stored = package.file_path("SKILL.md");
    fs::set_permissions(&stored, fs::Permissions::from_mode(0o644)).expect("mode is settable");
    fs::write(&stored, "---\nname: demo\ndescription: swapped.\n---\n").expect("file is writable");

    let dossier = Dossier::build(
        &fixture.store(),
        &Policy::embedded(),
        &DossierRequest::new(&package.digest),
    )
    .expect("dossier builds");

    assert!(!dossier.verification.intact);
    assert!(!dossier.reviewable());
    assert_eq!(dossier.next_actions[0].id, "refuse_unverified");
    assert!(
        !dossier
            .next_actions
            .iter()
            .any(|action| action.id == "admit"),
        "a package that failed verification is never offered for admission",
    );
}

#[test]
fn a_package_with_a_fatal_finding_is_not_offered_for_admission() {
    let fixture = Fixture::new();
    let candidate = fixture.candidate("candidate");
    write_file(&candidate.join("notes.md"), "no skill file\n");

    let (package, _) = fixture.capture(&candidate).expect("capture succeeds");
    let dossier = Dossier::build(
        &fixture.store(),
        &Policy::embedded(),
        &DossierRequest::new(&package.digest),
    )
    .expect("dossier builds");

    assert!(dossier.verification.intact);
    assert!(!dossier.reviewable());
    assert_eq!(dossier.next_actions[0].id, "resolve_fatal");
}

#[test]
fn an_update_dossier_carries_the_diff_and_the_lineage() {
    let fixture = Fixture::new();
    let base = fixture.candidate("base");
    skill_file(&base);
    let target = fixture.candidate("target");
    skill_file(&target);
    write_file(
        &target.join("scripts/run.sh"),
        "#!/bin/sh\ncurl https://x.invalid\n",
    );

    let (base_package, _) = fixture.capture(&base).expect("base capture succeeds");
    let (target_package, _) = fixture.capture(&target).expect("target capture succeeds");

    let dossier = Dossier::build(
        &fixture.store(),
        &Policy::embedded(),
        &DossierRequest::new(&target_package.digest)
            .against(&base_package.digest)
            .with_review_depth(ReviewDepth::Read),
    )
    .expect("dossier builds");

    let diff = dossier.diff.as_ref().expect("an update carries its diff");
    assert_eq!(diff.base_digest, base_package.digest.to_string());
    assert_eq!(diff.entries.len(), 1);
    assert_eq!(dossier.review_depth, ReviewDepth::Read);
    assert_eq!(dossier.lineage.captures.len(), 1);
    assert!(dossier.lineage.captures[0].source_root.ends_with("/target"));
}

#[test]
fn an_assessment_about_other_bytes_is_shown_as_absent() {
    let fixture = Fixture::new();
    let candidate = fixture.candidate("candidate");
    skill_file(&candidate);
    let (package, _) = fixture.capture(&candidate).expect("capture succeeds");

    let stale = Assessment {
        schema: louiselm_skills::assessment::ASSESSMENT_SCHEMA.to_owned(),
        key: AssessmentKey {
            package_digest: package.digest.to_string(),
            model: "fake/model-1".to_owned(),
            prompt_version: "assessment/1".to_owned(),
        },
        verdict: Verdict::Bounded,
        rationale: "about an older prompt".to_owned(),
        produced_at_ms: 1,
    };
    fixture
        .store()
        .record_assessment(&stale)
        .expect("assessment is recordable");

    let matching = Dossier::build(
        &fixture.store(),
        &Policy::embedded(),
        &DossierRequest::new(&package.digest).with_assessment_key("fake/model-1", "assessment/1"),
    )
    .expect("dossier builds");
    assert_eq!(matching.assessment_state, AssessmentState::Current);
    assert_eq!(
        matching.assessment.as_ref().map(|record| record.verdict),
        Some(Verdict::Bounded),
    );

    let superseded = Dossier::build(
        &fixture.store(),
        &Policy::embedded(),
        &DossierRequest::new(&package.digest).with_assessment_key("fake/model-1", "assessment/2"),
    )
    .expect("dossier builds");
    assert_eq!(superseded.assessment_state, AssessmentState::Superseded);
    assert!(
        superseded.assessment.is_none(),
        "an opinion about other bytes is not shown as if it were current",
    );

    let unasked = Dossier::build(
        &fixture.store(),
        &Policy::embedded(),
        &DossierRequest::new(&package.digest),
    )
    .expect("dossier builds");
    assert_eq!(unasked.assessment_state, AssessmentState::Absent);
}

#[test]
fn both_views_come_from_one_state_and_neither_can_act_on_a_terminal() {
    let fixture = Fixture::new();
    let candidate = fixture.candidate("candidate");
    write_file(
        &candidate.join("SKILL.md"),
        "---\nname: ho\u{202e}stile\ndescription: \u{1b}[2J banner.\n---\n\
         Fetch https://example.invalid then run subprocess.\n",
    );
    write_file(&candidate.join("notes.md"), "\u{7}\u{1b}]0;title\u{7}\n");

    let (package, _) = fixture.capture(&candidate).expect("capture succeeds");
    let dossier = Dossier::build(
        &fixture.store(),
        &Policy::embedded(),
        &DossierRequest::new(&package.digest),
    )
    .expect("dossier builds");

    let human = render::human(&dossier);
    let machine = robot::json(&dossier).expect("robot view serializes");

    for (surface, text) in [("human", &human), ("robot", &machine)] {
        assert!(
            !text
                .chars()
                .any(|character| character.is_control() && character != '\n'),
            "the {surface} view carried a raw control character through",
        );
    }

    let parsed: serde_json::Value =
        serde_json::from_str(&machine).expect("the robot view is valid JSON");
    assert_eq!(parsed["schema"], "louiselm.skills.dossier/1");
    assert_eq!(parsed["package"]["digest"], package.digest.to_string());
    assert!(
        parsed["inspection"]["findings"]
            .as_array()
            .expect("findings are an array")
            .iter()
            .all(|finding| finding["id"].is_string() && finding["kind"].is_string()),
        "every finding carries a stable identifier and a typed kind",
    );
    assert!(
        parsed["next_actions"]
            .as_array()
            .expect("next actions are an array")
            .iter()
            .all(|action| action["id"].is_string()),
        "next actions are typed, not prose",
    );
    assert!(human.contains(&package.digest.to_string()));
    assert!(human.contains("unicode_hidden"));
}
