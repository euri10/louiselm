//! Property sweeps over the parsers hostile input reaches first.
//!
//! These are deterministic: the generator is a fixed-seed LCG, so a failure
//! reproduces exactly rather than "sometimes on CI". They assert the two
//! properties that matter for a trusted parser — it never panics, and it never
//! accepts something it would then describe wrongly.

mod support;

use louiselm_skills::{
    Digest, Manifest, ManifestEntry, Policy, canonical::CanonicalPath, dossier::Dossier,
    dossier::DossierRequest, robot, scan,
};
use support::{Fixture, write_file};

/// A fixed-seed linear congruential generator, so every run sees one corpus.
struct Corpus(u64);

impl Corpus {
    fn new() -> Self {
        Self(0x2545_F491_4F6C_DD1D)
    }

    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }

    /// Builds a string from an alphabet chosen to hit every rejection path.
    fn text(&mut self, length: usize) -> String {
        const ALPHABET: [char; 24] = [
            'a', 'B', '9', '.', '/', '\\', '-', '_', ' ', '\t', '\n', '\0', '\u{7}', '\u{1b}',
            '\u{200b}', '\u{202e}', 'é', 'а', '"', '\'', ':', '~', '%', '*',
        ];
        (0..length)
            .map(|_| ALPHABET[self.below(ALPHABET.len())])
            .collect()
    }
}

#[test]
fn path_parsing_never_panics_and_never_accepts_what_it_forbids() {
    let mut corpus = Corpus::new();

    for _ in 0..20_000 {
        let length = corpus.below(24);
        let raw = corpus.text(length);

        for allow_non_ascii in [false, true] {
            let Ok(path) = CanonicalPath::parse(&raw, allow_non_ascii) else {
                continue;
            };
            assert_eq!(path.as_str(), raw, "an accepted path is unchanged");
            assert!(!path.as_str().is_empty());
            assert!(!path.as_str().starts_with('/'));
            assert!(
                !path
                    .as_str()
                    .split('/')
                    .any(|component| component.is_empty() || component == "." || component == ".."),
                "accepted '{raw}' has a component that could escape the package",
            );
            assert!(!path.as_str().chars().any(char::is_control));
            if !allow_non_ascii {
                assert!(path.as_str().is_ascii());
            }
        }
    }
}

#[test]
fn manifest_parsing_never_panics_and_only_accepts_canonical_bytes() {
    let mut corpus = Corpus::new();
    let golden = Manifest::new(
        vec![ManifestEntry {
            path: "SKILL.md".to_owned(),
            executable: false,
            size: 6,
            sha256: Digest::of(b"hello\n").hex().to_owned(),
        }],
        false,
    )
    .expect("golden manifest is admissible")
    .canonical_bytes();

    for _ in 0..20_000 {
        let mut bytes = golden.clone();
        let mutations = 1 + corpus.below(3);
        for _ in 0..mutations {
            let index = corpus.below(bytes.len());
            bytes[index] = (corpus.next() % 256) as u8;
        }

        if let Ok(manifest) = Manifest::parse(&bytes, false) {
            assert_eq!(
                manifest.canonical_bytes(),
                bytes,
                "an accepted manifest re-serializes to exactly the bytes it came from",
            );
            assert_eq!(manifest.digest(), Digest::of(&bytes));
        }
    }

    for _ in 0..5_000 {
        let length = corpus.below(64);
        let raw = corpus.text(length);
        let _ = Manifest::parse(raw.as_bytes(), false);
    }
}

#[test]
fn escaping_leaves_no_control_character_behind_for_any_input() {
    let mut corpus = Corpus::new();

    for _ in 0..20_000 {
        let length = corpus.below(40);
        let raw = corpus.text(length);
        let escaped = scan::escape(&raw);

        assert!(
            !escaped.chars().any(char::is_control),
            "escaping left a control character in {escaped:?}",
        );
        assert!(escaped.is_ascii(), "escaping left a non-ASCII byte");
    }
}

#[test]
fn no_generated_package_produces_robot_output_that_can_act_on_a_terminal() {
    let mut corpus = Corpus::new();
    let fixture = Fixture::new();
    let policy = Policy::embedded();

    for round in 0..40 {
        let candidate = fixture.candidate(&format!("candidate-{round}"));
        write_file(
            &candidate.join("SKILL.md"),
            &format!(
                "---\nname: demo\ndescription: {}\n---\n{}\n",
                corpus.text(20).replace(['\n', '\0'], " "),
                corpus.text(200),
            ),
        );
        write_file(&candidate.join("notes.md"), &corpus.text(300));

        let Ok((package, _)) = fixture.capture_with(&candidate, &policy) else {
            // Generated trees legitimately hit path and encoding refusals; the
            // property under test is about what reaches output, not what is
            // admitted.
            continue;
        };
        let dossier = Dossier::build(
            &fixture.store(),
            &policy,
            &DossierRequest::new(&package.digest),
        )
        .expect("dossier builds");
        let machine = robot::json(&dossier).expect("robot view serializes");

        for character in machine.chars() {
            assert!(
                !character.is_control() || character == '\n',
                "robot output carried {character:?} through for round {round}",
            );
        }
    }
}
