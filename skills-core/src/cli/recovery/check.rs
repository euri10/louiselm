//! Non-mutating paper verification; secrets enter only through the private TTY.

use super::{
    CliError, Instant, PaperPhrase, RecoveryError, Store, Terminal, TrustStore, passkey, recovery,
    scan,
};
use crate::trust::persistence::LockedTrust;

pub(super) fn run(store: &Store) -> Result<i32, CliError> {
    // Existing read-only lock, held through input/result: no creation, repair,
    // publication or concurrent replacement of the enrollment being checked.
    let locked = LockedTrust::read_only(store)?;
    let trust = locked.load()?.ok_or(RecoveryError::Unauthorized)?;
    recovery::require_hardware_policy(&trust)?;
    if trust.paper_verifier.is_none() {
        return Err(RecoveryError::Unauthorized.into());
    }
    let mut terminal = Terminal::open()?;
    read(&trust, &mut terminal, Instant::now() + passkey::TIMEOUT)?;
    terminal.restore()?;
    println!(
        "Paper phrase matches the current enrollment for {}.\nSnapshot: {}\nNo trust or recovery method changed; the phrase was not consumed.",
        scan::escape(&trust.trust_domain),
        trust.digest()
    );
    Ok(0)
}

fn read(
    trust: &TrustStore,
    terminal: &mut Terminal,
    deadline: Instant,
) -> Result<(), RecoveryError> {
    terminal.write(&format!(
        "\r\nCheck ONLY — trust domain: {}\r\nSnapshot: {}\r\nNothing will be changed or consumed. Enter CURRENT 24-word paper phrase (hidden); Ctrl-C cancels:\r\n",
        scan::escape(&trust.trust_domain),
        trust.digest()
    ))?;
    let phrase = PaperPhrase::parse(&terminal.read_hidden_until(deadline)?)?;
    if trust.paper_verifier.as_ref() != Some(&phrase.verifier(&trust.trust_domain)) {
        return Err(RecoveryError::Unauthorized);
    }
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "Synthetic phrase/PTY fixtures and assertions, never operator secrets."
    )]
    use super::*;
    use crate::sshsig::SkPolicy;
    use rustix::{
        pty::{self, OpenptFlags},
        termios,
    };
    use std::{
        fs::{self, File},
        io::{Read, Write},
        time::Duration,
    };

    fn phrase(byte: u8) -> PaperPhrase {
        PaperPhrase::parse(
            &bip39::Mnemonic::from_entropy(&[byte; 32])
                .unwrap()
                .to_string(),
        )
        .unwrap()
    }

    fn terminal_check(
        snapshot: &TrustStore,
        input: &str,
        timeout: Duration,
    ) -> (Result<(), RecoveryError>, String) {
        // Other suite threads exec strict helper fixtures: never leak our PTY.
        let flags = OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC;
        let master = pty::openpt(flags).unwrap();
        pty::grantpt(&master).unwrap();
        pty::unlockpt(&master).unwrap();
        let slave = File::from(pty::ioctl_tiocgptpeer(&master, flags).unwrap());
        assert!(
            rustix::io::fcntl_getfd(&master)
                .unwrap()
                .contains(rustix::io::FdFlags::CLOEXEC)
        );
        assert!(
            rustix::io::fcntl_getfd(&slave)
                .unwrap()
                .contains(rustix::io::FdFlags::CLOEXEC)
        );
        let observer = slave.try_clone().unwrap();
        let original = termios::tcgetattr(&observer).unwrap();
        let mut terminal = Terminal::fixture(slave).unwrap();
        assert!(
            !termios::tcgetattr(&observer)
                .unwrap()
                .local_modes
                .contains(termios::LocalModes::ECHO)
        );
        let mut master = File::from(master);
        master.write_all(input.as_bytes()).unwrap();
        let result = read(snapshot, &mut terminal, Instant::now() + timeout);
        if result.is_ok() {
            terminal.restore().unwrap();
        }
        drop(terminal);
        let restored = termios::tcgetattr(&observer).unwrap();
        assert_eq!(restored.local_modes, original.local_modes);
        assert_eq!(restored.input_modes, original.input_modes);
        assert_eq!(restored.output_modes, original.output_modes);
        assert_eq!(restored.control_modes, original.control_modes);
        drop(observer);
        let mut output = Vec::new();
        // With the final slave closed, Linux ends the PTY stream with EIO.
        loop {
            let mut chunk = [0; 1024];
            match master.read(&mut chunk) {
                Ok(0) => break,
                Ok(count) => {
                    output.extend_from_slice(&chunk[..count]);
                    assert!(output.len() < 4096);
                }
                Err(error)
                    if error.raw_os_error() == Some(rustix::io::Errno::IO.raw_os_error()) =>
                {
                    break;
                }
                Err(error) => panic!("PTY output failed: {error}"),
            }
        }
        (result, String::from_utf8(output).unwrap())
    }

    #[test]
    fn hidden_check_matches_current_only_and_never_changes_the_store() {
        let fixture = tempfile::tempdir().unwrap();
        let store = Store::open(fixture.path()).unwrap();
        let mut trust = TrustStore::bootstrap(
            &store,
            "test/check-paper",
            "fixture-primary",
            "fixture-release",
            SkPolicy::require_presence_and_verification(),
            1,
        )
        .unwrap();
        trust.paper_verifier = Some(phrase(0).verifier(&trust.trust_domain));
        trust
            .retired_paper_verifiers
            .insert(phrase(1).verifier(&trust.trust_domain));
        LockedTrust::acquire(&store).unwrap().write(&trust).unwrap();
        let before = fs::read(store.root().join("trust/roles.json")).unwrap();
        let provenance = fs::read(store.root().join("provenance.json")).unwrap();
        for case in [
            "current", "wrong", "retired", "domain", "missing", "invalid", "cancel", "expire",
        ] {
            let locked = LockedTrust::read_only(&store).unwrap();
            let mut snapshot = locked.load().unwrap().unwrap();
            if case == "domain" {
                snapshot.trust_domain = "other/domain".to_owned();
            }
            if case == "missing" {
                snapshot.paper_verifier = None;
            }
            assert!(matches!(
                LockedTrust::acquire(&store),
                Err(crate::trust::TrustError::Busy(_))
            ));
            let input = match case {
                "invalid" => "invalid-fixture\r".to_owned(),
                "cancel" => "synthetic-secret\x03".to_owned(),
                "expire" => String::new(),
                _ => format!(
                    "{}\r",
                    phrase(match case {
                        "wrong" => 2,
                        "retired" => 1,
                        _ => 0,
                    })
                    .expose_secret()
                    .as_str()
                ),
            };
            let (result, output) = terminal_check(
                &snapshot,
                &input,
                Duration::from_millis(if case == "expire" { 5 } else { 5000 }),
            );
            match case {
                "current" => result.unwrap(),
                "invalid" => assert!(matches!(result, Err(RecoveryError::InvalidPhrase))),
                "cancel" => assert!(matches!(result, Err(RecoveryError::Cancelled))),
                "expire" => assert!(matches!(result, Err(RecoveryError::Expired))),
                _ => assert!(matches!(result, Err(RecoveryError::Unauthorized))),
            }
            assert!(output.contains("Check ONLY"));
            assert!(!output.contains(phrase(0).expose_secret().as_str()));
            assert!(!output.contains("synthetic-secret"));
            assert!(!output.contains("invalid-fixture"));
            assert_eq!(
                fs::read(store.root().join("trust/roles.json")).unwrap(),
                before
            );
            assert_eq!(
                fs::read(store.root().join("provenance.json")).unwrap(),
                provenance
            );
            drop(locked);
            LockedTrust::acquire(&store).unwrap();
        }
    }
}
