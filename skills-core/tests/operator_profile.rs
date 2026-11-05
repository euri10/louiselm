//! Behavioral coverage for recognizing Operator profile packages.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]

//! An Operator profile is recognized from its bytes alone: a root
//! `PROFILE.md` declaring `type: louiselm/operator-profile`. It is always
//! loaded into every Agent, so it is held to stricter rules than a skill: one
//! small file, and no credential references at all.

mod support;

use louiselm_skills::{
    Policy,
    dossier::{Dossier, DossierRequest},
    inspect::{FatalKind, INSPECTION_SCHEMA, Inspection, MAX_PROFILE_BYTES, PackageKind},
    render,
};
use support::{Fixture, write_file};

const PROFILE: &str = "---\ntype: louiselm/operator-profile\ntitle: Operator profile\n---\n\nUse plain English.\nDefault Git host: example.org, namespace team.\n";

fn inspect(files: &[(&str, &str)]) -> (Fixture, Inspection) {
    let fixture = Fixture::new();
    let candidate = fixture.candidate("profile");
    for (path, body) in files {
        write_file(&candidate.join(path), body);
    }
    let (package, _) = fixture.capture(&candidate).unwrap();
    let inspection = Inspection::run(&package, &Policy::embedded()).unwrap();
    (fixture, inspection)
}

fn fatal_kinds(inspection: &Inspection) -> Vec<FatalKind> {
    inspection.fatal.iter().map(|fatal| fatal.kind).collect()
}

#[test]
fn a_declared_profile_is_recognized_and_reviewable() {
    let (_, inspection) = inspect(&[("PROFILE.md", PROFILE)]);

    assert_eq!(inspection.package_kind, PackageKind::OperatorProfile);
    assert_eq!(fatal_kinds(&inspection), Vec::new());
    assert_eq!(INSPECTION_SCHEMA, "louiselm.skills.inspection/2");
}

#[test]
fn a_skill_stays_a_skill() {
    let (_, inspection) = inspect(&[(
        "SKILL.md",
        "---\nname: demo\ndescription: Demo.\n---\n\nBody.\n",
    )]);

    assert_eq!(inspection.package_kind, PackageKind::Skill);
    assert_eq!(fatal_kinds(&inspection), Vec::new());
}

#[test]
fn a_profile_without_the_exact_type_is_still_a_profile_but_fatal() {
    for body in [
        "---\ntitle: no type\n---\nBody.\n",
        "---\ntype: louiselm/operator-profile-v2\n---\nBody.\n",
        "no frontmatter at all\n",
    ] {
        let (_, inspection) = inspect(&[("PROFILE.md", body)]);
        assert_eq!(
            inspection.package_kind,
            PackageKind::OperatorProfile,
            "recognition is conservative so refusals cannot be dodged"
        );
        assert_eq!(
            fatal_kinds(&inspection),
            vec![FatalKind::ProfileType],
            "{body}"
        );
    }
}

#[test]
fn a_profile_over_the_size_cap_is_fatal() {
    let large = format!("{PROFILE}{}", "x".repeat(MAX_PROFILE_BYTES));
    let (_, inspection) = inspect(&[("PROFILE.md", &large)]);

    assert_eq!(fatal_kinds(&inspection), vec![FatalKind::ProfileTooLarge]);
    assert_eq!(MAX_PROFILE_BYTES, 8 * 1024);
}

#[test]
fn a_profile_carries_exactly_one_file() {
    let (_, inspection) = inspect(&[("PROFILE.md", PROFILE), ("notes/extra.md", "more\n")]);

    assert_eq!(fatal_kinds(&inspection), vec![FatalKind::ProfileExtraFile]);
}

#[test]
fn a_package_cannot_be_both_a_skill_and_a_profile() {
    let (_, inspection) = inspect(&[
        ("PROFILE.md", PROFILE),
        (
            "SKILL.md",
            "---\nname: demo\ndescription: Demo.\n---\n\nBody.\n",
        ),
    ]);

    assert_eq!(inspection.package_kind, PackageKind::OperatorProfile);
    assert!(
        fatal_kinds(&inspection).contains(&FatalKind::PackageKindAmbiguous),
        "{:?}",
        inspection.fatal
    );
}

#[test]
fn a_credential_reference_in_a_profile_is_fatal() {
    let body = format!("{PROFILE}Use the key in ~/.ssh/id_ed25519 for pushes.\n");
    let (_, inspection) = inspect(&[("PROFILE.md", &body)]);

    assert_eq!(fatal_kinds(&inspection), vec![FatalKind::ProfileCredential]);
}

#[test]
fn the_dossier_labels_a_profile_as_private_always_on_supply() {
    let (fixture, inspection) = inspect(&[("PROFILE.md", PROFILE)]);
    let digest = louiselm_skills::Digest::parse(&inspection.package_digest).unwrap();

    let dossier = Dossier::build(
        &fixture.store(),
        &Policy::embedded(),
        &DossierRequest::new(&digest),
    )
    .unwrap();

    assert_eq!(
        dossier.inspection.package_kind,
        PackageKind::OperatorProfile
    );
    let human = render::human(&dossier);
    assert!(
        human
            .contains("Operator profile: always loaded into every Agent; private, never published"),
        "{human}"
    );
    let robot = serde_json::to_value(&dossier).unwrap();
    assert_eq!(robot["inspection"]["package_kind"], "operator_profile");
}
