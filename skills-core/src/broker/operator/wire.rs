//! Length-prefixed local frames with one total deadline, including slow peers.
use std::{
    io::{self, Read, Write},
    os::unix::net::UnixStream,
    time::Instant,
};

const MAX: usize = crate::launch_protocol::MAX_PROTOCOL_MESSAGE_BYTES;

pub(super) fn connect(path: &std::path::Path) -> io::Result<UnixStream> {
    use rustix::net::{
        AddressFamily, SocketAddrUnix, SocketFlags, SocketType, connect, socket_with,
    };
    let fd = socket_with(
        AddressFamily::UNIX,
        SocketType::STREAM,
        SocketFlags::CLOEXEC | SocketFlags::NONBLOCK,
        None,
    )?;
    // Linux Unix connect completes immediately or refuses a full backlog with
    // EAGAIN. Never let an unresponsive listener park the operator indefinitely.
    connect(&fd, &SocketAddrUnix::new(path)?)?;
    let stream = UnixStream::from(fd);
    stream.set_nonblocking(false)?;
    Ok(stream)
}

fn remaining(deadline: Instant) -> io::Result<std::time::Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "inspection deadline"))
}

fn read_exact(stream: &mut UnixStream, mut bytes: &mut [u8], deadline: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        stream.set_read_timeout(Some(remaining(deadline)?))?;
        match stream.read(bytes) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(count) => bytes = &mut bytes[count..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

pub(super) fn read(stream: &mut UnixStream, deadline: Instant) -> io::Result<Vec<u8>> {
    let mut header = [0; 4];
    read_exact(stream, &mut header, deadline)?;
    let size =
        usize::try_from(u32::from_be_bytes(header)).map_err(|_| io::ErrorKind::InvalidData)?;
    if size == 0 || size > MAX {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut bytes = vec![0; size];
    read_exact(stream, &mut bytes, deadline)?;
    Ok(bytes)
}

pub(super) fn write(stream: &mut UnixStream, bytes: &[u8], deadline: Instant) -> io::Result<()> {
    if bytes.is_empty() || bytes.len() > MAX {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let size = u32::try_from(bytes.len()).map_err(|_| io::ErrorKind::InvalidData)?;
    for mut bytes in [&size.to_be_bytes()[..], bytes] {
        while !bytes.is_empty() {
            stream.set_write_timeout(Some(remaining(deadline)?))?;
            match stream.write(bytes) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(count) => bytes = &bytes[count..],
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
    }
    Ok(())
}
