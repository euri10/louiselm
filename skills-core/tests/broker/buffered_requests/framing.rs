use super::*;
use serde::Deserialize;

const MAX_BUFFER: usize = 16 * 1024;
const MAX_HEADER: usize = 4096;
const MAX_BODY: usize = 4096;

fn invalid() -> ProtocolError {
    ProtocolError::new(ErrorCode::InvalidRequest, None, None)
}

// Deliberately synthetic, closed operation fixture. It is not a claim about
// all stock Codex fields; .3.9 must supply observed real ACP request fixtures.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Operation {
    model: String,
    input: String,
    stream: bool,
}

#[derive(Default)]
pub(super) struct Frames(Vec<u8>);

impl Frames {
    pub(super) fn feed(&mut self, bytes: &[u8]) -> Result<(), ProtocolError> {
        if bytes.len() > MAX_BUFFER.saturating_sub(self.0.len()) {
            return Err(ProtocolError::new(ErrorCode::MessageTooLarge, None, None));
        }
        self.0.extend_from_slice(bytes);
        Ok(())
    }

    pub(super) fn next(&mut self) -> Result<Option<String>, ProtocolError> {
        let Some(end) = self.0.windows(4).position(|bytes| bytes == b"\r\n\r\n") else {
            return if self.0.len() > MAX_HEADER {
                Err(invalid())
            } else {
                Ok(None)
            };
        };
        let end = end + 4;
        if end > MAX_HEADER {
            return Err(invalid());
        }
        let header = &self.0[..end];
        // httparse permits bare LF; this fixture explicitly accepts only CRLF.
        if header
            .windows(2)
            .any(|pair| pair[1] == b'\n' && pair[0] != b'\r')
        {
            return Err(invalid());
        }
        let mut headers = [httparse::EMPTY_HEADER; 8];
        let mut parsed = httparse::Request::new(&mut headers);
        if parsed.parse(header).map_err(|_| invalid())? != httparse::Status::Complete(end)
            || parsed.method != Some("POST")
            || parsed.path != Some("/v1/responses")
            || parsed.version != Some(1)
        {
            return Err(invalid());
        }
        let (mut host, mut content_type, mut length) = (None, None, None);
        for field in parsed.headers {
            let slot = if field.name.eq_ignore_ascii_case("host") {
                &mut host
            } else if field.name.eq_ignore_ascii_case("content-type") {
                &mut content_type
            } else if field.name.eq_ignore_ascii_case("content-length") {
                &mut length
            } else {
                return Err(invalid());
            };
            if slot.replace(field.value).is_some() {
                return Err(invalid());
            }
        }
        if host != Some(b"localhost".as_slice())
            || content_type != Some(b"application/json".as_slice())
        {
            return Err(invalid());
        }
        let length = length.ok_or_else(invalid)?;
        if length.is_empty() || !length.iter().all(u8::is_ascii_digit) {
            return Err(invalid());
        }
        let length: usize = std::str::from_utf8(length)
            .map_err(|_| invalid())?
            .parse()
            .map_err(|_| invalid())?;
        if length == 0 || length > MAX_BODY {
            return Err(invalid());
        }
        if self.0.len() < end + length {
            return Ok(None);
        }
        let operation: Operation =
            serde_json::from_slice(&self.0[end..end + length]).map_err(|_| invalid())?;
        if operation.input != "synthetic" || !operation.stream {
            return Err(invalid());
        }
        self.0.drain(..end + length);
        Ok(Some(operation.model))
    }
}
