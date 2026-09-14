//! Authenticated bounded local delivery, independently of Neovim.

use super::{BrokerError, Projection};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, BufRead, BufReader, Write},
    os::unix::{fs::MetadataExt, net::UnixStream},
    path::PathBuf,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// Explicit local endpoint provisioned for the dedicated broker identity.
/// The capability file is a broker-owned private copy of the Attention-only
/// producer capability. It is never placed inside a Session or sent to an Agent.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttentionEndpoint {
    /// Configured capture-service Attention socket, accessible to the broker UID.
    pub socket: PathBuf,
    /// Private broker-owned file containing the Attention producer capability.
    pub capability_file: PathBuf,
    /// Expected kernel UID of the capture-service socket peer.
    pub receiver_uid: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Acknowledgement {
    sequence: u64,
    digest: String,
    #[serde(rename = "applied")]
    _applied: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    r#type: String,
    request_id: String,
    result: Acknowledgement,
}

impl AttentionEndpoint {
    /// Starts one bounded asynchronous delivery and returns its owned worker.
    /// The caller must join the worker. Completion runs on that worker, exactly
    /// once when queued successfully; no UI or authorizing state is accessed.
    ///
    /// # Errors
    /// Returns a worker-start failure before the callback has been queued.
    pub fn publish(
        &self,
        entry: Projection,
        complete: Box<dyn FnOnce(Result<(), BrokerError>) + Send>,
    ) -> Result<JoinHandle<()>, BrokerError> {
        let endpoint = self.clone();
        thread::Builder::new()
            .name("louiselm-attention-delivery".into())
            .spawn(move || complete(endpoint.send(&entry).map_err(BrokerError::Attention)))
            .map_err(BrokerError::Attention)
    }

    fn send(&self, entry: &Projection) -> io::Result<()> {
        entry
            .change
            .validate()
            .map_err(|_| invalid("invalid projection"))?;
        let metadata = fs::symlink_metadata(&self.capability_file)?;
        if !metadata.is_file()
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.mode() & 0o077 != 0
            || metadata.len() > 256
        {
            return Err(invalid("Attention capability file is not private"));
        }
        let token = fs::read_to_string(&self.capability_file)?;
        let token = token.trim();
        if token.is_empty() || token.len() > 256 {
            return Err(invalid("Attention capability is invalid"));
        }
        let mut stream = connect_stream(&self.socket)?;
        let credentials = rustix::net::sockopt::socket_peercred(&stream)?;
        if credentials.uid.as_raw() != self.receiver_uid {
            return Err(invalid("Attention receiver identity does not match"));
        }
        let deadline = Instant::now() + Duration::from_secs(30);
        stream.set_write_timeout(Some(Duration::from_secs(30)))?;
        let request_id = format!("projection-{}", entry.sequence);
        let request = json!({"type": "project", "request_id": request_id,
            "projection": entry.wire(), "capability": token});
        stream.write_all(request.to_string().as_bytes())?;
        stream.write_all(b"\n")?;
        let mut reader = BufReader::new(stream);
        for _ in 0..16 {
            let frame: Value = serde_json::from_slice(&read_frame(&mut reader, deadline)?)
                .map_err(io::Error::other)?;
            if matches!(
                frame.get("type").and_then(Value::as_str),
                Some("snapshot" | "attention_changed")
            ) {
                continue;
            }
            let response: Response = serde_json::from_value(frame).map_err(io::Error::other)?;
            if response.r#type != "projection_result"
                || response.request_id != request_id
                || response.result.sequence != entry.sequence
                || response.result.digest != entry.digest()
            {
                return Err(invalid("Attention acknowledgement does not match"));
            }
            // A stale exact delivery is deliberately acknowledged without applying it.
            return Ok(());
        }
        Err(invalid("Attention acknowledgement was not received"))
    }
}

fn read_frame(reader: &mut BufReader<UnixStream>, deadline: Instant) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "Attention deadline expired"))?;
        reader.get_ref().set_read_timeout(Some(remaining))?;
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Attention connection closed",
            ));
        }
        let end = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|index| index + 1);
        let length = end.unwrap_or(available.len());
        if bytes.len().saturating_add(length) > 1024 * 1024 {
            return Err(invalid("Attention frame exceeds its bound"));
        }
        bytes.extend_from_slice(&available[..length]);
        reader.consume(length);
        if end.is_some() {
            return Ok(bytes);
        }
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn connect_stream(path: &std::path::Path) -> io::Result<UnixStream> {
    use rustix::{
        fs::{OFlags, fcntl_getfl, fcntl_setfl},
        net::{AddressFamily, SocketAddrUnix, SocketFlags, SocketType, connect, socket_with},
    };
    let address = SocketAddrUnix::new(path)?;
    let socket = socket_with(
        AddressFamily::UNIX,
        SocketType::STREAM,
        SocketFlags::CLOEXEC | SocketFlags::NONBLOCK,
        None,
    )?;
    // A full listener backlog is a retryable delivery failure, never an
    // unbounded blocking connect before the read deadline starts.
    connect(&socket, &address)?;
    let mut flags = fcntl_getfl(&socket)?;
    flags.remove(OFlags::NONBLOCK);
    fcntl_setfl(&socket, flags)?;
    Ok(socket.into())
}
