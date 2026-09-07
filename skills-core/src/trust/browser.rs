//! Bounded, single-ceremony loopback HTTP. No daemon, arbitrary RP or remote host.

use super::{passkey::TIMEOUT, recovery::RecoveryError};
use serde_json::{Value, json};
use std::{
    io::{self, Read, Write},
    net::{TcpListener, TcpStream},
    thread,
    time::{Duration, Instant},
};
use webauthn_rs::prelude::Uuid;

const MAX_REQUEST: usize = 65_536;

#[cfg(test)]
#[path = "browser_tests.rs"]
mod tests;

pub(crate) struct Browser {
    listener: TcpListener,
    origin: String,
    prefix: String,
    deadline: Instant,
}

impl Browser {
    pub(crate) fn bind() -> Result<Self, RecoveryError> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let origin = format!("http://localhost:{}", listener.local_addr()?.port());
        Ok(Self {
            listener,
            origin,
            prefix: format!("/{}/", Uuid::new_v4()),
            deadline: Instant::now() + TIMEOUT,
        })
    }

    pub(crate) fn port(&self) -> Result<u16, RecoveryError> {
        Ok(self.listener.local_addr()?.port())
    }

    pub(crate) fn url(&self) -> String {
        format!("{}{}", self.origin, self.prefix)
    }

    pub(crate) fn until(mut self, deadline: Instant) -> Self {
        self.deadline = self.deadline.min(deadline);
        self
    }

    // Ownership closes the listener on success, cancellation, error or timeout.
    pub(crate) fn run<T>(
        self,
        kind: &str,
        action: &Value,
        options: &Value,
        finish: impl FnOnce(Value) -> Result<(T, &'static str), RecoveryError>,
    ) -> Result<T, RecoveryError> {
        let document =
            serde_json::to_vec(&json!({"kind":kind, "action":action, "options":options}))
                .map_err(|_| RecoveryError::Passkey)?;
        loop {
            self.check_deadline()?;
            let mut stream = match self.listener.accept() {
                Ok((stream, _)) => stream,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(20));
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let Ok(request) = self.request(&mut stream) else {
                // Malformed traffic has no authority; keep the bounded ceremony
                // alive so an unrelated localhost probe cannot cancel it.
                let _ = respond(
                    &mut stream,
                    "400 Bad Request",
                    "text/plain",
                    b"Request refused",
                );
                continue;
            };
            match (
                request.method.as_str(),
                request.path.strip_prefix(&self.prefix),
            ) {
                ("GET", Some("")) => respond(
                    &mut stream,
                    "200 OK",
                    "text/html; charset=utf-8",
                    include_bytes!("browser.html"),
                )?,
                ("GET", Some("client.js")) => respond(
                    &mut stream,
                    "200 OK",
                    "text/javascript; charset=utf-8",
                    include_bytes!("browser.js"),
                )?,
                ("GET", Some("ceremony.json")) => {
                    respond(&mut stream, "200 OK", "application/json", &document)?;
                }
                ("POST", Some("cancel")) => {
                    // No callback, and thus no authority write, has occurred.
                    let _ = respond(
                        &mut stream,
                        "200 OK",
                        "application/json",
                        br#"{"message":"Cancelled; trust unchanged."}"#,
                    );
                    return Err(RecoveryError::Cancelled);
                }
                ("POST", Some("finish")) => {
                    self.check_deadline()?;
                    let value = serde_json::from_slice(&request.body)
                        .map_err(|_| RecoveryError::Passkey)?;
                    // A closed peer before commit is cancellation. Disconnection
                    // racing publication is inherently indeterminate to the browser;
                    // the page explicitly tells the operator to inspect trust state.
                    stream.set_nonblocking(true)?;
                    let closed = matches!(stream.peek(&mut [0_u8; 1]), Ok(0));
                    stream.set_nonblocking(false)?;
                    if closed {
                        return Err(RecoveryError::Cancelled);
                    }
                    let result = finish(value);
                    let message = result.as_ref().map_or("Recovery refused; inspect the local terminal. A persistence error may follow publication.", |(_, message)| *message);
                    let body = serde_json::to_vec(&json!({"message":message, "ok":result.is_ok()}))
                        .map_err(|_| RecoveryError::Passkey)?;
                    // A lost result must not turn a completed commit into a false
                    // failure on the trusted CLI. It reports its own durable result.
                    let _ = respond(&mut stream, "200 OK", "application/json", &body);
                    return result.map(|(value, _)| value);
                }
                _ => respond(&mut stream, "404 Not Found", "text/plain", b"Not found")?,
            }
        }
    }

    fn check_deadline(&self) -> Result<(), RecoveryError> {
        if Instant::now() >= self.deadline {
            return Err(RecoveryError::Passkey);
        }
        Ok(())
    }

    fn request(&self, stream: &mut TcpStream) -> Result<Request, RecoveryError> {
        let deadline = self.deadline.min(Instant::now() + Duration::from_secs(2));
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
        let mut bytes = Vec::new();
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or(RecoveryError::Passkey)?;
            stream.set_read_timeout(Some(remaining))?;
            let mut buffer = [0_u8; 4096];
            let count = stream.read(&mut buffer)?;
            if count == 0 || bytes.len() + count > MAX_REQUEST {
                return Err(RecoveryError::Passkey);
            }
            bytes.extend_from_slice(&buffer[..count]);
            let mut headers = [httparse::EMPTY_HEADER; 32];
            let mut parsed = httparse::Request::new(&mut headers);
            let httparse::Status::Complete(offset) =
                parsed.parse(&bytes).map_err(|_| RecoveryError::Passkey)?
            else {
                continue;
            };
            let method = parsed.method.ok_or(RecoveryError::Passkey)?;
            let path = parsed.path.ok_or(RecoveryError::Passkey)?;
            let header = |name: &str| -> Result<Option<&[u8]>, RecoveryError> {
                let mut matching = parsed
                    .headers
                    .iter()
                    .filter(|header| header.name.eq_ignore_ascii_case(name));
                let value = matching.next().map(|header| header.value);
                if matching.next().is_some() {
                    return Err(RecoveryError::Passkey);
                }
                Ok(value)
            };
            if parsed.version != Some(1)
                || header("host")? != Some(self.origin.trim_start_matches("http://").as_bytes())
                || header("transfer-encoding")?.is_some()
                || header("sec-fetch-site")?
                    .is_some_and(|site| site != b"same-origin" && site != b"none")
                || (method == "POST"
                    && (header("origin")? != Some(self.origin.as_bytes())
                        || header("content-type")? != Some(b"application/json")))
            {
                return Err(RecoveryError::Passkey);
            }
            let length = header("content-length")?
                .map(|raw| {
                    std::str::from_utf8(raw)
                        .ok()
                        .and_then(|raw| raw.parse::<usize>().ok())
                        .ok_or(RecoveryError::Passkey)
                })
                .transpose()?
                .unwrap_or(0);
            if length > MAX_REQUEST - offset || (method == "GET" && length != 0) {
                return Err(RecoveryError::Passkey);
            }
            if bytes.len() < offset + length {
                continue;
            }
            if bytes.len() != offset + length {
                return Err(RecoveryError::Passkey);
            }
            return Ok(Request {
                method: method.to_owned(),
                path: path.to_owned(),
                body: bytes[offset..].to_vec(),
            });
        }
    }
}

struct Request {
    method: String,
    path: String,
    body: Vec<u8>,
}

fn respond(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
) -> io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: default-src 'none'; script-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    stream.flush()
}
