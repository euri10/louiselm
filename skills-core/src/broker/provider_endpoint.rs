//! Local HTTP/1.1 endpoint a confined runtime uses as its Responses `base_url`.
//!
//! Parses each complete request, hands it to `admit` (which runs on the
//! Session's owning worker), and relays the upstream body as chunks arrive.
//! Placing the listener inside the Session's network namespace is
//! `louiselm-qbr.5.1.3.2.4`; until then nothing in production serves this.

use std::io::{Read, Write};

use super::{BrokerError, provider_transport::UpstreamResponse};
use crate::{
    launch_protocol::{ErrorCode, ProtocolError},
    provider_request::{Frames, ProviderRequest},
};

const RELAY_CHUNK: usize = 16 * 1024;

/// Serves one local connection until EOF or the first refusal.
///
/// A refusal answers with a typed JSON error and closes the connection; a
/// partial request at EOF is abandoned without admission. An upstream failure
/// mid-stream closes the connection without the terminating chunk, so the
/// runtime observes an incomplete (unknown) outcome rather than a clean end.
///
/// # Errors
/// Returns [`BrokerError::ProviderUnavailable`] when the local connection or
/// the upstream stream fails; the admitted unit stays spent either way.
pub fn serve_provider_connection<S, A>(
    mut stream: S,
    host: String,
    mut admit: A,
) -> Result<(), BrokerError>
where
    S: Read + Write,
    A: FnMut(&ProviderRequest) -> Result<UpstreamResponse, BrokerError>,
{
    let mut frames = Frames::new(host);
    let mut buffer = vec![0; RELAY_CHUNK];
    loop {
        match frames.next_request() {
            Ok(Some(request)) => match admit(&request) {
                Ok(response) => relay(&mut stream, response)?,
                Err(error) => return refuse(&mut stream, &error),
            },
            Ok(None) => {
                let count = stream
                    .read(&mut buffer)
                    .map_err(|_| BrokerError::ProviderUnavailable)?;
                if count == 0 {
                    return Ok(());
                }
                if let Err(error) = frames.feed(&buffer[..count]) {
                    return refuse(&mut stream, &error.into());
                }
            }
            Err(error) => return refuse(&mut stream, &error.into()),
        }
    }
}

fn relay<S: Write>(stream: &mut S, mut response: UpstreamResponse) -> Result<(), BrokerError> {
    let failed = |_| BrokerError::ProviderUnavailable;
    let content_type = response
        .content_type
        .filter(|value| {
            value
                .bytes()
                .all(|byte| byte.is_ascii_graphic() || byte == b' ')
        })
        .unwrap_or_else(|| "application/octet-stream".into());
    write!(
        stream,
        "HTTP/1.1 {} {}\r\ncontent-type: {content_type}\r\ntransfer-encoding: chunked\r\n\r\n",
        response.status,
        reason(response.status)
    )
    .and_then(|()| stream.flush())
    .map_err(failed)?;
    let mut buffer = vec![0; RELAY_CHUNK];
    loop {
        let count = response.body.read(&mut buffer).map_err(failed)?;
        if count == 0 {
            return stream
                .write_all(b"0\r\n\r\n")
                .and_then(|()| stream.flush())
                .map_err(failed);
        }
        write!(stream, "{count:x}\r\n")
            .and_then(|()| stream.write_all(&buffer[..count]))
            .and_then(|()| stream.write_all(b"\r\n"))
            .and_then(|()| stream.flush())
            .map_err(failed)?;
    }
}

fn refuse<S: Write>(stream: &mut S, error: &BrokerError) -> Result<(), BrokerError> {
    let (status, protocol) = match error {
        BrokerError::Policy(protocol) => (
            if protocol.code == ErrorCode::CredentialUnavailable {
                502
            } else {
                400
            },
            protocol.clone(),
        ),
        BrokerError::ProviderUnavailable => (
            502,
            ProtocolError::new(ErrorCode::BrokerUnavailable, None, None),
        ),
        _ => (
            403,
            ProtocolError::new(ErrorCode::CapabilityDenied, None, None),
        ),
    };
    let body = serde_json::to_vec(&serde_json::json!({ "error": protocol }))
        .map_err(|_| BrokerError::ProviderUnavailable)?;
    // The runtime may already have gone; the refusal itself is the outcome.
    let _ = write!(
        stream,
        "HTTP/1.1 {status} {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        reason(status),
        body.len()
    )
    .and_then(|()| stream.write_all(&body))
    .and_then(|()| stream.flush());
    Ok(())
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        429 => "Too Many Requests",
        502 => "Bad Gateway",
        _ => "Upstream",
    }
}
