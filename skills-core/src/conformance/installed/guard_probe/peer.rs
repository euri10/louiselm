//! Unprivileged, fixed sentinel peers for the installed guard probe.

use crate::{
    conformance::Outcome,
    launch_protocol::{ProtocolResponse, ResponseResult},
};
use rustix::net::{self, RecvAncillaryBuffer, RecvAncillaryMessage};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{self, BufRead, IoSliceMut, Read, Write},
    net::{SocketAddr, TcpStream},
    os::{fd::OwnedFd, unix::net::UnixListener},
    path::PathBuf,
    time::Duration,
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) enum Request {
    Runtime,
    Broker {
        channel: PathBuf,
        handoff: PathBuf,
        owner: u32,
    },
    Enrollment,
    Send(SocketAddr),
    Take,
    Write,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) enum Reply {
    Ready,
    Observed(Outcome),
}

struct Broker {
    listener: OwnedFd,
    channel: Option<OwnedFd>,
    handoff: UnixListener,
    owner: u32,
    endpoint: Option<[OwnedFd; 3]>,
    // Socket is dropped before its namespace leases.
    upstream: Option<(TcpStream, File, File)>,
}

fn invalid() -> io::Error {
    io::ErrorKind::InvalidData.into()
}

fn check_owner(socket: &impl std::os::fd::AsFd, owner: u32) -> io::Result<()> {
    let credentials = net::sockopt::socket_peercred(socket)?;
    if credentials.uid.as_raw() != 0
        || credentials.pid.as_raw_nonzero().get().cast_unsigned() != owner
    {
        return Err(invalid());
    }
    Ok(())
}

impl Broker {
    fn new(channel: &std::path::Path, handoff: &std::path::Path, owner: u32) -> io::Result<Self> {
        if rustix::process::geteuid().is_root() || owner <= 1 {
            return Err(invalid());
        }
        let listener = net::socket_with(
            net::AddressFamily::UNIX,
            net::SocketType::SEQPACKET,
            net::SocketFlags::CLOEXEC,
            None,
        )?;
        net::bind(&listener, &net::SocketAddrUnix::new(channel)?)?;
        net::listen(&listener, 1)?;
        Ok(Self {
            listener,
            channel: None,
            handoff: UnixListener::bind(handoff)?,
            owner,
            endpoint: None,
            upstream: None,
        })
    }

    fn enrollment(&mut self) -> io::Result<()> {
        let socket = net::accept_with(&self.listener, net::SocketFlags::CLOEXEC)?;
        check_owner(&socket, self.owner)?;
        let mut bytes = [0; 8192];
        let (count, descriptors) = receive_descriptors(&socket, &mut bytes)?;
        let mut response: ProtocolResponse =
            serde_json::from_slice(&bytes[..count]).map_err(|_| invalid())?;
        response.validate().map_err(|_| invalid())?;
        let ResponseResult::SenderGuardEnrolled { enrollment } = response.result else {
            return Err(invalid());
        };
        if enrollment.broker_pid != std::process::id() {
            return Err(invalid());
        }
        response.result = ResponseResult::SenderGuardAccepted { enrollment };
        net::send(
            &socket,
            &response.canonical_bytes(),
            net::SendFlags::NOSIGNAL,
        )?;
        self.endpoint = Some(descriptors);
        self.channel = Some(socket);
        Ok(())
    }

    fn take(&mut self) -> io::Result<()> {
        if self.upstream.is_some() || self.channel.is_none() {
            return Err(invalid());
        }
        let (channel, _) = self.handoff.accept()?;
        check_owner(&channel, self.owner)?;
        let mut byte = [0];
        let (count, [socket, pins, network]) = receive_descriptors(&channel, &mut byte)?;
        if count != 1 || byte != *b"G" {
            return Err(invalid());
        }
        let socket = TcpStream::from(socket);
        socket.set_write_timeout(Some(Duration::from_secs(2)))?;
        self.upstream = Some((socket, File::from(pins), File::from(network)));
        Ok(())
    }
}

fn receive_descriptors(
    socket: &impl std::os::fd::AsFd,
    bytes: &mut [u8],
) -> io::Result<(usize, [OwnedFd; 3])> {
    let mut space = [std::mem::MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(3))];
    let mut ancillary = RecvAncillaryBuffer::new(&mut space);
    let message = net::recvmsg(
        socket,
        &mut [IoSliceMut::new(bytes)],
        &mut ancillary,
        net::RecvFlags::CMSG_CLOEXEC,
    )?;
    if message
        .flags
        .intersects(net::ReturnFlags::TRUNC | net::ReturnFlags::CTRUNC)
    {
        return Err(invalid());
    }
    let mut descriptors = Vec::new();
    for item in ancillary.drain() {
        match item {
            RecvAncillaryMessage::ScmRights(rights) => descriptors.extend(rights),
            _ => return Err(invalid()),
        }
    }
    Ok((
        message.bytes,
        descriptors.try_into().map_err(|_| invalid())?,
    ))
}

fn observe(result: io::Result<()>) -> Outcome {
    match result {
        Ok(()) => Outcome::Allowed,
        Err(error) if error.raw_os_error() == Some(rustix::io::Errno::PERM.raw_os_error()) => {
            Outcome::Denied("send EPERM".into())
        }
        Err(_) => Outcome::Error("guard sentinel write unavailable".into()),
    }
}

/// Serve fixed unprivileged sentinel operations; never loads BPF or grants authority.
/// The certifier owns this process and bounds its lifetime and input.
/// # Errors
/// Refuses malformed commands, foreign peers, incomplete handoff or failed I/O.
pub fn serve() -> io::Result<()> {
    if rustix::process::geteuid().is_root() {
        return Err(invalid());
    }
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut broker = None;
    loop {
        let mut line = Vec::new();
        if input.by_ref().take(8193).read_until(b'\n', &mut line)? == 0 {
            return Ok(());
        }
        if line.len() > 8192 || !line.ends_with(b"\n") {
            return Err(invalid());
        }
        let request: Request = serde_json::from_slice(&line).map_err(|_| invalid())?;
        let reply = match request {
            Request::Runtime => Reply::Ready,
            Request::Broker {
                channel,
                handoff,
                owner,
            } if broker.is_none() => {
                broker = Some(Broker::new(&channel, &handoff, owner)?);
                Reply::Ready
            }
            Request::Enrollment => {
                broker.as_mut().ok_or_else(invalid)?.enrollment()?;
                Reply::Ready
            }
            Request::Take => {
                broker.as_mut().ok_or_else(invalid)?.take()?;
                Reply::Ready
            }
            Request::Write => Reply::Observed(observe(
                broker
                    .as_mut()
                    .and_then(|peer| peer.upstream.as_mut())
                    .ok_or_else(invalid)?
                    .0
                    .write_all(b"sentinel\n"),
            )),
            Request::Send(address) if address.ip().is_loopback() => Reply::Observed(observe(
                TcpStream::connect_timeout(&address, Duration::from_secs(2)).and_then(
                    |mut stream| {
                        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
                        stream.write_all(b"sentinel\n")
                    },
                ),
            )),
            _ => return Err(invalid()),
        };
        serde_json::to_writer(io::stdout(), &reply)?;
        io::stdout().write_all(b"\n")?;
        io::stdout().flush()?;
    }
}
