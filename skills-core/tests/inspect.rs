//! Inspection: the deterministic, model-free examination of package bytes.
//!
//! Inspection decides nothing about whether a skill is safe. It decides what a
//! reviewer must be shown, so the assertions here are about coverage and
//! determinism, not verdicts.

mod support;

use std::{fs, os::unix::fs::PermissionsExt, path::Path};

use louiselm_skills::{
    Policy,
    inspect::{ContentKind, FatalKind, Finding, FindingKind, Inspection},
};
use support::{Fixture, write_file};

fn inspect(fixture: &Fixture, candidate: &Path) -> Inspection {
    let (package, _) = fixture.capture(candidate).expect("capture succeeds");
    Inspection::run(&package, &Policy::embedded()).expect("inspection runs")
}

fn skill_file(candidate: &Path) {
    write_file(
        &candidate.join("SKILL.md"),
        "---\nname: demo\ndescription: A demonstration skill.\n---\n\nBody.\n",
    );
}

fn finding<'a>(inspection: &'a Inspection, kind: FindingKind, path: &str) -> &'a Finding {
    inspection
        .findings
        .iter()
        .find(|finding| finding.kind == kind && finding.path == path)
        .unwrap_or_else(|| {
            panic!(
                "no {kind:?} finding for '{path}' in {:?}",
                inspection
                    .findings
                    .iter()
                    .map(|finding| (finding.kind, finding.path.as_str()))
                    .collect::<Vec<_>>()
            )
        })
}

#[test]
fn hidden_and_bidi_characters_are_reported_with_their_class() {
    let fixture = Fixture::new();
    let candidate = fixture.candidate("candidate");
    skill_file(&candidate);
    write_file(
        &candidate.join("notes.md"),
        "run the\u{200b} installer\nthen \u{202e}reverse me\n",
    );

    let inspection = inspect(&fixture, &candidate);
    let hidden = finding(&inspection, FindingKind::UnicodeHidden, "notes.md");

    assert_eq!(hidden.occurrences, 2);
    assert!(
        hidden.samples.iter().any(|sample| sample.line == 1),
        "the zero-width space is located on its line: {:?}",
        hidden.samples,
    );
    assert!(
        hidden
            .samples
            .iter()
            .any(|sample| sample.detail == "invisible"),
        "the profile class is reported: {:?}",
        hidden.samples,
    );
    assert!(
        hidden
            .samples
            .iter()
            .any(|sample| sample.detail == "bidi_control"),
        "the bidi class is reported: {:?}",
        hidden.samples,
    );
}

#[test]
fn characters_imitating_ascii_are_reported_with_what_they_imitate() {
    let fixture = Fixture::new();
    let candidate = fixture.candidate("candidate");
    skill_file(&candidate);
    write_file(&candidate.join("notes.md"), "set the \u{430}pi_key value\n");

    let inspection = inspect(&fixture, &candidate);
    let confusable = finding(&inspection, FindingKind::UnicodeConfusable, "notes.md");

    assert_eq!(confusable.occurrences, 1);
    assert_eq!(confusable.samples[0].detail, "a");
}

#[test]
fn urls_credentials_decoders_and_payloads_are_reported() {
    let fixture = Fixture::new();
    let candidate = fixture.candidate("candidate");
    skill_file(&candidate);
    write_file(
        &candidate.join("scripts/setup.md"),
        &format!(
            "Fetch https://example.invalid/payload\nRead ~/.aws/credentials for the api_key\n\
             Then base64.b64decode('{}')\n",
            "QUJDREVG".repeat(40),
        ),
    );

    let inspection = inspect(&fixture, &candidate);

    let url = finding(&inspection, FindingKind::Url, "scripts/setup.md");
    assert!(url.samples[0].evidence.contains("https://example.invalid"));
    let credential = finding(
        &inspection,
        FindingKind::CredentialReference,
        "scripts/setup.md",
    );
    assert!(credential.occurrences >= 2, "both markers are reported");
    let payload = finding(&inspection, FindingKind::EncodedPayload, "scripts/setup.md");
    assert!(
        payload.occurrences >= 2,
        "decoder and run are both reported"
    );
}

#[test]
fn network_and_process_reach_are_reported_separately() {
    let fixture = Fixture::new();
    let candidate = fixture.candidate("candidate");
    skill_file(&candidate);
    write_file(
        &candidate.join("scripts/run.py"),
        "import requests\nimport subprocess\nsubprocess.run(['sh', '-c', 'echo hi'])\n",
    );

    let inspection = inspect(&fixture, &candidate);

    assert_eq!(
        finding(&inspection, FindingKind::NetworkImport, "scripts/run.py").occurrences,
        1,
    );
    assert!(finding(&inspection, FindingKind::ProcessImport, "scripts/run.py").occurrences >= 1);
}

