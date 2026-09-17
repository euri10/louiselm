//! Authenticated dependency relay; proposed names do not become execution requests.

use super::*;
use crate::dependency_fetch::{Candidate, DependencyRequest, DependencyStatus, Source};

fn begin(harness: &mut Harness, bytes: &[u8]) -> CommandMessage {
    let candidate = Candidate {
        name: "malicious".into(),
        version: "1.0.0".into(),
        source: Source::Registry {
            registry: "test".into(),
        },
        integrity: Some(Digest::of(bytes).to_string()),
    };
    let query = harness.owner.command_message(
        "dependency-agent",
        CommandOperation::DependencyFetch {
            request: DependencyRequest {
                request_id: "dependency".into(),
                candidate,
                max_bytes: 64 * 1024,
            },
        },
    );
    settle(|complete| harness.agent.send(query.canonical_bytes(), complete));
    harness.tick();
    receive(&harness.broker)
}

fn chunk(harness: &Harness, query: &CommandMessage, bytes: &[u8]) -> CommandMessage {
    let CommandOperation::DependencyFetch { request } = &query.operation else {
        unreachable!()
    };
    CommandMessage {
        operation: CommandOperation::DependencyChunk {
            chunk: crate::dependency_fetch::CacheChunk {
                candidate_id: request.candidate.id().unwrap(),
                artifact_digest: Digest::of(bytes).to_string(),
                total_size: bytes.len() as u64,
                offset: 0,
                bytes: bytes.to_vec(),
                head: harness.owner.broker_head.clone(),
                expires_at_ms: u64::try_from(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_millis(),
                )
                .unwrap()
                    + 30_000,
            },
        },
        ..query.clone()
    }
}

fn complete_cache(harness: &mut Harness) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while harness
        .owner
        .commands
        .cache_result
        .lock()
        .unwrap()
        .is_none()
    {
        assert!(Instant::now() < deadline, "cache worker did not complete");
        std::thread::sleep(Duration::from_millis(1));
    }
    harness.owner.collect_tool_result();
}

fn hostile_tar(script: &[u8]) -> Vec<u8> {
    let mut archive = Vec::new();
    for (name, bytes) in [
        ("post-install.sh", script),
        ("../../escaped", b"archive traversal".as_slice()),
    ] {
        let mut header = [0_u8; 512];
        header[..name.len()].copy_from_slice(name.as_bytes());
        header[100..108].copy_from_slice(b"0000777\0");
        header[108..116].copy_from_slice(b"0000000\0");
        header[116..124].copy_from_slice(b"0000000\0");
        header[124..136].copy_from_slice(format!("{:011o}\0", bytes.len()).as_bytes());
        header[136..148].copy_from_slice(b"00000000000\0");
        header[148..156].fill(b' ');
        header[156] = b'0';
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        let checksum: u32 = header.iter().map(|byte| u32::from(*byte)).sum();
        header[148..156].copy_from_slice(format!("{checksum:06o}\0 ").as_bytes());
        archive.extend_from_slice(&header);
        archive.extend_from_slice(bytes);
        archive.resize(archive.len().next_multiple_of(512), 0);
    }
    archive.resize(archive.len() + 1024, 0);
    archive
}

#[test]
fn dependency_chunks_publish_only_opaque_cache_bytes_and_never_run_scripts() {
    use std::os::unix::fs::PermissionsExt;
    let mut harness = Harness::new("true", false);
    // A valid tar with executable install code and a traversal entry remains
    // opaque. The transport/cache path has no extractor or script runner.
    let marker = harness.root.path().join("script-ran");
    let bytes = hostile_tar(format!("#!/bin/sh\ntouch {}\n", marker.display()).as_bytes());
    let query = begin(&mut harness, &bytes);
    harness
        .owner
        .handle_command(chunk(&harness, &query, &bytes));
    complete_cache(&mut harness);
    let reply = receive(&harness.broker);
    let CommandOperation::DependencyChunkResult {
        received,
        artifact: Some(artifact),
    } = reply.operation
    else {
        panic!("{reply:?}")
    };
    assert_eq!(received, bytes.len() as u64);
    let path = harness
        .root
        .path()
        .join("cache-session-1")
        .join(&artifact.name);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(!marker.exists());
    assert!(!harness.root.path().join("escaped").exists());
    assert!(harness.audit.entries().unwrap().is_empty());
    harness.owner.handle_command(CommandMessage {
        operation: CommandOperation::DependencyResult {
            status: DependencyStatus::Complete { artifact },
        },
        ..query
    });
    assert!(matches!(
        receive(&harness.agent).operation,
        CommandOperation::DependencyResult {
            status: DependencyStatus::Complete { .. }
        }
    ));
}

