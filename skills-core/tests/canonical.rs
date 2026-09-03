//! Golden vectors and rejection cases for the canonical contract.
//!
//! The digests pinned here are the identity of every package this tool will
//! ever publish. A change to any of them is a change to what a signature over
//! a Skill Generation means, so they are asserted literally rather than
//! recomputed by the test.

use louiselm_skills::{
    Digest, Manifest, ManifestEntry, ManifestError, PathError,
    canonical::CanonicalPath,
    manifest::MANIFEST_SCHEMA,
    policy::{POLICY_SCHEMA, Policy},
};

const HELLO_SHA256: &str = "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03";
const GOLDEN_MANIFEST: &str = concat!(
    r#"{"schema":"louiselm.skills.manifest/1","entries":"#,
    r#"[{"path":"SKILL.md","executable":false,"size":6,"#,
    r#""sha256":"5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03"}]}"#,
);
const GOLDEN_DIGEST: &str = "87b20633dc609fd67d7cb7a1d771051dcbd935d8e915bb106577235d6cb9d2bd";

fn entry(path: &str) -> ManifestEntry {
    ManifestEntry {
        path: path.to_owned(),
        executable: false,
        size: 6,
        sha256: HELLO_SHA256.to_owned(),
    }
}

#[test]
fn manifest_serializes_to_its_pinned_canonical_bytes() {
    let manifest = Manifest::new(vec![entry("SKILL.md")], false).expect("manifest is admissible");

    assert_eq!(
        String::from_utf8(manifest.canonical_bytes()).expect("canonical bytes are UTF-8"),
        GOLDEN_MANIFEST,
    );
    assert_eq!(manifest.digest().hex(), GOLDEN_DIGEST);
    assert_eq!(
        manifest.digest().to_string(),
        format!("sha256:{GOLDEN_DIGEST}")
    );
    assert_eq!(
        manifest.digest().directory_name(),
        format!("sha256-{GOLDEN_DIGEST}"),
    );
}

#[test]
fn manifest_digest_ignores_the_order_entries_were_discovered_in() {
    let discovered = Manifest::new(
        vec![entry("z/last.md"), entry("SKILL.md"), entry("a/first.md")],
        false,
    )
    .expect("manifest is admissible");
    let rediscovered = Manifest::new(
        vec![entry("a/first.md"), entry("z/last.md"), entry("SKILL.md")],
        false,
    )
    .expect("manifest is admissible");

    assert_eq!(discovered.digest(), rediscovered.digest());
    assert_eq!(
        discovered
            .entries
            .iter()
            .map(|entry| entry.path.as_str())
            .collect::<Vec<_>>(),
        vec!["SKILL.md", "a/first.md", "z/last.md"],
    );
}

#[test]
fn manifest_parsing_rejects_bytes_that_are_not_in_canonical_form() {
    let spaced = format!("{GOLDEN_MANIFEST}\n");
    assert!(matches!(
        Manifest::parse(spaced.as_bytes(), false),
        Err(ManifestError::NonCanonical),
    ));

    let reordered = GOLDEN_MANIFEST.replace(
        r#"{"path":"SKILL.md","executable":false"#,
        r#"{"executable":false,"path":"SKILL.md""#,
    );
    assert!(matches!(
        Manifest::parse(reordered.as_bytes(), false),
        Err(ManifestError::NonCanonical),
    ));

    assert_eq!(
        Manifest::parse(GOLDEN_MANIFEST.as_bytes(), false).expect("golden parses"),
        Manifest::new(vec![entry("SKILL.md")], false).expect("manifest is admissible"),
    );
}

#[test]
fn manifest_rejects_duplicate_and_colliding_paths() {
    assert!(matches!(
        Manifest::new(vec![entry("SKILL.md"), entry("SKILL.md")], false),
        Err(ManifestError::DuplicatePath(path)) if path == "SKILL.md",
    ));
    assert!(matches!(
        Manifest::new(vec![entry("SKILL.md"), entry("skill.md")], false),
        Err(ManifestError::CollidingPaths { .. }),
    ));
}

#[test]
fn manifest_rejects_an_unknown_schema() {
    let foreign = GOLDEN_MANIFEST.replace(MANIFEST_SCHEMA, "louiselm.skills.manifest/99");

    assert!(matches!(
        Manifest::parse(foreign.as_bytes(), false),
        Err(ManifestError::UnsupportedSchema(schema)) if schema == "louiselm.skills.manifest/99",
    ));
}

#[test]
fn canonical_paths_reject_everything_that_could_escape_a_package() {
    let rejected = [
        ("", PathError::Empty),
        ("/etc/passwd", PathError::Absolute("/etc/passwd".to_owned())),
        (
            "../outside.md",
            PathError::RelativeComponent("../outside.md".to_owned()),
        ),
        (
            "a/./b.md",
            PathError::RelativeComponent("a/./b.md".to_owned()),
        ),
        ("a//b.md", PathError::EmptyComponent("a//b.md".to_owned())),
        (
            "trailing/",
            PathError::EmptyComponent("trailing/".to_owned()),
        ),
        (
            "hidden\u{7}.md",
            PathError::ControlCharacter("hidden\u{7}.md".to_owned()),
        ),
        ("réadme.md", PathError::NonAscii("réadme.md".to_owned())),
    ];

    for (raw, expected) in rejected {
        assert_eq!(
            CanonicalPath::parse(raw, false),
            Err(expected),
            "path '{raw}' must be rejected",
        );
    }

    assert!(CanonicalPath::parse("réadme.md", true).is_ok());
    assert_eq!(
        CanonicalPath::parse("scripts/run.sh", false)
            .expect("path is admissible")
            .extension()
            .as_deref(),
        Some("sh"),
    );
}

#[test]
fn digests_parse_from_every_spelling_the_tool_prints() {
    let digest = Digest::parse(GOLDEN_DIGEST).expect("bare hex parses");

    assert_eq!(
        Digest::parse(&format!("sha256:{GOLDEN_DIGEST}")).expect("prefixed parses"),
        digest,
    );
    assert_eq!(
        Digest::parse(&format!("sha256-{GOLDEN_DIGEST}")).expect("directory name parses"),
        digest,
    );
    assert!(Digest::parse("sha256:not-a-digest").is_err());
    assert!(Digest::parse(&GOLDEN_DIGEST.to_uppercase()).is_err());
}

#[test]
fn the_embedded_policy_is_content_addressed_and_versioned() {
    let policy = Policy::embedded();

    assert_eq!(policy.document().schema, POLICY_SCHEMA);
    assert_eq!(policy.document().version, "2026-09-03.1");
    assert_eq!(policy.document().unicode.profile_version, "2026-09-03.1");
    assert!(!policy.allows_non_ascii_paths());
    assert_eq!(
        policy.digest(),
        Policy::embedded().digest(),
        "the embedded policy digest is stable across loads",
    );
}
