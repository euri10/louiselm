//! Behavioral coverage for Endorsement focus beside the Dossier.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]

//! Focus frames review; it never changes what Admission accepts. A base earns
//! the focus label only when it is admitted or validly endorsed by a trusted
//! endorser; lineage only chooses among trusted bases.

mod support;

use louiselm_skills::{
    Digest, Policy, ReviewDepth, SshKeygenSigner, TrustStore,
    dossier::{Dossier, DossierRequest},
    endorsement::{
        self, Batch, EndorsedPackage, EndorserRole, Signing,
        focus::{Basis, FocusContext},
        import, local,
        trust::{EndorserEntry, MAX_LIFETIME_MS},
    },
    sshsig::SkPolicy,
};
use support::{Fixture, SshKey, write_file};

const NOW: u64 = 1_800_000_000_000;

struct World {
    fixture: Fixture,
    publisher: SshKey,
}

impl World {
    fn new() -> Self {
        let fixture = Fixture::new();
        let primary = SshKey::generate(&fixture, "primary");
        let release = SshKey::generate(&fixture, "release");
        let publisher = SshKey::generate(&fixture, "publisher");
        let store = fixture.store();
        TrustStore::bootstrap(
            &store,
            "louiselm/skills",
            &primary.public_key(),
            &release.public_key(),
            SkPolicy::none(),
            NOW,
        )
        .unwrap();
        local::add(
            &store,
            &EndorserEntry {
                label: "Publisher".into(),
                max_lifetime_ms: MAX_LIFETIME_MS,
                public_key: publisher.public_key(),
                roles: vec![EndorserRole::Publisher],
                sk_policy: SkPolicy::none(),
            },
            &SshKeygenSigner::new(primary.private_key_path()),
            NOW,
        )
        .unwrap();
        Self { fixture, publisher }
    }

    fn capture(&self, relative: &str, body: &str) -> Digest {
        let candidate = self.fixture.candidate(relative);
        write_file(
            &candidate.join("SKILL.md"),
            &format!("---\nname: demo\ndescription: Demo.\n---\n\n{body}\n"),
        );
        self.fixture.capture(&candidate).unwrap().0.digest
    }

    fn publish(&self, key: &SshKey, batch: &Batch, expires_at_ms: u64) {
        for bytes in endorsement::sign_batch(
            &Signing {
                signer_role: EndorserRole::Publisher,
                signer_key: key.public_key(),
                issued_at_ms: NOW - 1,
                expires_at_ms,
            },
            batch,
            &SshKeygenSigner::new(key.private_key_path()),
        )
        .unwrap()
        {
            import::import_bytes(&self.fixture.store(), &bytes).unwrap();
        }
    }

    fn endorse(&self, key: &SshKey, package: &Digest) {
        self.publish(
            key,
            &Batch::Endorsements(vec![EndorsedPackage {
                package_digest: package.clone(),
                policy_digest: Policy::embedded().digest().clone(),
                dossier_digest: None,
                assessment: None,
                review_depth: ReviewDepth::Read,
                assumptions: Vec::new(),
            }]),
            NOW + 1_000,
        );
    }

    fn focus(
        &self,
        admitted: &[Digest],
        package: &Digest,
        against: Option<&Digest>,
    ) -> endorsement::focus::Focus {
        let store = self.fixture.store();
        FocusContext::load(&store, admitted, NOW)
            .unwrap()
            .evaluate(&store, &Policy::embedded(), package, against)
            .unwrap()
    }
}

#[test]
fn exact_bytes_endorsed_by_a_trusted_publisher_are_focused_and_relied_on() {
    let world = World::new();
    let package = world.capture("skills/demo", "v1");
    world.endorse(&world.publisher, &package);

    let focus = world.focus(&[], &package, None);

    assert_eq!(focus.exact.len(), 1);
    assert_eq!(focus.exact[0].endorser, "Publisher");
    assert_eq!(focus.relied_on, vec![focus.exact[0].record.clone()]);
    assert!(focus.base.is_none());
    assert!(focus.evidence[0].trusted);
}