#[test]
fn an_svg_that_does_something_rather_than_draw_is_reported() {
    let fixture = Fixture::new();
    let candidate = fixture.candidate("candidate");
    skill_file(&candidate);
    write_file(
        &candidate.join("assets/logo.svg"),
        "<svg xmlns=\"http://www.w3.org/2000/svg\" onload=\"go()\">\
         <script>fetch('https://example.invalid')</script></svg>\n",
    );

    let inspection = inspect(&fixture, &candidate);
    let behavior = finding(&inspection, FindingKind::SvgBehavior, "assets/logo.svg");

    assert!(
        behavior.occurrences >= 2,
        "script and handler are both reported"
    );
    assert_eq!(
        inspection
            .file("assets/logo.svg")
            .expect("the asset is inventoried")
            .kind,
        ContentKind::Svg,
    );
}

#[test]
fn declared_and_undeclared_binaries_are_told_apart() {
    let fixture = Fixture::new();
    let candidate = fixture.candidate("candidate");
    skill_file(&candidate);
    fs::create_dir_all(candidate.join("scripts/__pycache__")).expect("directory is creatable");
    fs::write(
        candidate.join("scripts/__pycache__/fetch.cpython-313.pyc"),
        [0xcb, 0x0d, 0x0d, 0x0a, 0x00, 0x00, 0x00, 0x00],
    )
    .expect("file is writable");
    fs::write(
        candidate.join("notes.md"),
        [0x7f, b'E', b'L', b'F', 0x02, 0x00, 0x00, 0x00],
    )
    .expect("file is writable");

    let inspection = inspect(&fixture, &candidate);

    finding(
        &inspection,
        FindingKind::DeclaredBinary,
        "scripts/__pycache__/fetch.cpython-313.pyc",
    );
    let undeclared = finding(&inspection, FindingKind::UndeclaredBinary, "notes.md");
    assert!(undeclared.samples[0].detail.contains("elf"));
}

#[test]
fn executables_and_images_are_inventoried() {
    let fixture = Fixture::new();
    let candidate = fixture.candidate("candidate");
    skill_file(&candidate);
    write_file(&candidate.join("scripts/run.sh"), "#!/bin/sh\necho hi\n");
    fs::set_permissions(
        candidate.join("scripts/run.sh"),
        fs::Permissions::from_mode(0o755),
    )
    .expect("mode is settable");
    fs::write(
        candidate.join("assets/icon.png"),
        [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a],
    )
    .ok();
    fs::create_dir_all(candidate.join("assets")).expect("directory is creatable");
    fs::write(
        candidate.join("assets/icon.png"),
        [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a],
    )
    .expect("file is writable");

    let inspection = inspect(&fixture, &candidate);

    finding(&inspection, FindingKind::Executable, "scripts/run.sh");
    finding(&inspection, FindingKind::Image, "assets/icon.png");
    assert_eq!(
        inspection.executables(),
        vec!["scripts/run.sh"],
        "the executable inventory is exactly the executable files",
    );
}

#[test]
fn content_that_contradicts_its_extension_is_reported() {
    let fixture = Fixture::new();
    let candidate = fixture.candidate("candidate");
    skill_file(&candidate);
    fs::create_dir_all(candidate.join("assets")).expect("directory is creatable");
    fs::write(
        candidate.join("assets/icon.png"),
        [0x7f, b'E', b'L', b'F', 0x02, 0x00, 0x00, 0x00],
    )
    .expect("file is writable");

    let inspection = inspect(&fixture, &candidate);
    let mismatch = finding(
        &inspection,
        FindingKind::ContentTypeMismatch,
        "assets/icon.png",
    );

    assert!(mismatch.samples[0].detail.contains("elf"));
}

