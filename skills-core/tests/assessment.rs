//! Assessment: a model's advisory opinion, with no authority and no reach.

mod support;

use louiselm_skills::assessment::{
    Assessment, AssessmentError, AssessmentKey, AssessmentRequest, Assessor, CapabilityEnvelope,
    Verdict, run,
};
use support::{Fixture, write_file};

struct FixedAssessor {
    verdict: Verdict,
}

impl Assessor for FixedAssessor {
    fn assess(&self, request: &AssessmentRequest) -> Result<(Verdict, String), AssessmentError> {
        assert!(
            request.envelope.is_empty(),
            "an assessor must never be handed capabilities",
        );
        Ok((self.verdict, "fixed opinion".to_owned()))
    }
}

struct ReachingAssessor;

impl Assessor for ReachingAssessor {
    fn assess(&self, _request: &AssessmentRequest) -> Result<(Verdict, String), AssessmentError> {
        unreachable!("an assessor with capabilities is never called");
    }
}

fn key(package_digest: &str) -> AssessmentKey {
    AssessmentKey {
        package_digest: package_digest.to_owned(),
        model: "fake/model-1".to_owned(),
        prompt_version: "assessment/1".to_owned(),
    }
}

#[test]
fn an_assessment_is_recorded_against_the_exact_bytes_model_and_prompt() {
    let fixture = Fixture::new();
    let candidate = fixture.candidate("candidate");
    write_file(&candidate.join("SKILL.md"), "body\n");
    let (package, _) = fixture.capture(&candidate).expect("capture succeeds");
    let key = key(&package.digest.to_string());

    let assessment = run(
        &FixedAssessor {
            verdict: Verdict::Unbounded,
        },
        &AssessmentRequest {
            key: key.clone(),
            envelope: CapabilityEnvelope::empty(),
            excerpt: "SKILL.md".to_owned(),
        },
        1_756_800_000_000,
    )
    .expect("assessment runs");

    assert_eq!(assessment.verdict, Verdict::Unbounded);
    assert_eq!(assessment.key, key);

    fixture
        .store()
        .record_assessment(&assessment)
        .expect("assessment is recordable");
    let loaded = fixture
        .store()
        .assessment(&package.digest)
        .expect("assessment is readable")
        .expect("an assessment was recorded");
    assert_eq!(loaded, assessment);
    assert_eq!(loaded.current_for(&key), Some(&loaded));
}

#[test]
fn an_assessment_keyed_to_anything_else_is_treated_as_absent() {
    let fixture = Fixture::new();
    let candidate = fixture.candidate("candidate");
    write_file(&candidate.join("SKILL.md"), "body\n");
    let (package, _) = fixture.capture(&candidate).expect("capture succeeds");
    let recorded = Assessment {
        schema: louiselm_skills::assessment::ASSESSMENT_SCHEMA.to_owned(),
        key: key(&package.digest.to_string()),
        verdict: Verdict::Bounded,
        rationale: "stale opinion".to_owned(),
        produced_at_ms: 1_756_800_000_000,
    };

    let other_model = AssessmentKey {
        model: "fake/model-2".to_owned(),
        ..key(&package.digest.to_string())
    };
    let other_prompt = AssessmentKey {
        prompt_version: "assessment/2".to_owned(),
        ..key(&package.digest.to_string())
    };
    let other_package = AssessmentKey {
        package_digest: louiselm_skills::Digest::of(b"other").to_string(),
        ..key(&package.digest.to_string())
    };

    assert_eq!(recorded.current_for(&other_model), None);
    assert_eq!(recorded.current_for(&other_prompt), None);
    assert_eq!(recorded.current_for(&other_package), None);
}

#[test]
fn an_assessor_offered_capabilities_is_refused_before_it_runs() {
    let error = run(
        &ReachingAssessor,
        &AssessmentRequest {
            key: key("sha256:0"),
            envelope: CapabilityEnvelope {
                network: true,
                ..CapabilityEnvelope::empty()
            },
            excerpt: String::new(),
        },
        0,
    )
    .expect_err("a non-empty envelope is refused");

    assert!(matches!(error, AssessmentError::EnvelopeNotEmpty(_)));
}
