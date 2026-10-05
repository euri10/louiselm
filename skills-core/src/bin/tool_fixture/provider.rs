//! Measured fixture-only local Provider endpoint request, never a vendor call.

use std::{
    io::{self, BufRead, Read, Write},
    net::{Shutdown, SocketAddr, TcpStream},
    time::Duration,
};

pub(super) fn exchange(
    input: &mut impl BufRead,
    output: &mut impl Write,
    retained: &mut Option<TcpStream>,
) -> io::Result<()> {
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
    if variant == "probe-retained" {
        let mut connection = retained
            .take()
            .ok_or_else(|| io::Error::other("no retained Provider socket"))?;
        let answer = match connection.write_all(b"old revision probe") {
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => b"DENIED".as_slice(),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
                ) =>
            {
                b"CLOSED".as_slice()
            }
            Ok(()) => b"ALLOWED".as_slice(),
            Err(error) => return Err(error),
        };
        return write_answer(output, answer);
    }
    if variant == "guard-probe" {
        let result = TcpStream::connect_timeout(&address, Duration::from_secs(1))
            .and_then(|mut connection| connection.write_all(b"guard probe"));
        let answer = match result {
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => b"DENIED".as_slice(),
            Ok(()) => b"ALLOWED".as_slice(),
            Err(error) => return Err(error),
        };
        return write_answer(output, answer);
    }
    let mut connection = TcpStream::connect_timeout(&address, Duration::from_secs(5))?;
    connection.set_read_timeout(Some(Duration::from_secs(15)))?;
    connection.set_write_timeout(Some(Duration::from_secs(5)))?;
    if variant == "retain-socket" {
        if retained.is_some() {
            return Err(io::Error::other("Provider socket already retained"));
        }
        // Pin an accepted local connection to this revision without a complete
        // request or an upstream attempt. Park must close its broker owner.
        connection.write_all(b"POST /v1/responses HTTP/1.1\r\n")?;
        *retained = Some(connection);
        return write_answer(output, b"RETAINED");
    }
    let (body, count) = match variant {
        "valid-batch" => (
            r#"{"model":"fixture-model","input":[],"reasoning":{"effort":"low"},"stream":true}"#,
            2,
        ),
        "valid-once" => (
            r#"{"model":"fixture-model","input":[],"reasoning":{"effort":"low"},"stream":true}"#,
            1,
        ),
        "bad-host" => ("", 1),
        "bad-model" => (
            r#"{"model":"unapproved-model","input":[],"reasoning":{"effort":"low"},"stream":true}"#,
            1,
        ),
        "bad-effort" => (
            r#"{"model":"fixture-model","input":[],"reasoning":{"effort":"xhigh"},"stream":true}"#,
            1,
        ),
        "bad-disclosure" => (
            r#"{"model":"fixture-model","input":[],"reasoning":{"effort":"low"},"stream":true,"client_metadata":{"unknown":"not reviewed"}}"#,
            1,
        ),
        _ => return Err(io::Error::other("unknown Provider fixture variant")),
    };
    let host = if variant == "bad-host" {
        "unapproved.invalid".to_owned()
    } else {
        address.to_string()
    };
    let frame = format!(
        "POST /v1/responses HTTP/1.1\r\nhost: {host}\r\naccept: text/event-stream\r\ncontent-type: application/json\r\nsession-id: fixture\r\ncontent-length: {}\r\n\r\n{body}",
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
    write_answer(output, &answer)
}

fn write_answer(output: &mut impl Write, answer: &[u8]) -> io::Result<()> {
    let length = u32::try_from(answer.len()).map_err(io::Error::other)?;
    output.write_all(b"\x1b")?;
    output.write_all(&length.to_be_bytes())?;
    output.write_all(answer)?;
    output.flush()
}