#[test]
fn the_small_fatal_class_stops_a_package_from_being_reviewable() {
    let fixture = Fixture::new();

    let missing = fixture.candidate("missing");
    write_file(&missing.join("notes.md"), "no skill file here\n");
    let inspection = inspect(&fixture, &missing);
    assert!(inspection.is_fatal());
    assert_eq!(inspection.fatal[0].kind, FatalKind::SkillFileMissing);

    let malformed = fixture.candidate("malformed");
    write_file(&malformed.join("SKILL.md"), "no frontmatter at all\n");
    let inspection = inspect(&fixture, &malformed);
    assert_eq!(inspection.fatal[0].kind, FatalKind::SkillFileFrontmatter);

    let undescribed = fixture.candidate("undescribed");
    write_file(
        &undescribed.join("SKILL.md"),
        "---\nname: demo\n---\nbody\n",
    );
    let inspection = inspect(&fixture, &undescribed);
    assert_eq!(inspection.fatal[0].kind, FatalKind::SkillFileFrontmatter);

    let mangled = fixture.candidate("mangled");
    skill_file(&mangled);
    fs::write(mangled.join("notes.md"), [b'a', 0xff, 0xfe, b'b']).expect("file is writable");
    let inspection = inspect(&fixture, &mangled);
    assert_eq!(inspection.fatal[0].kind, FatalKind::TextNotUtf8);
}

#[test]
fn a_valid_skill_package_has_no_fatal_findings() {
    let fixture = Fixture::new();
    let candidate = fixture.candidate("candidate");
    skill_file(&candidate);

    let inspection = inspect(&fixture, &candidate);

    assert!(!inspection.is_fatal(), "unexpected: {:?}", inspection.fatal);
    assert_eq!(inspection.skill_name.as_deref(), Some("demo"));
    assert_eq!(
        inspection.skill_description.as_deref(),
        Some("A demonstration skill."),
    );
}

#[test]
fn evidence_never_carries_a_control_sequence_through() {
    let fixture = Fixture::new();
    let candidate = fixture.candidate("candidate");
    skill_file(&candidate);
    write_file(
        &candidate.join("notes.md"),
        "hostile \u{1b}[2J\u{1b}[H https://example.invalid \u{7} banner\n",
    );

    let inspection = inspect(&fixture, &candidate);

    for finding in &inspection.findings {
        for sample in &finding.samples {
            assert!(
                !sample.evidence.chars().any(char::is_control),
                "raw control character survived into evidence: {:?}",
                sample.evidence,
            );
            assert!(
                !sample.detail.chars().any(char::is_control),
                "raw control character survived into detail: {:?}",
                sample.detail,
            );
        }
        assert!(!finding.message.chars().any(char::is_control));
    }
    let escape = finding(&inspection, FindingKind::ControlSequence, "notes.md");
    assert!(escape.samples[0].evidence.contains("\\u{1b}"));
}

#[test]
fn a_scan_that_could_not_cover_a_whole_file_says_so() {
    let fixture = Fixture::new();
    let candidate = fixture.candidate("candidate");
    skill_file(&candidate);
    write_file(
        &candidate.join("big.md"),
        &"long line of prose\n".repeat(200),
    );

    let (package, _) = fixture.capture(&candidate).expect("capture succeeds");
    let tight = support::policy_with(&[(
        "\"max_text_scan_bytes\": 1048576",
        "\"max_text_scan_bytes\": 64",
    )]);
    let inspection = Inspection::run(&package, &tight).expect("inspection runs");

    let truncated = finding(&inspection, FindingKind::TruncatedScan, "big.md");
    assert!(truncated.samples[0].detail.contains("64"));
}

#[test]
fn inspection_is_deterministic_and_names_the_rules_it_applied() {
    let fixture = Fixture::new();
    let candidate = fixture.candidate("candidate");
    skill_file(&candidate);
    write_file(&candidate.join("notes.md"), "see https://example.invalid\n");

    let (package, _) = fixture.capture(&candidate).expect("capture succeeds");
    let policy = Policy::embedded();
    let first = Inspection::run(&package, &policy).expect("inspection runs");
    let second = Inspection::run(&package, &policy).expect("inspection runs again");

    assert_eq!(
        serde_json::to_string(&first).expect("inspection serializes"),
        serde_json::to_string(&second).expect("inspection serializes"),
    );
    assert_eq!(first.policy_digest, policy.digest().to_string());
    assert_eq!(first.policy_version, policy.document().version);
    assert_eq!(
        first.unicode_profile_version,
        policy.document().unicode.profile_version,
    );
    assert_eq!(first.package_digest, package.digest.to_string());
    assert!(
        first
            .findings
            .windows(2)
            .all(|pair| pair[0].id <= pair[1].id)
            || first.findings.windows(2).all(|pair| {
                (pair[0].path.as_str(), pair[0].kind as u8)
                    <= (pair[1].path.as_str(), pair[1].kind as u8)
            }),
        "findings are emitted in a stable order",
    );
}