#[test]
fn dependency_cache_transfer_accumulates_only_contiguous_bounded_chunks() {
    let mut harness = Harness::new("true", false);
    let bytes = vec![42; crate::dependency_fetch::CACHE_CHUNK_BYTES * 2 + 1];
    let query = begin(&mut harness, &bytes);
    let original = chunk(&harness, &query, &bytes);
    let mut offset = 0;
    for part in bytes.chunks(crate::dependency_fetch::CACHE_CHUNK_BYTES) {
        let mut message = original.clone();
        let CommandOperation::DependencyChunk { chunk } = &mut message.operation else {
            unreachable!()
        };
        chunk.offset = offset;
        chunk.bytes = part.to_vec();
        harness.owner.handle_command(message);
        offset += part.len() as u64;
        if offset == bytes.len() as u64 {
            complete_cache(&mut harness);
        }
        let reply = receive(&harness.broker);
        assert!(
            matches!(reply.operation, CommandOperation::DependencyChunkResult { received, .. } if received == offset)
        );
    }
    assert_eq!(
        std::fs::read(
            harness
                .root
                .path()
                .join("cache-session-1")
                .join(format!("artifact-{}", Digest::of(&bytes).hex()))
        )
        .unwrap(),
        bytes
    );
}

#[test]
fn dependency_ingress_rejects_foreign_context_changed_heads_offsets_and_corrupt_bytes() {
    for variant in [
        "session", "run", "revision", "head", "offset", "digest", "corrupt", "expired", "revoked",
    ] {
        let mut harness = Harness::new("true", false);
        let query = begin(&mut harness, b"archive");
        let mut message = chunk(&harness, &query, b"archive");
        let CommandOperation::DependencyChunk { chunk } = &mut message.operation else {
            unreachable!()
        };
        match variant {
            "session" => message.session_id = "foreign".into(),
            "run" => message.run_id = "foreign".into(),
            "revision" => message.envelope_revision += 1,
            "head" => chunk.head.sequence += 1,
            "offset" => {
                chunk.offset = 1;
                chunk.bytes.pop();
            }
            "digest" => chunk.artifact_digest = Digest::of(b"wrong").to_string(),
            "corrupt" => chunk.bytes[0] ^= 1,
            "expired" => chunk.expires_at_ms = 1,
            "revoked" => harness
                .owner
                .resources
                .capability
                .as_ref()
                .unwrap()
                .command_enforcer()
                .unwrap()
                .revoke()
                .unwrap(),
            _ => unreachable!(),
        }
        harness.owner.handle_command(message);
        if matches!(variant, "corrupt" | "revoked") {
            complete_cache(&mut harness);
        }
        assert!(
            matches!(
                receive(&harness.broker).operation,
                CommandOperation::DependencyRefused { .. }
            ),
            "{variant}"
        );
        assert_eq!(
            std::fs::read_dir(harness.root.path().join("cache-session-1"))
                .unwrap()
                .count(),
            0,
            "{variant}"
        );
    }
}

#[test]
fn broker_cannot_claim_a_completed_dependency_without_confirmed_cache_publication() {
    let mut harness = Harness::new("true", false);
    let query = begin(&mut harness, b"archive");
    let digest = Digest::of(b"archive");
    harness.owner.handle_command(CommandMessage {
        operation: CommandOperation::DependencyResult {
            status: DependencyStatus::Complete {
                artifact: crate::dependency_fetch::Artifact {
                    name: format!("artifact-{}", digest.hex()),
                    digest: digest.to_string(),
                    size: 7,
                    integrity_verified: true,
                },
            },
        },
        ..query.clone()
    });
    assert!(
        harness.owner.commands.status.is_some(),
        "unproven completion is ignored"
    );
    harness.owner.expire_agent_status(&query.request_id);
    assert!(matches!(
        receive(&harness.agent).operation,
        CommandOperation::DependencyRefused { .. }
    ));
    assert_eq!(
        std::fs::read_dir(harness.root.path().join("cache-session-1"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn dependency_candidates_use_authenticated_relay_and_exact_pending_identity() {
    let mut harness = Harness::new("true", false);
    let candidate = Candidate {
        name: "example".into(),
        version: "1.2.3".into(),
        source: Source::Registry {
            registry: "crates-io".into(),
        },
        integrity: Some(Digest::of(b"archive").to_string()),
    };
    let query = harness.owner.command_message(
        "dependency-outer",
        CommandOperation::DependencyFetch {
            request: DependencyRequest {
                request_id: "dependency-one".into(),
                candidate: candidate.clone(),
                max_bytes: 1024,
            },
        },
    );
    settle(|complete| harness.agent.send(query.canonical_bytes(), complete));
    harness.tick();
    assert!(
        harness.owner.commands.status.is_some(),
        "a dependency proposal must reach the broker as typed data"
    );
    let mut forwarded = receive(&harness.broker);
    assert_eq!(forwarded.operation, query.operation);
    forwarded.operation = CommandOperation::DependencyResult {
        status: DependencyStatus::Pending {
            candidate_id: candidate.id().unwrap(),
        },
    };
    harness.owner.handle_command(forwarded);
    assert_eq!(receive(&harness.agent).request_id, query.request_id);
    assert!(
        harness.audit.entries().unwrap().is_empty(),
        "a proposal does not authorize command execution"
    );
}
