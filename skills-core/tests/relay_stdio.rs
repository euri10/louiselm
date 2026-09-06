//! The controller descriptor boundary preserves bytes and restores shared flags.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert observed outcomes."
)]

use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Write},
    os::{fd::OwnedFd, unix::net::UnixStream},
};

use louiselm_skills::launch_supervisor::RelayStdio;
use rustix::fs::{Mode, OFlags, fcntl_getfl};

#[test]
fn prefetched_acp_bytes_survive_attachment_and_flags_are_restored() {
    let (mut controller, input) = UnixStream::pair().unwrap();
    let (mut received, output) = UnixStream::pair().unwrap();
    let input_flags = fcntl_getfl(&input).unwrap();
    let output_flags = fcntl_getfl(&output).unwrap();
    let input_observer = input.try_clone().unwrap();
    let output_observer = output.try_clone().unwrap();
    controller.write_all(b"launch\n\xff\0opaque\n").unwrap();
    let mut input = BufReader::new(File::from(OwnedFd::from(input)));
    let mut frame = String::new();
    input.read_line(&mut frame).unwrap();
    assert_eq!(frame, "launch\n");
    assert_eq!(
        input.buffer(),
        b"\xff\0opaque\n",
        "the fixture really prefetched ACP bytes"
    );
    let mut stdio = RelayStdio::new(input, File::from(OwnedFd::from(output))).unwrap();
    let mut bytes = [0; 9];
    assert_eq!(stdio.read(&mut bytes).unwrap(), 9);
    assert_eq!(&bytes, b"\xff\0opaque\n");
    assert_eq!(
        stdio.read(&mut bytes).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    stdio.write_all(b"\0\xffreply").unwrap();
    let mut reply = [0; 7];
    received.read_exact(&mut reply).unwrap();
    assert_eq!(&reply, b"\0\xffreply");
    assert!(
        fcntl_getfl(&input_observer)
            .unwrap()
            .contains(OFlags::NONBLOCK)
    );
    assert!(
        fcntl_getfl(&output_observer)
            .unwrap()
            .contains(OFlags::NONBLOCK)
    );
    stdio.close().unwrap();
    assert_eq!(fcntl_getfl(&input_observer).unwrap(), input_flags);
    assert_eq!(fcntl_getfl(&output_observer).unwrap(), output_flags);
}

#[test]
fn duplex_descriptor_aliases_restore_the_original_flags() {
    let (_controller, duplex) = UnixStream::pair().unwrap();
    let observer = duplex.try_clone().unwrap();
    let input = duplex.try_clone().unwrap();
    let original = fcntl_getfl(&observer).unwrap();
    RelayStdio::new(
        BufReader::new(File::from(OwnedFd::from(input))),
        File::from(OwnedFd::from(duplex)),
    )
    .unwrap()
    .close()
    .unwrap();
    assert_eq!(fcntl_getfl(&observer).unwrap(), original);
}

#[test]
fn partial_descriptor_setup_failure_restores_the_already_configured_input() {
    let (_controller, input) = UnixStream::pair().unwrap();
    let observer = input.try_clone().unwrap();
    let original = fcntl_getfl(&observer).unwrap();
    let fixture = tempfile::tempdir().unwrap();
    let unusable_output = rustix::fs::open(
        fixture.path(),
        OFlags::PATH | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .unwrap();
    let result = RelayStdio::new(
        BufReader::new(File::from(OwnedFd::from(input))),
        File::from(unusable_output),
    );
    assert!(result.is_err());
    assert_eq!(fcntl_getfl(&observer).unwrap(), original);
}