#[test]
fn an_untrusted_signer_is_shown_but_never_focuses() {
    let world = World::new();
    let stranger = SshKey::generate(&world.fixture, "stranger");
    let package = world.capture("skills/demo", "v1");
    world.endorse(&stranger, &package);

    let focus = world.focus(&[], &package, None);

    assert!(!focus.is_focused());
    assert_eq!(focus.relied_on, Vec::<String>::new());
    assert_eq!(focus.evidence.len(), 1);
    assert_eq!(focus.evidence[0].trust, "untrusted: unknown signer");
}

#[test]
fn an_update_is_framed_against_the_endorsed_earlier_capture() {
    let world = World::new();
    let first = world.capture("skills/demo", "v1");
    world.endorse(&world.publisher, &first);
    let second = world.capture("skills/demo", "v2 changes this line");

    let focus = world.focus(&[], &second, None);

    let base = focus
        .base
        .expect("the endorsed predecessor frames the review");
    assert_eq!(base.package_digest, first.to_string());
    assert!(matches!(base.basis, Basis::Endorsed { ref endorser, .. } if endorser == "Publisher"));
    assert!(!base.diff.entries.is_empty(), "every changed byte is shown");
    assert_eq!(focus.relied_on.len(), 1);
    assert_eq!(focus.exact, Vec::new());
}

#[test]
fn a_withdrawal_cancels_its_signers_focus_even_after_it_expires() {
    let world = World::new();
    let first = world.capture("skills/demo", "v1");
    world.endorse(&world.publisher, &first);
    world.publish(
        &world.publisher,
        &Batch::Withdrawals(vec![first.clone()]),
        NOW,
    );
    let second = world.capture("skills/demo", "v2");

    assert!(
        !world.focus(&[], &first, None).is_focused(),
        "an expired withdrawal must not bring focus back"
    );
    assert!(world.focus(&[], &second, None).base.is_none());
    let withdrawn = world.focus(&[], &first, None).evidence;
    assert!(withdrawn.iter().any(|evidence| evidence.withdrawn));
}

#[test]
fn another_signers_withdrawal_does_not_cancel() {
    let world = World::new();
    let stranger = SshKey::generate(&world.fixture, "stranger");
    let package = world.capture("skills/demo", "v1");
    world.endorse(&world.publisher, &package);
    world.publish(
        &stranger,
        &Batch::Withdrawals(vec![package.clone()]),
        NOW + 1_000,
    );

    assert_eq!(world.focus(&[], &package, None).exact.len(), 1);
}

#[test]
fn an_admitted_earlier_capture_is_a_trusted_base() {
    let world = World::new();
    let first = world.capture("skills/demo", "v1");
    let second = world.capture("skills/demo", "v2");

    let focus = world.focus(std::slice::from_ref(&first), &second, None);

    let base = focus.base.unwrap();
    assert_eq!(base.package_digest, first.to_string());
    assert_eq!(base.basis, Basis::Admitted {});
    assert_eq!(focus.relied_on, Vec::<String>::new());
}

#[test]
fn lineage_to_an_untrusted_capture_earns_no_focus() {
    let world = World::new();
    let _untrusted = world.capture("skills/demo", "v1");
    let second = world.capture("skills/demo", "v2");

    assert!(world.focus(&[], &second, None).base.is_none());
}

#[test]
fn an_explicit_base_is_labelled_only_when_trusted() {
    let world = World::new();
    let trusted = world.capture("one/demo", "v1");
    let untrusted = world.capture("two/demo", "other");
    let target = world.capture("three/demo", "v2");
    world.endorse(&world.publisher, &trusted);

    assert!(world.focus(&[], &target, Some(&untrusted)).base.is_none());
    assert_eq!(
        world
            .focus(&[], &target, Some(&trusted))
            .base
            .unwrap()
            .package_digest,
        trusted.to_string()
    );
}

#[test]
fn focus_never_changes_the_portable_dossier() {
    let world = World::new();
    let package = world.capture("skills/demo", "v1");
    let store = world.fixture.store();
    let before = Dossier::build(&store, &Policy::embedded(), &DossierRequest::new(&package))
        .unwrap()
        .portable_digest();
    world.endorse(&world.publisher, &package);
    assert!(world.focus(&[], &package, None).is_focused());

    let after = Dossier::build(&store, &Policy::embedded(), &DossierRequest::new(&package))
        .unwrap()
        .portable_digest();
    assert_eq!(before, after);
}
