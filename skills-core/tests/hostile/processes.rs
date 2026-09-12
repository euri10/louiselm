//! Simultaneous owned targets; never inspect or signal desktop processes.

use super::{
    fixture::{Fixture, mode},
    probe::{Attack, Probe},
};
use std::{
    fs, thread,
    time::{Duration, Instant},
};

pub fn run(fixture: &Fixture) {
    let mut operator = fixture.outside(fixture.operator);
    let mut launcher = fixture.outside(0);
    let mut broker = fixture.outside(60002);
    let first_plan = fixture.plan("first", 60000);
    let second_plan = fixture.plan("second", 60001);
    let mut first = fixture.inside(&first_plan);
    let mut second = fixture.inside(&second_plan);
    // A response proves each process reached its live command loop before it
    // becomes a target. A dead process cannot satisfy a negative control.
    for agent in [
        &mut operator,
        &mut launcher,
        &mut broker,
        &mut first,
        &mut second,
    ] {
        assert!(agent.request(&[]).is_empty());
    }
    let targets = [
        ("operator", operator.pid()),
        ("launcher", launcher.pid()),
        ("broker", broker.pid()),
        ("second-session", second.pid()),
    ];
    let mut probes = Vec::new();
    for (name, pid) in targets {
        assert!(
            pid > 20,
            "outside PIDs must not alias the short-lived inner probe tree"
        );
        probes.push(Probe::new(
            &format!("proc-{name}"),
            Attack::Read(format!("/proc/{pid}/status").into()),
        ));
        probes.push(Probe::new(&format!("ptrace-{name}"), Attack::Ptrace(pid)));
        probes.push(Probe::new(&format!("signal-{name}"), Attack::Signal(pid)));
    }
    let second_file = second_plan.home.join("private-sentinel");
    fs::write(&second_file, b"second-session-private").unwrap();
    probes.push(Probe::new("second-session-file", Attack::Read(second_file)));
    let mut control = fixture.outside(0);
    fixture.check(&probes, &control.request(&probes), &first.request(&probes));
    let first_file = first_plan.home.join("private-sentinel");
    fs::write(&first_file, b"first-session-private").unwrap();
    let reverse = [
        Probe::new("first-session-file", Attack::Read(first_file)),
        Probe::new(
            "proc-first-session",
            Attack::Read(format!("/proc/{}/status", first.pid()).into()),
        ),
        Probe::new("ptrace-first-session", Attack::Ptrace(first.pid())),
        Probe::new("signal-first-session", Attack::Signal(first.pid())),
    ];
    fixture.check(
        &reverse,
        &control.request(&reverse),
        &second.request(&reverse),
    );
    for agent in [
        &mut operator,
        &mut launcher,
        &mut broker,
        &mut first,
        &mut second,
    ] {
        assert!(
            agent.request(&[]).is_empty(),
            "targets survive every attack"
        );
        agent.dispose();
    }
    control.dispose();
    lifecycle(fixture);
}

fn wait_for(stage: &str, mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !predicate() {
        assert!(
            Instant::now() < deadline,
            "required {stage} observation timed out"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn lifecycle(fixture: &Fixture) {
    let mut plan = fixture.plan("forking", 60000);
    let heartbeat = plan.workspace.join("heartbeat");
    let foreground = plan.workspace.join("foreground");
    plan.executable = plan.runtime_root.join("fork-agent");
    // The authenticated principal is this measured ELF, not a shebang path
    // that exec resolves to a different interpreter inode.
    fs::copy("/bin/bash", &plan.executable).unwrap();
    mode(&plan.executable, 0o755);
    plan.arguments = vec!["-c".to_owned(), "( trap '' INT; while :; do printf x >> \"$HEARTBEAT\"; sleep .01; done ) &\nwhile :; do printf x >> \"$FOREGROUND\"; sleep .01; done\n".to_owned()];
    plan.environment
        .insert("HEARTBEAT".into(), heartbeat.display().to_string());
    plan.environment
        .insert("FOREGROUND".into(), foreground.display().to_string());
    let mut agent = fixture.inside(&plan);
    wait_for("heartbeat", || {
        fs::metadata(&heartbeat).is_ok_and(|metadata| metadata.len() > 2)
    });
    wait_for("foreground heartbeat", || {
        fs::metadata(&foreground).is_ok_and(|metadata| metadata.len() > 2)
    });
    assert!(
        agent.session().processes().unwrap().len() >= 4,
        "forked descendants really exist"
    );
    agent.session().park().unwrap();
    let frozen = fs::read(&heartbeat).unwrap();
    thread::sleep(Duration::from_millis(100));
    assert_eq!(
        fs::read(&heartbeat).unwrap(),
        frozen,
        "whole tree stops writing while Parked"
    );
    agent.session().resume().unwrap();
    wait_for("Resume", || {
        fs::read(&heartbeat).unwrap().len() > frozen.len()
    });
    assert!(agent.session().interrupt().unwrap() > 0);
    // Delivery can kill namespace supervision before a shell trap executes.
    // Observable stopped work, not a trap acknowledgement, is the contract.
    thread::sleep(Duration::from_millis(150));
    let interrupted = fs::read(&foreground).unwrap();
    thread::sleep(Duration::from_millis(100));
    assert_eq!(
        fs::read(&foreground).unwrap(),
        interrupted,
        "foreground work stops after Interrupt"
    );
    agent.dispose();
    let disposed = fs::read(&heartbeat).unwrap();
    thread::sleep(Duration::from_millis(50));
    assert_eq!(
        fs::read(&heartbeat).unwrap(),
        disposed,
        "INT-ignoring descendants cannot survive Disposal"
    );
    println!("lifecycle: fork/Park/Resume/Interrupt/Disposal passed");
    fixture.record(
        "lifecycle",
        "live descendants stopped during Park, resumed, interrupted; zero survivors after Disposal",
    );
}
