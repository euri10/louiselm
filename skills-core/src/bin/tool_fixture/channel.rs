//! Bounded fixture client for a fixed supervisor-owned socket mount.
//! The privileged server authenticates the client's actual kernel process.
//! The client relies on the immutable socket mount, not an ancestor PID that
//! cannot be represented inside its private PID/user namespaces.

use louiselm_skills::launch_protocol::{
    CommandMessage, MAX_PROTOCOL_MESSAGE_BYTES, ProtocolMessage, decode_message,
};
use rustix::net::{
    self, AddressFamily, RecvFlags, SendFlags, SocketAddrUnix, SocketFlags, SocketType,
    sockopt::{self, Timeout},
};
use std::{io, os::fd::OwnedFd, path::Path, time::Duration};

pub(super) struct Channel(pub(super) OwnedFd);

impl Channel {
    pub(super) fn connect(path: &Path) -> io::Result<Self> {
        let fd = net::socket_with(
            AddressFamily::UNIX,
            SocketType::SEQPACKET,
            SocketFlags::CLOEXEC,
            None,
        )?;
        sockopt::set_socket_send_buffer_size(&fd, MAX_PROTOCOL_MESSAGE_BYTES)?;
        sockopt::set_socket_recv_buffer_size(&fd, MAX_PROTOCOL_MESSAGE_BYTES)?;
        for direction in [Timeout::Send, Timeout::Recv] {
            sockopt::set_socket_timeout(&fd, direction, Some(Duration::from_secs(35)))?;
        }
        net::connect(&fd, &SocketAddrUnix::new(path)?)?;
        Ok(Self(fd))
    }

    pub(super) fn send(&self, bytes: &[u8]) -> io::Result<()> {
        if bytes.is_empty() || bytes.len() > MAX_PROTOCOL_MESSAGE_BYTES {
            return Err(io::Error::other("invalid fixture packet size"));
        }
        if net::send(&self.0, bytes, SendFlags::NOSIGNAL)? != bytes.len() {
            return Err(io::Error::other("incomplete fixture packet"));
        }
        Ok(())
    }

    pub(super) fn receive(&self) -> io::Result<CommandMessage> {
        let mut bytes = vec![0; MAX_PROTOCOL_MESSAGE_BYTES];
        let (_, count) = net::recv(&self.0, bytes.as_mut_slice(), RecvFlags::TRUNC)?;
        if count == 0 || count > bytes.len() {
            return Err(io::Error::other("invalid fixture response size"));
        }
        match decode_message(&bytes[..count]).map_err(io::Error::other)? {
            ProtocolMessage::Command(message) => Ok(message),
            _ => Err(io::Error::other("unexpected fixture response")),
        }
    }
}
