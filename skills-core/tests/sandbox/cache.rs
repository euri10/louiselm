//! Cache consumers through real private-home mounts and cgroup lifecycle.

use super::*;
use louiselm_skills::{Digest, cache::CacheBase};
use std::os::unix::fs::{MetadataExt as _, chown};

#[test]
fn cache_mounts_preserve_bytes_through_park_and_dispose_only_the_owner() {
    if !cgroup_available() {
        return;
    }
    exercise(None);
}

#[test]
fn host_identity_cache_isolation_and_lifecycle() {
    let Some(uid) = host_identity_test_id() else {
        assert!(
            env::var_os("LOUISELM_REQUIRE_INITIAL_HOST_IDENTITY").is_none(),
            "required cache conformance needs host root"
        );
        eprintln!("skipping: cache host-identity conformance requires guest root");
        return;
    };
    if env::var_os("LOUISELM_REQUIRE_INITIAL_HOST_IDENTITY").is_some() {
        assert!(is_initial_user_namespace());
    }
    exercise(Some(uid));
}

fn cache_plan(fixture: &Fixture, id: &str, uid: Option<u32>) -> ConfinementPlan {
    let mut confinement = plan(
        fixture,
        id,
        r#"#!/bin/sh
set -eu
test "$(cat "$CACHE/dependency")" = warm
test ! -e "$OTHER"
test ! -e "$SOURCE"
if (printf poison > "$OTHER/dependency") 2>/dev/null; then exit 11; fi
printf changed > "$CACHE/dependency"
printf 'ready\n'
read -r command
test "$command" = resume
test "$(cat "$CACHE/$ARTIFACT")" = download
printf 'resumed\n'
sleep 30
"#,
    );
    fs::create_dir_all(&confinement.home).unwrap();
    fs::set_permissions(
        confinement.home.parent().unwrap(),
        fs::Permissions::from_mode(0o711),
    )
    .unwrap();
    fs::set_permissions(&confinement.home, fs::Permissions::from_mode(0o700)).unwrap();
    if let Some(uid) = uid {
        confinement.identity = IdentityPlan::HostIdentity { uid, gid: uid };
        chown(&confinement.home, Some(uid), Some(uid)).unwrap();
    }
    confinement
}

fn exercise(uid: Option<u32>) {
    let fixture = Fixture::new();
    fs::set_permissions(fixture.path(""), fs::Permissions::from_mode(0o755)).unwrap();
    let source = fixture.path("warm");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("dependency"), b"warm").unwrap();
    let base = CacheBase::capture(&source).unwrap();
    let mut first = cache_plan(&fixture, &unique_id("cache-first"), uid);
    let mut second = cache_plan(&fixture, &unique_id("cache-second"), uid.map(|id| id + 1));
    let mut first_cache = base
        .materialize(&first.home, &first.session_id, first.identity)
        .unwrap();
    let mut second_cache = base
        .materialize(&second.home, &second.session_id, second.identity)
        .unwrap();
    let download = Digest::of(b"download");
    let artifact = format!("artifact-{}", download.hex());
    for (plan, own, other) in [
        (&mut first, &first_cache, &second_cache),
        (&mut second, &second_cache, &first_cache),
    ] {
        plan.environment
            .insert("CACHE".into(), own.path().display().to_string());
        plan.environment
            .insert("OTHER".into(), other.path().display().to_string());
        plan.environment
            .insert("SOURCE".into(), source.display().to_string());
        plan.environment.insert("ARTIFACT".into(), artifact.clone());
    }
    let backend = BubblewrapBackend::new().with_bootstrap(Path::new(BOOTSTRAP));
    let mut first_session = backend.spawn(&first).unwrap();
    let mut second_session = backend.spawn(&second).unwrap();
    let mut first_output = BufReader::new(first_session.take_stdout().unwrap()).lines();
    let mut second_output = BufReader::new(second_session.take_stdout().unwrap()).lines();
    assert_eq!(first_output.next().unwrap().unwrap(), "ready");
    assert_eq!(second_output.next().unwrap().unwrap(), "ready");
    let mut inputs = discovery_support::DiscoveryFixture::new().manifest;
    inputs.cache_base_digest = base.digest().to_string();
    assert_eq!(
        first_cache.check_verified(&first_session, &inputs).is_ok(),
        uid.is_some()
    );
    assert!(
        first_cache
            .check_verified(&second_session, &inputs)
            .is_err()
    );
    inputs.cache_base_digest = Digest::of(b"substitution").to_string();
    assert!(first_cache.check_verified(&first_session, &inputs).is_err());
    inputs.cache_base_digest = base.digest().to_string();
    let evidence = first_session.evidence.clone();
    first_session.evidence.dimensions.clear();
    assert!(first_cache.check_verified(&first_session, &inputs).is_err());
    first_session.evidence = evidence;
    assert!(first_cache.dispose(&mut second_session).is_err());
    for (cache, expected_uid) in [
        (&mut first_cache, uid),
        (&mut second_cache, uid.map(|id| id + 1)),
    ] {
        assert_eq!(
            cache.store_download(&download, b"download").unwrap(),
            artifact
        );
        if let Some(expected) = expected_uid {
            assert_eq!(
                fs::metadata(cache.path().join(&artifact)).unwrap().uid(),
                expected
            );
        }
    }
    first_session.park().unwrap();
    second_session.park().unwrap();
    assert_eq!(
        fs::read(first_cache.path().join("dependency")).unwrap(),
        b"changed"
    );
    assert_eq!(
        fs::read(second_cache.path().join("dependency")).unwrap(),
        b"changed"
    );
    assert_eq!(fs::read(source.join("dependency")).unwrap(), b"warm");
    assert_eq!(CacheBase::capture(&source).unwrap().digest(), base.digest());
    for session in [&mut first_session, &mut second_session] {
        session.resume().unwrap();
        session.stdin().unwrap().write_all(b"resume\n").unwrap();
    }
    assert_eq!(first_output.next().unwrap().unwrap(), "resumed");
    assert_eq!(second_output.next().unwrap().unwrap(), "resumed");
    first_cache.dispose(&mut first_session).unwrap();
    assert!(!first_cache.path().exists());
    assert!(first_cache.store_download(&download, b"download").is_err());
    assert!(second_cache.path().exists());
    second_cache.dispose(&mut second_session).unwrap();
    assert!(!second_cache.path().exists());
    assert!(source.join("dependency").exists());
}
