//! Real controlling-terminal coverage in an isolated subprocess/session.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Isolated PTY test fixtures and assertions."
)]

use super::*;

#[test]
fn foreground_signer_fixture_child() {
    use rustix::pty::{self, OpenptFlags};
    use rustix::termios;
    if std::env::var_os("LOUISELM_TEST_FOREGROUND_CHILD").is_none() {
        return;
    }
    rustix::process::setsid().unwrap();
    let master = pty::openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
    pty::grantpt(&master).unwrap();
    pty::unlockpt(&master).unwrap();
    let slave = pty::ioctl_tiocgptpeer(&master, OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
    let master = File::from(master);
    let terminal = File::from(slave);
    rustix::process::ioctl_tiocsctty(&terminal).unwrap();
    let group = rustix::process::getpgrp();
    assert_eq!(termios::tcgetpgrp(&terminal).unwrap(), group);
    let mut input = master.try_clone().unwrap();
    input.write_all(b"public-fixture-pin\n").unwrap();
    let invocation = CommandInvocation {
        program: "/bin/sh".into(),
        arguments: vec![
            "-c".into(),
            "read pin </dev/tty; test \"$pin\" = public-fixture-pin".into(),
        ],
        stdin: vec![],
        current_dir: None,
    };
    let result = run_signing_command(
        &invocation,
        Instant::now() + Duration::from_secs(2),
        Some(&terminal),
    )
    .expect("foreground read completes before deadline");
    assert!(result.success);
    assert_eq!(termios::tcgetpgrp(&terminal).unwrap(), group);
    let invocation = CommandInvocation {
        program: "/bin/sh".into(),
        arguments: vec!["-c".into(), "read pin </dev/tty".into()],
        stdin: vec![],
        current_dir: None,
    };
    let error = run_signing_command(
        &invocation,
        Instant::now() + Duration::from_millis(250),
        Some(&terminal),
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(termios::tcgetpgrp(&terminal).unwrap(), group);
    // The master must stay alive through all assertions; closing it delivers HUP
    // to this disposable session, never to the maintainer's real terminal.
    std::mem::forget(master);
}

#[test]
fn bounded_signer_can_read_the_tty_and_restores_it_after_timeout() {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "launcher_install::foreground_tests::foreground_signer_fixture_child",
            "--nocapture",
        ])
        .env("LOUISELM_TEST_FOREGROUND_CHILD", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= deadline {
            // This still-waitable child created its own disposable session. Kill
            // only its known process group and reap it if a TTY regression hangs.
            let _ = rustix::process::kill_process_group(
                rustix::process::Pid::from_child(&child),
                rustix::process::Signal::KILL,
            );
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("foreground signer fixture hung");
        }
        thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
