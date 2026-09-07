//! Installed initial enrollment, public readiness and explicit last-resort reset.

use super::{
    Browser, CliError, Flags, Instant, RecoveryError, Role, Store, Terminal, TrustStore,
    browser_url, invalid, now_ms, paper, passkey, proofs, register, replacement_keys, review, scan,
};
use crate::trust::onboarding::{self, PendingSetup, SETUP_NAMESPACE};

pub(super) fn status(store: &Store) -> Result<i32, CliError> {
    let status = crate::trust::status::read(store)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&status)
            .map_err(|_| RecoveryError::InvalidChange("public status serialization failed"))?
    );
    Ok(0)
}

pub(super) fn enroll(store: &Store, flags: &Flags<'_>) -> Result<i32, CliError> {
    if TrustStore::load(store)?.is_some() {
        return Err(RecoveryError::InvalidChange(
            "already enrolled; use recovery change or explicit reset",
        )
        .into());
    }
    let deadline = Instant::now() + passkey::TIMEOUT;
    let keys = replacement_keys(flags)?;
    let primary = keys
        .iter()
        .find(|key| key.role == Role::Primary)
        .ok_or_else(invalid)?;
    let release = keys
        .iter()
        .find(|key| key.role == Role::Release)
        .ok_or_else(invalid)?;
    let domain = flags
        .get("--trust-domain")
        .copied()
        .unwrap_or(crate::cli::DEFAULT_TRUST_DOMAIN);
    let setup = PendingSetup::new(
        domain,
        &primary.public_key,
        &release.public_key,
        crate::sshsig::SkPolicy::require_presence_and_verification(),
        now_ms(),
    )?;
    ceremony(store, flags, setup, deadline)
}

fn ceremony(
    store: &Store,
    flags: &Flags<'_>,
    setup: PendingSetup,
    deadline: Instant,
) -> Result<i32, CliError> {
    let mut terminal = Terminal::open()?;
    terminal.write(&format!("\r\nNew production trust domain: {}\r\nNothing is enrolled until both methods and both keys are confirmed.\r\n", scan::escape(&setup.trust().trust_domain)))?;
    let paper = paper(&mut terminal, deadline)?;
    let registration = register(&mut terminal, setup.trust(), deadline)?;
    let bytes = setup.plan(&paper, &registration)?;
    let mut terminal = review(
        &bytes,
        "BOTH primary and release signing-key possession",
        deadline,
    )?;
    let proofs = proofs(flags, SETUP_NAMESPACE, &bytes, deadline)?;
    let browser = Browser::bind()?.until(deadline);
    browser_url(&mut terminal, &browser)?;
    let plan: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| RecoveryError::Passkey)?;
    browser.run("confirm", &plan, &serde_json::json!({}), |value| {
        if value != serde_json::json!({}) || Instant::now() >= deadline { return Err(RecoveryError::Cancelled); }
        setup.apply(store, &paper, &registration, &proofs)?;
        Ok(((), "Committed: both signing roles, paper and passkey recovery enrolled. This does not establish Verified posture."))
    })?;
    terminal.write("\r\nSetup committed. Keep the paper offline and confirm the passkey is saved in your password manager.\r\n")?;
    status(store)
}

pub(super) fn reset(store: &Store, trust: &TrustStore) -> Result<i32, CliError> {
    let deadline = Instant::now() + passkey::TIMEOUT;
    let mut terminal = Terminal::open()?;
    terminal.write(&format!("\r\nLAST RESORT: discard ALL trust for {}.\r\nAll enrolled keys, methods and recorded-history authorization will be invalidated.\r\nSnapshot: {}\r\nUse recovery change if ANY method survives. Otherwise type reset then Enter (hidden):\r\n", scan::escape(&trust.trust_domain), trust.digest()))?;
    if terminal.read_hidden_until(deadline)?.as_str() != "reset" {
        return Err(RecoveryError::Cancelled.into());
    }
    terminal.restore()?;
    onboarding::reset(store, &trust.digest())?;
    terminal.write(
        "\r\nTrust reset. Run recovery setup to enroll everything anew, then re-admit supply.\r\n",
    )?;
    Ok(0)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Opt-in disposable PTY/browser fixture, never production credentials."
)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "Opt-in full local TTY/browser setup, passkey and paper change fixture. Run under a foreground PTY with an isolated virtual authenticator."]
    fn virtual_setup_and_changes() {
        let fixture = tempfile::tempdir().unwrap();
        let store = Store::open(&fixture.path().join("store")).unwrap();
        let mut paths = Vec::new();
        for name in ["primary", "release"] {
            let path = fixture.path().join(name);
            assert!(
                std::process::Command::new("/usr/bin/ssh-keygen")
                    .args(["-q", "-t", "ed25519", "-N", "", "-f"])
                    .arg(&path)
                    .status()
                    .unwrap()
                    .success()
            );
            paths.push(path.to_str().unwrap().to_owned());
        }
        let flags = Flags::from([
            ("--primary", paths[0].as_str()),
            ("--release", paths[1].as_str()),
        ]);
        let keys = replacement_keys(&flags).unwrap();
        let pending = PendingSetup::new(
            "PUBLIC DISPOSABLE SETUP FIXTURE",
            &keys[0].public_key,
            &keys[1].public_key,
            crate::sshsig::SkPolicy::none(),
            1,
        )
        .unwrap();
        // Same operator flow and real ssh-keygen/browser, software policy only in
        // this in-memory fixture. The release CLI has no bypass of its guard.
        ceremony(&store, &flags, pending, Instant::now() + passkey::TIMEOUT).unwrap();
        let trust = TrustStore::load(&store).unwrap().unwrap();
        assert!(trust.paper_verifier.is_some() && trust.passkey.is_some());
        assert!(!crate::trust::status::read(&store).unwrap().recovery_ready);
        let flags = Flags::from([("--via", "passkey"), ("--paper", "replace")]);
        super::super::change(&store, &trust, &flags).unwrap();
        let next = TrustStore::load(&store).unwrap().unwrap();
        assert_ne!(next.paper_verifier, trust.paper_verifier);
        let flags = Flags::from([("--via", "paper"), ("--paper", "replace")]);
        super::super::change(&store, &next, &flags).unwrap();
        let final_trust = TrustStore::load(&store).unwrap().unwrap();
        assert_ne!(final_trust.paper_verifier, next.paper_verifier);
        assert_eq!(final_trust.keys, trust.keys);
        println!(
            "PUBLIC_FIXTURE_COMPLETE: setup, passkey change and paper change committed; no production authority."
        );
    }
}
