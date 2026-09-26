//! Measured fixture-only local Provider endpoint request, never a vendor call.

use std::{
    io::{self, BufRead, Read, Write},
    net::{Shutdown, SocketAddr, TcpStream},
    time::Duration,
};

pub(super) fn exchange(input: &mut impl BufRead, output: &mut impl Write) -> io::Result<()> {
    let mut address = Vec::new();
    input.take(128).read_until(b'\n', &mut address)?;
    if address.last() != Some(&b'\n') {
        return Err(io::Error::other("Provider fixture address incomplete"));
    }
    address.pop();
    let line = std::str::from_utf8(&address).map_err(io::Error::other)?;
    let (address, variant) = line
        .split_once(' ')
        .ok_or_else(|| io::Error::other("Provider fixture variant missing"))?;
    let address: SocketAddr = address.parse().map_err(io::Error::other)?;
    if !address.ip().is_loopback() {
        return Err(io::Error::other("Provider fixture address is not loopback"));
    }
    let mut connection = TcpStream::connect_timeout(&address, Duration::from_secs(5))?;
    connection.set_read_timeout(Some(Duration::from_secs(15)))?;
    connection.set_write_timeout(Some(Duration::from_secs(5)))?;
    let (body, count) = match variant {
        "valid-batch" => (
            r#"{"model":"fixture-model","input":[],"reasoning":{"effort":"low"},"stream":true}"#,
            2,
        ),
        "valid-once" => (
            r#"{"model":"fixture-model","input":[],"reasoning":{"effort":"low"},"stream":true}"#,
            1,
        ),
        "bad-model" => (
            r#"{"model":"unapproved-model","input":[],"reasoning":{"effort":"low"},"stream":true}"#,
            1,
        ),
        "bad-disclosure" => (
            r#"{"model":"fixture-model","input":[],"reasoning":{"effort":"low"},"stream":true,"client_metadata":{"unknown":"not reviewed"}}"#,
            1,
        ),
        _ => return Err(io::Error::other("unknown Provider fixture variant")),
    };
    let frame = format!(
        "POST /v1/responses HTTP/1.1\r\nhost: {address}\r\naccept: text/event-stream\r\ncontent-type: application/json\r\nsession-id: fixture\r\ncontent-length: {}\r\n\r\n{body}",
        body.len(),
    );
    connection.write_all(frame.repeat(count).as_bytes())?;
    connection.shutdown(Shutdown::Write)?;
    let mut answer = Vec::new();
    (&mut connection)
        .take(64 * 1024 + 1)
        .read_to_end(&mut answer)?;
    if answer.len() > 64 * 1024 {
        return Err(io::Error::other("Provider fixture response exceeded bound"));
    }
    let length = u32::try_from(answer.len()).map_err(io::Error::other)?;
    output.write_all(b"\x1b")?;
    output.write_all(&length.to_be_bytes())?;
    output.write_all(&answer)?;
    output.flush()
}
