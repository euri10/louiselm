//! Durable, append-only admission cutoff intent.

use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::fixed;
use crate::hex::lower_hex;

const MAX_LOG_BYTES: u64 = 65_536;
const MAX_ENTRIES: u64 = 256;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum CutoffKind {
    Disable,
    Forget,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CutoffIntent {
    pub(super) kind: CutoffKind,
    pub(super) device_id: String,
    pub(super) intent_id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum EntryBody {
    Initialized {
        v: u8,
        seq: u64,
        receiver: String,
        generation: String,
        nonce: String,
    },
    Pending {
        v: u8,
        seq: u64,
        receiver: String,
        generation: String,
        nonce: String,
        kind: CutoffKind,
        device_id: String,
        intent_id: String,
    },
    Complete {
        v: u8,
        seq: u64,
        receiver: String,
        generation: String,
        nonce: String,
        intent_id: String,
    },
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    body: EntryBody,
    sha256: String,
}

pub(super) struct State {
    pub(super) next_seq: u64,
    pub(super) pending: Option<CutoffIntent>,
}

impl EntryBody {
    fn common(&self) -> (u8, u64, &str, &str, &str) {
        match self {
            Self::Initialized {
                v,
                seq,
                receiver,
                generation,
                nonce,
            }
            | Self::Pending {
                v,
                seq,
                receiver,
                generation,
                nonce,
                ..
            }
            | Self::Complete {
                v,
                seq,
                receiver,
                generation,
                nonce,
                ..
            } => (*v, *seq, receiver, generation, nonce),
        }
    }
}

pub(super) fn read(
    file: &mut File,
    receiver: &str,
    generation: &str,
    nonce: &str,
) -> io::Result<State> {
    file.rewind()?;
    let mut bytes = Vec::new();
    file.take(MAX_LOG_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_LOG_BYTES || bytes.last() != Some(&b'\n') {
        return Err(io::Error::other("barrier cutoff log is corrupt"));
    }
    let mut pending = None;
    let mut expected_seq = 0_u64;
    for raw in bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let entry: Entry = serde_json::from_slice(raw)
            .map_err(|_| io::Error::other("barrier cutoff log is corrupt"))?;
        let digest = lower_hex(&Sha256::digest(
            serde_json::to_vec(&entry.body).map_err(io::Error::other)?,
        ));
        let (version, seq, bound_receiver, bound_generation, bound_nonce) = entry.body.common();
        if entry.sha256 != digest
            || version != 1
            || seq != expected_seq
            || bound_receiver != receiver
            || bound_generation != generation
            || bound_nonce != nonce
        {
            return Err(io::Error::other("barrier cutoff log is corrupt or unbound"));
        }
        match entry.body {
            EntryBody::Initialized { .. } if seq == 0 && pending.is_none() => {}
            EntryBody::Pending {
                kind,
                device_id,
                intent_id,
                ..
            } if seq > 0 && pending.is_none() => {
                if !valid_device(&device_id) || fixed::<16>(&intent_id).is_err() {
                    return Err(io::Error::other("barrier cutoff log is corrupt"));
                }
                pending = Some(CutoffIntent {
                    kind,
                    device_id,
                    intent_id,
                });
            }
            EntryBody::Complete { intent_id, .. }
                if pending
                    .as_ref()
                    .is_some_and(|intent| intent.intent_id == intent_id) =>
            {
                pending = None;
            }
            _ => return Err(io::Error::other("barrier cutoff log is inconsistent")),
        }
        expected_seq = expected_seq
            .checked_add(1)
            .ok_or_else(|| io::Error::other("barrier cutoff sequence overflow"))?;
        if expected_seq > MAX_ENTRIES {
            return Err(io::Error::other("barrier cutoff log is full"));
        }
    }
    if expected_seq == 0 {
        return Err(io::Error::other("barrier cutoff log is empty"));
    }
    Ok(State {
        next_seq: expected_seq,
        pending,
    })
}

pub(super) fn append_pending(
    file: &mut File,
    state: &mut State,
    receiver: &str,
    generation: &str,
    nonce: &str,
    intent: &CutoffIntent,
) -> io::Result<()> {
    if state.pending.is_some() || state.next_seq >= MAX_ENTRIES {
        return Err(io::Error::other("barrier cutoff is unavailable"));
    }
    append(
        file,
        EntryBody::Pending {
            v: 1,
            seq: state.next_seq,
            receiver: receiver.to_owned(),
            generation: generation.to_owned(),
            nonce: nonce.to_owned(),
            kind: intent.kind,
            device_id: intent.device_id.clone(),
            intent_id: intent.intent_id.clone(),
        },
    )?;
    state.next_seq += 1;
    state.pending = Some(intent.clone());
    Ok(())
}

pub(super) fn append_complete(
    file: &mut File,
    state: &mut State,
    receiver: &str,
    generation: &str,
    nonce: &str,
) -> io::Result<()> {
    let intent = state
        .pending
        .as_ref()
        .ok_or_else(|| io::Error::other("barrier cutoff intent is missing"))?;
    if state.next_seq >= MAX_ENTRIES {
        return Err(io::Error::other("barrier cutoff log is full"));
    }
    append(
        file,
        EntryBody::Complete {
            v: 1,
            seq: state.next_seq,
            receiver: receiver.to_owned(),
            generation: generation.to_owned(),
            nonce: nonce.to_owned(),
            intent_id: intent.intent_id.clone(),
        },
    )?;
    state.next_seq += 1;
    state.pending = None;
    Ok(())
}

fn append(file: &mut File, body: EntryBody) -> io::Result<()> {
    let sha256 = lower_hex(&Sha256::digest(
        serde_json::to_vec(&body).map_err(io::Error::other)?,
    ));
    let mut bytes = serde_json::to_vec(&Entry { body, sha256 }).map_err(io::Error::other)?;
    bytes.push(b'\n');
    let end = file.seek(SeekFrom::End(0))?;
    let next = end
        .checked_add(u64::try_from(bytes.len()).map_err(io::Error::other)?)
        .ok_or_else(|| io::Error::other("barrier cutoff log size overflow"))?;
    if next > MAX_LOG_BYTES {
        return Err(io::Error::other("barrier cutoff log is full"));
    }
    file.write_all(&bytes)?;
    file.sync_all()
}

fn valid_device(device: &str) -> bool {
    uuid::Uuid::parse_str(device).is_ok_and(|parsed| parsed.to_string() == device)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test fixture serialization")]
pub(super) fn initialized_bytes(receiver: &str, generation: &str, nonce: &str) -> Vec<u8> {
    let body = EntryBody::Initialized {
        v: 1,
        seq: 0,
        receiver: receiver.to_owned(),
        generation: generation.to_owned(),
        nonce: nonce.to_owned(),
    };
    let sha256 = lower_hex(&Sha256::digest(serde_json::to_vec(&body).unwrap()));
    let mut bytes = serde_json::to_vec(&Entry { body, sha256 }).unwrap();
    bytes.push(b'\n');
    bytes
}
