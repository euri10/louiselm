//! Authenticated Linux transport for launcher protocol packets.
//!
//! The transport owns only packet framing, kernel credentials, and bounded
//! background I/O. Lifecycle policy and process supervision remain outside
//! this module.

#![cfg(target_os = "linux")]

use std::{
    fmt,
    io::IoSliceMut,
    mem::MaybeUninit,
    os::fd::OwnedFd,
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread,
};

use rustix::{
    cmsg_space,
    fs::{OFlags, fcntl_getfl, fcntl_setfl},
    io::{Errno, fcntl_dupfd_cloexec},
    net::{
        AddressFamily, RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags,
        SendFlags, Shutdown, SocketAddrUnix, SocketFlags, SocketType, UCred, accept_with, bind,
        connect, listen, recvmsg, send, shutdown, socket_with,
        sockopt::{
            set_socket_passcred, set_socket_recv_buffer_size, set_socket_send_buffer_size,
            socket_passcred, socket_peercred, socket_recv_buffer_size, socket_send_buffer_size,
        },
    },
};
use serde::Deserialize;
use thiserror::Error;

use crate::launch_protocol::{
    MAX_PROTOCOL_MESSAGE_BYTES, ProtocolError, ProtocolMessage, ProtocolResponse, RESPONSE_SCHEMA,
    decode_message,
};

/// Maximum bytes in one launcher packet.
pub const MAX_PACKET_BYTES: usize = MAX_PROTOCOL_MESSAGE_BYTES;

const COMMAND_QUEUE_CAPACITY: usize = 8;
const LISTEN_BACKLOG: i32 = COMMAND_QUEUE_CAPACITY as i32;
// Linux reports doubled SO_SNDBUF/SO_RCVBUF values, and Unix seqpacket
// payloads consume a small amount of that space for kernel bookkeeping.
const MIN_KERNEL_SOCKET_BUFFER: usize = MAX_PACKET_BYTES * 2;

/// One exactly-once result delivered by a fixed transport worker.
pub type TransportCompletion<T> = Box<dyn FnOnce(Result<T, TransportError>) + Send + 'static>;

/// Credentials supplied by the Linux kernel for a Unix-domain socket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KernelCredentials {
    /// Host process ID.
    pub pid: u32,
    /// Kernel-reported host user ID.
    pub uid: u32,
    /// Kernel-reported host group ID.
    pub gid: u32,
}

impl From<UCred> for KernelCredentials {
    fn from(credentials: UCred) -> Self {
        Self {
            pid: u32::try_from(credentials.pid.as_raw_pid())
                .expect("Linux SCM_CREDENTIALS contains a positive pid"),
            uid: credentials.uid.as_raw(),
            gid: credentials.gid.as_raw(),
        }
    }
}

/// Kernel identity required for a connection and every packet on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialPin {
    /// Accept any process running under one assigned host identity.
    Identity {
        /// Required kernel user ID.
        uid: u32,
        /// Required kernel group ID.
        gid: u32,
    },
    /// Accept only one exact host process identity.
    Process(KernelCredentials),
}

impl CredentialPin {
    fn matches(self, credentials: KernelCredentials) -> bool {
        match self {
            Self::Identity { uid, gid } => credentials.uid == uid && credentials.gid == gid,
            Self::Process(expected) => credentials == expected,
        }
    }
}

/// One decoded packet in either launcher protocol direction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LauncherPacket {
    /// Broker-to-supervisor request.
    Request(ProtocolMessage),
    /// Correlated supervisor or broker response.
    Response(Box<ProtocolResponse>),
}

/// One protocol packet accompanied by both kernel credential observations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthenticatedPacket {
    /// Exact packet bytes received from the socket.
    pub bytes: Vec<u8>,
    /// Validated launcher protocol value.
    pub packet: LauncherPacket,
    /// Connection peer captured from `SO_PEERCRED`.
    pub peer_credentials: KernelCredentials,
    /// Sender attached to this packet through `SCM_CREDENTIALS`.
    pub message_credentials: KernelCredentials,
}

/// Stable failures at the launcher transport boundary.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum TransportError {
    /// This transport object has been closed.
    #[error("launcher transport is closed")]
    Closed,
    /// The fixed background command queue is full.
    #[error("launcher transport queue is full")]
    QueueFull,
    /// The rendezvous path cannot be represented as a Unix socket address.
    #[error("invalid launcher socket address")]
    InvalidAddress,
    /// A Unix `SOCK_SEQPACKET` socket could not be created.
    #[error("launcher socket creation failed")]
    SocketFailed,
    /// Required socket options could not be established.
    #[error("launcher socket configuration failed")]
    SocketConfigurationFailed,
    /// The rendezvous socket could not be bound.
    #[error("launcher socket bind failed")]
    BindFailed,
    /// The rendezvous socket could not start listening.
    #[error("launcher socket listen failed")]
    ListenFailed,
    /// An incoming connection could not be accepted.
    #[error("launcher socket accept failed")]
    AcceptFailed,
    /// A connection to the rendezvous socket failed.
    #[error("launcher socket connect failed")]
    ConnectFailed,
    /// The listener backlog is full; a later connection attempt may succeed.
    #[error("launcher socket listener is busy")]
    ConnectBusy,
    /// A fixed background worker could not be started.
    #[error("launcher transport worker unavailable")]
    WorkerUnavailable,
    /// The connection peer did not match the caller's kernel identity pin.
    #[error("launcher connection peer credentials did not match")]
    PeerCredentialsMismatch {
        /// Required identity.
        expected: CredentialPin,
        /// Kernel-reported connection peer.
        actual: KernelCredentials,
    },
    /// One packet sender did not match the caller's kernel identity pin.
    #[error("launcher packet credentials did not match")]
    MessageCredentialsMismatch {
        /// Required identity.
        expected: CredentialPin,
        /// Kernel-reported packet sender.
        actual: KernelCredentials,
    },
    /// A non-empty packet arrived without kernel credentials.
    #[error("launcher packet credentials were missing")]
    MissingCredentials,
    /// A packet carried ancillary data other than one credentials record.
    #[error("launcher packet carried unexpected ancillary data")]
    UnexpectedAncillary,
    /// Ancillary data did not fit the fixed receive buffer.
    #[error("launcher packet ancillary data was truncated")]
    TruncatedAncillary,
    /// An outgoing packet exceeded the fixed protocol bound.
    #[error("launcher packet exceeded the size limit")]
    PacketTooLarge,
    /// An incoming packet exceeded the fixed receive buffer.
    #[error("launcher packet was truncated")]
    TruncatedPacket,
    /// A zero-length packet was sent rather than a protocol message.
    #[error("launcher packet was empty")]
    EmptyPacket,
    /// The peer disconnected or shut down the socket.
    #[error("launcher transport peer disconnected")]
    Disconnected,
    /// One packet could not be sent atomically.
    #[error("launcher packet send was partial")]
    PartialSend,
    /// The kernel rejected a packet send operation.
    #[error("launcher packet send failed")]
    SendFailed,
    /// The kernel rejected a packet receive operation.
    #[error("launcher packet receive failed")]
    ReceiveFailed,
    /// Packet bytes failed the closed launcher protocol decoder.
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
}

#[derive(Deserialize)]
struct PacketHeader {
    schema: String,
}

fn decode_packet(bytes: &[u8]) -> Result<LauncherPacket, TransportError> {
    let header: PacketHeader = serde_json::from_slice(bytes).map_err(|_| {
        TransportError::Protocol(ProtocolError::new(
            crate::launch_protocol::ErrorCode::MalformedMessage,
            None,
            None,
        ))
    })?;
    if header.schema == RESPONSE_SCHEMA {
        ProtocolResponse::parse_canonical(bytes)
            .map(Box::new)
            .map(LauncherPacket::Response)
            .map_err(TransportError::Protocol)
    } else {
        decode_message(bytes)
            .map(LauncherPacket::Request)
            .map_err(TransportError::Protocol)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn complete<T>(completion: TransportCompletion<T>, result: Result<T, TransportError>) {
    let _ = catch_unwind(AssertUnwindSafe(|| completion(result)));
}

fn configure_passcred(fd: &OwnedFd) -> Result<(), TransportError> {
    set_socket_passcred(fd, true).map_err(|_| TransportError::SocketConfigurationFailed)?;
    if !socket_passcred(fd).map_err(|_| TransportError::SocketConfigurationFailed)? {
        return Err(TransportError::SocketConfigurationFailed);
    }
    Ok(())
}

fn configure_packet_buffers(fd: &OwnedFd) -> Result<(), TransportError> {
    set_socket_send_buffer_size(fd, MAX_PACKET_BYTES)
        .map_err(|_| TransportError::SocketConfigurationFailed)?;
    set_socket_recv_buffer_size(fd, MAX_PACKET_BYTES)
        .map_err(|_| TransportError::SocketConfigurationFailed)?;
    let send_size =
        socket_send_buffer_size(fd).map_err(|_| TransportError::SocketConfigurationFailed)?;
    let receive_size =
        socket_recv_buffer_size(fd).map_err(|_| TransportError::SocketConfigurationFailed)?;
    if send_size < MIN_KERNEL_SOCKET_BUFFER || receive_size < MIN_KERNEL_SOCKET_BUFFER {
        return Err(TransportError::SocketConfigurationFailed);
    }
    Ok(())
}

fn peer_credentials(fd: &OwnedFd) -> Result<KernelCredentials, TransportError> {
    socket_peercred(fd)
        .map(KernelCredentials::from)
        .map_err(|_| TransportError::SocketConfigurationFailed)
}

fn duplicate(fd: &OwnedFd) -> Result<OwnedFd, TransportError> {
    fcntl_dupfd_cloexec(fd, 0).map_err(|_| TransportError::SocketFailed)
}

fn is_disconnected(error: Errno) -> bool {
    matches!(
        error,
        Errno::PIPE | Errno::CONNRESET | Errno::NOTCONN | Errno::SHUTDOWN
    )
}

struct SendCommand {
    packet: Result<Vec<u8>, TransportError>,
    completion: TransportCompletion<()>,
}

struct ReceiveCommand {
    completion: TransportCompletion<AuthenticatedPacket>,
}

struct ChannelCore {
    control: OwnedFd,
    pin: CredentialPin,
    peer_credentials: KernelCredentials,
    closed: AtomicBool,
    completion_gate: Mutex<()>,
    send_commands: Mutex<Option<SyncSender<SendCommand>>>,
    receive_commands: Mutex<Option<SyncSender<ReceiveCommand>>>,
}

impl ChannelCore {
    fn close(&self) -> bool {
        let _completion = lock(&self.completion_gate);
        self.close_locked()
    }

    fn close_locked(&self) -> bool {
        let newly_closed = !self.closed.swap(true, Ordering::AcqRel);
        if newly_closed {
            lock(&self.send_commands).take();
            lock(&self.receive_commands).take();
            let _ = shutdown(&self.control, Shutdown::Both);
        }
        newly_closed
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    fn finish<T>(
        &self,
        completion: TransportCompletion<T>,
        result: Result<T, TransportError>,
        fatal: bool,
    ) -> bool {
        let completion_result = {
            let _completion = lock(&self.completion_gate);
            if self.is_closed() {
                Err(TransportError::Closed)
            } else {
                if fatal {
                    self.close_locked();
                }
                result
            }
        };
        complete(completion, completion_result);
        self.is_closed()
    }
}

struct ChannelInner {
    core: Arc<ChannelCore>,
}

impl Drop for ChannelInner {
    fn drop(&mut self) {
        self.core.close();
    }
}

/// One authenticated full-duplex `SOCK_SEQPACKET` connection.
#[derive(Clone)]
pub struct SeqpacketChannel {
    inner: Arc<ChannelInner>,
}

impl fmt::Debug for SeqpacketChannel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SeqpacketChannel")
            .field("peer_credentials", &self.peer_credentials())
            .field("closed", &self.is_closed())
            .finish_non_exhaustive()
    }
}

impl SeqpacketChannel {
    fn from_connected(
        fd: OwnedFd,
        pin: CredentialPin,
        peer_credentials: KernelCredentials,
    ) -> Result<Self, TransportError> {
        let send_fd = duplicate(&fd)?;
        let receive_fd = duplicate(&fd)?;
        let (send_commands, send_receiver) = mpsc::sync_channel(COMMAND_QUEUE_CAPACITY);
        let (receive_commands, receive_receiver) = mpsc::sync_channel(COMMAND_QUEUE_CAPACITY);
        let core = Arc::new(ChannelCore {
            control: fd,
            pin,
            peer_credentials,
            closed: AtomicBool::new(false),
            completion_gate: Mutex::new(()),
            send_commands: Mutex::new(Some(send_commands)),
            receive_commands: Mutex::new(Some(receive_commands)),
        });

        let send_core = Arc::clone(&core);
        let send_worker = thread::Builder::new()
            .name("louiselm-launch-send".to_owned())
            .spawn(move || send_loop(send_fd, send_receiver, send_core))
            .map_err(|_| TransportError::WorkerUnavailable)?;
        let receive_core = Arc::clone(&core);
        let receive_worker = match thread::Builder::new()
            .name("louiselm-launch-receive".to_owned())
            .spawn(move || receive_loop(receive_fd, receive_receiver, receive_core))
        {
            Ok(worker) => worker,
            Err(_) => {
                core.close();
                drop(send_worker);
                return Err(TransportError::WorkerUnavailable);
            }
        };
        drop(send_worker);
        drop(receive_worker);

        Ok(Self {
            inner: Arc::new(ChannelInner { core }),
        })
    }

    /// Returns the `SO_PEERCRED` identity captured for this connection.
    #[must_use]
    pub fn peer_credentials(&self) -> KernelCredentials {
        self.inner.core.peer_credentials
    }

    /// Queues one exact packet for validation and atomic sending.
    ///
    /// Queue admission errors are returned synchronously. The completion runs
    /// on the channel's fixed send worker exactly once.
    pub fn send(
        &self,
        bytes: Vec<u8>,
        completion: TransportCompletion<()>,
    ) -> Result<(), TransportError> {
        let packet = if bytes.is_empty() {
            Err(TransportError::EmptyPacket)
        } else if bytes.len() > MAX_PACKET_BYTES {
            Err(TransportError::PacketTooLarge)
        } else {
            Ok(bytes)
        };
        let commands = lock(&self.inner.core.send_commands);
        if self.inner.core.is_closed() {
            return Err(TransportError::Closed);
        }
        let Some(commands) = commands.as_ref() else {
            return Err(TransportError::Closed);
        };
        commands
            .try_send(SendCommand { packet, completion })
            .map_err(map_send_admission)
    }

    /// Queues one authenticated packet receive.
    ///
    /// Queue admission errors are returned synchronously. The completion runs
    /// on the channel's fixed receive worker exactly once.
    pub fn receive(
        &self,
        completion: TransportCompletion<AuthenticatedPacket>,
    ) -> Result<(), TransportError> {
        let commands = lock(&self.inner.core.receive_commands);
        if self.inner.core.is_closed() {
            return Err(TransportError::Closed);
        }
        let Some(commands) = commands.as_ref() else {
            return Err(TransportError::Closed);
        };
        commands
            .try_send(ReceiveCommand { completion })
            .map_err(map_receive_admission)
    }

    /// Closes this channel and wakes both fixed I/O workers.
    pub fn close(&self) -> bool {
        self.inner.core.close()
    }

    /// Whether this channel has entered its terminal closed state.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.inner.core.is_closed()
    }
}

fn map_send_admission(error: TrySendError<SendCommand>) -> TransportError {
    match error {
        TrySendError::Full(_) => TransportError::QueueFull,
        TrySendError::Disconnected(_) => TransportError::Closed,
    }
}

fn map_receive_admission(error: TrySendError<ReceiveCommand>) -> TransportError {
    match error {
        TrySendError::Full(_) => TransportError::QueueFull,
        TrySendError::Disconnected(_) => TransportError::Closed,
    }
}

fn send_loop(fd: OwnedFd, commands: Receiver<SendCommand>, core: Arc<ChannelCore>) {
    while let Ok(command) = commands.recv() {
        if core.is_closed() {
            complete(command.completion, Err(TransportError::Closed));
            drain_sends(&commands);
            return;
        }
        let result = command.packet.and_then(|bytes| send_one(&fd, &bytes));
        let fatal = result.is_err();
        if core.finish(command.completion, result, fatal) {
            drain_sends(&commands);
            return;
        }
    }
}

fn drain_sends(commands: &Receiver<SendCommand>) {
    for command in commands.try_iter() {
        complete(command.completion, Err(TransportError::Closed));
    }
}

fn send_one(fd: &OwnedFd, bytes: &[u8]) -> Result<(), TransportError> {
    debug_assert!(!bytes.is_empty());
    debug_assert!(bytes.len() <= MAX_PACKET_BYTES);
    let _ = decode_packet(bytes)?;
    let sent = loop {
        match send(fd, bytes, SendFlags::NOSIGNAL) {
            Err(Errno::INTR) => continue,
            Err(error) if is_disconnected(error) => return Err(TransportError::Disconnected),
            Err(_) => return Err(TransportError::SendFailed),
            Ok(sent) => break sent,
        }
    };
    if sent != bytes.len() {
        return Err(TransportError::PartialSend);
    }
    Ok(())
}

fn receive_loop(fd: OwnedFd, commands: Receiver<ReceiveCommand>, core: Arc<ChannelCore>) {
    while let Ok(command) = commands.recv() {
        if core.is_closed() {
            complete(command.completion, Err(TransportError::Closed));
            drain_receives(&commands);
            return;
        }
        let result = receive_one(&fd, core.pin, core.peer_credentials);
        let fatal = result.is_err();
        if core.finish(command.completion, result, fatal) {
            drain_receives(&commands);
            return;
        }
    }
}

fn drain_receives(commands: &Receiver<ReceiveCommand>) {
    for command in commands.try_iter() {
        complete(command.completion, Err(TransportError::Closed));
    }
}

fn receive_one(
    fd: &OwnedFd,
    pin: CredentialPin,
    peer_credentials: KernelCredentials,
) -> Result<AuthenticatedPacket, TransportError> {
    let mut bytes = vec![0_u8; MAX_PACKET_BYTES];
    let mut io = [IoSliceMut::new(&mut bytes)];
    let mut ancillary_space = [MaybeUninit::uninit(); cmsg_space!(ScmCredentials(2), ScmRights(4))];
    let mut ancillary = RecvAncillaryBuffer::new(&mut ancillary_space);
    let received = loop {
        match recvmsg(
            fd,
            &mut io,
            &mut ancillary,
            RecvFlags::TRUNC | RecvFlags::CMSG_CLOEXEC,
        ) {
            Err(Errno::INTR) => continue,
            Err(error) if is_disconnected(error) => return Err(TransportError::Disconnected),
            Err(_) => return Err(TransportError::ReceiveFailed),
            Ok(received) => break received,
        }
    };

    let mut credentials = None;
    let mut unexpected_ancillary = false;
    for message in ancillary.drain() {
        match message {
            RecvAncillaryMessage::ScmCredentials(found) => {
                if credentials
                    .replace(KernelCredentials::from(found))
                    .is_some()
                {
                    unexpected_ancillary = true;
                }
            }
            RecvAncillaryMessage::ScmRights(rights) => {
                rights.for_each(drop);
                unexpected_ancillary = true;
            }
            _ => unexpected_ancillary = true,
        }
    }

    if received.flags.contains(ReturnFlags::CTRUNC) {
        return Err(TransportError::TruncatedAncillary);
    }
    if received.flags.contains(ReturnFlags::TRUNC) || received.bytes > MAX_PACKET_BYTES {
        return Err(TransportError::TruncatedPacket);
    }
    if unexpected_ancillary {
        return Err(TransportError::UnexpectedAncillary);
    }
    if received.bytes == 0 {
        return if credentials.is_some() {
            Err(TransportError::EmptyPacket)
        } else {
            Err(TransportError::Disconnected)
        };
    }
    let message_credentials = credentials.ok_or(TransportError::MissingCredentials)?;
    if !pin.matches(message_credentials) {
        return Err(TransportError::MessageCredentialsMismatch {
            expected: pin,
            actual: message_credentials,
        });
    }
    bytes.truncate(received.bytes);
    let packet = decode_packet(&bytes)?;
    Ok(AuthenticatedPacket {
        bytes,
        packet,
        peer_credentials,
        message_credentials,
    })
}

struct AcceptCommand {
    pin: CredentialPin,
    completion: TransportCompletion<SeqpacketChannel>,
}

struct ListenerCore {
    control: OwnedFd,
    closed: AtomicBool,
    completion_gate: Mutex<()>,
    commands: Mutex<Option<SyncSender<AcceptCommand>>>,
}

impl ListenerCore {
    fn close(&self) -> bool {
        let _completion = lock(&self.completion_gate);
        self.close_locked()
    }

    fn close_locked(&self) -> bool {
        let newly_closed = !self.closed.swap(true, Ordering::AcqRel);
        if newly_closed {
            lock(&self.commands).take();
            let _ = shutdown(&self.control, Shutdown::Both);
        }
        newly_closed
    }

    fn finish(
        &self,
        completion: TransportCompletion<SeqpacketChannel>,
        result: Result<SeqpacketChannel, TransportError>,
    ) -> bool {
        let completion_result = {
            let _completion = lock(&self.completion_gate);
            if self.closed.load(Ordering::Acquire) {
                Err(TransportError::Closed)
            } else {
                result
            }
        };
        complete(completion, completion_result);
        self.closed.load(Ordering::Acquire)
    }
}

struct ListenerInner {
    core: Arc<ListenerCore>,
}

impl Drop for ListenerInner {
    fn drop(&mut self) {
        self.core.close();
    }
}

/// Bound Unix `SOCK_SEQPACKET` rendezvous listener.
#[derive(Clone)]
pub struct SeqpacketListener {
    inner: Arc<ListenerInner>,
}

impl SeqpacketListener {
    /// Binds a new listener without deleting or replacing an existing path.
    pub fn bind(path: &Path) -> Result<Self, TransportError> {
        let address = SocketAddrUnix::new(path).map_err(|_| TransportError::InvalidAddress)?;
        let fd = socket_with(
            AddressFamily::UNIX,
            SocketType::SEQPACKET,
            SocketFlags::CLOEXEC,
            None,
        )
        .map_err(|_| TransportError::SocketFailed)?;
        configure_passcred(&fd)?;
        configure_packet_buffers(&fd)?;
        bind(&fd, &address).map_err(|_| TransportError::BindFailed)?;
        listen(&fd, LISTEN_BACKLOG).map_err(|_| TransportError::ListenFailed)?;
        let accept_fd = duplicate(&fd)?;
        let (commands, receiver) = mpsc::sync_channel(COMMAND_QUEUE_CAPACITY);
        let core = Arc::new(ListenerCore {
            control: fd,
            closed: AtomicBool::new(false),
            completion_gate: Mutex::new(()),
            commands: Mutex::new(Some(commands)),
        });
        let worker_core = Arc::clone(&core);
        let worker = thread::Builder::new()
            .name("louiselm-launch-accept".to_owned())
            .spawn(move || accept_loop(accept_fd, receiver, worker_core))
            .map_err(|_| TransportError::WorkerUnavailable)?;
        drop(worker);
        Ok(Self {
            inner: Arc::new(ListenerInner { core }),
        })
    }

    /// Queues one authenticated accept operation.
    pub fn accept(
        &self,
        pin: CredentialPin,
        completion: TransportCompletion<SeqpacketChannel>,
    ) -> Result<(), TransportError> {
        let commands = lock(&self.inner.core.commands);
        if self.inner.core.closed.load(Ordering::Acquire) {
            return Err(TransportError::Closed);
        }
        let Some(commands) = commands.as_ref() else {
            return Err(TransportError::Closed);
        };
        commands
            .try_send(AcceptCommand { pin, completion })
            .map_err(|error| match error {
                TrySendError::Full(_) => TransportError::QueueFull,
                TrySendError::Disconnected(_) => TransportError::Closed,
            })
    }

    /// Closes the listener without unlinking its rendezvous path.
    pub fn close(&self) -> bool {
        self.inner.core.close()
    }

    /// Whether this listener has been closed.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.inner.core.closed.load(Ordering::Acquire)
    }
}

fn accept_loop(fd: OwnedFd, commands: Receiver<AcceptCommand>, core: Arc<ListenerCore>) {
    while let Ok(command) = commands.recv() {
        if core.closed.load(Ordering::Acquire) {
            complete(command.completion, Err(TransportError::Closed));
            drain_accepts(&commands);
            return;
        }
        let result = accept_one(&fd, command.pin);
        if core.finish(command.completion, result) {
            drain_accepts(&commands);
            return;
        }
    }
}

fn drain_accepts(commands: &Receiver<AcceptCommand>) {
    for command in commands.try_iter() {
        complete(command.completion, Err(TransportError::Closed));
    }
}

fn accept_one(fd: &OwnedFd, pin: CredentialPin) -> Result<SeqpacketChannel, TransportError> {
    let accepted = loop {
        match accept_with(fd, SocketFlags::CLOEXEC) {
            Err(Errno::INTR) => continue,
            Err(_) => return Err(TransportError::AcceptFailed),
            Ok(accepted) => break accepted,
        }
    };
    configure_passcred(&accepted)?;
    configure_packet_buffers(&accepted)?;
    let actual = peer_credentials(&accepted)?;
    if !pin.matches(actual) {
        return Err(TransportError::PeerCredentialsMismatch {
            expected: pin,
            actual,
        });
    }
    SeqpacketChannel::from_connected(accepted, pin, actual)
}

struct ConnectCommand {
    path: PathBuf,
    pin: CredentialPin,
    completion: TransportCompletion<SeqpacketChannel>,
}

struct ConnectorCore {
    closed: AtomicBool,
    completion_gate: Mutex<()>,
    commands: Mutex<Option<SyncSender<ConnectCommand>>>,
}

impl ConnectorCore {
    fn close(&self) {
        let _completion = lock(&self.completion_gate);
        self.closed.store(true, Ordering::Release);
        lock(&self.commands).take();
    }

    fn finish(
        &self,
        completion: TransportCompletion<SeqpacketChannel>,
        result: Result<SeqpacketChannel, TransportError>,
    ) -> bool {
        let completion_result = {
            let _completion = lock(&self.completion_gate);
            if self.closed.load(Ordering::Acquire) {
                Err(TransportError::Closed)
            } else {
                result
            }
        };
        complete(completion, completion_result);
        self.closed.load(Ordering::Acquire)
    }
}

struct ConnectorInner {
    core: Arc<ConnectorCore>,
}

impl Drop for ConnectorInner {
    fn drop(&mut self) {
        self.core.close();
    }
}

/// Bounded asynchronous connector for launcher rendezvous sockets.
#[derive(Clone)]
pub struct SeqpacketConnector {
    inner: Arc<ConnectorInner>,
}

impl SeqpacketConnector {
    /// Starts the connector's single fixed worker.
    pub fn new() -> Result<Self, TransportError> {
        let (commands, receiver) = mpsc::sync_channel(COMMAND_QUEUE_CAPACITY);
        let core = Arc::new(ConnectorCore {
            closed: AtomicBool::new(false),
            completion_gate: Mutex::new(()),
            commands: Mutex::new(Some(commands)),
        });
        let worker_core = Arc::clone(&core);
        let worker = thread::Builder::new()
            .name("louiselm-launch-connect".to_owned())
            .spawn(move || connect_loop(receiver, worker_core))
            .map_err(|_| TransportError::WorkerUnavailable)?;
        drop(worker);
        Ok(Self {
            inner: Arc::new(ConnectorInner { core }),
        })
    }

    /// Queues one connection and authenticates its kernel peer identity.
    pub fn connect(
        &self,
        path: &Path,
        pin: CredentialPin,
        completion: TransportCompletion<SeqpacketChannel>,
    ) -> Result<(), TransportError> {
        let commands = lock(&self.inner.core.commands);
        if self.inner.core.closed.load(Ordering::Acquire) {
            return Err(TransportError::Closed);
        }
        let Some(commands) = commands.as_ref() else {
            return Err(TransportError::Closed);
        };
        commands
            .try_send(ConnectCommand {
                path: path.to_owned(),
                pin,
                completion,
            })
            .map_err(|error| match error {
                TrySendError::Full(_) => TransportError::QueueFull,
                TrySendError::Disconnected(_) => TransportError::Closed,
            })
    }
}

fn connect_loop(commands: Receiver<ConnectCommand>, core: Arc<ConnectorCore>) {
    while let Ok(command) = commands.recv() {
        if core.closed.load(Ordering::Acquire) {
            complete(command.completion, Err(TransportError::Closed));
            for command in commands.try_iter() {
                complete(command.completion, Err(TransportError::Closed));
            }
            return;
        }
        let result = connect_one(&command.path, command.pin);
        if core.finish(command.completion, result) {
            for command in commands.try_iter() {
                complete(command.completion, Err(TransportError::Closed));
            }
            return;
        }
    }
}

fn connect_one(path: &Path, pin: CredentialPin) -> Result<SeqpacketChannel, TransportError> {
    let address = SocketAddrUnix::new(path).map_err(|_| TransportError::InvalidAddress)?;
    let fd = socket_with(
        AddressFamily::UNIX,
        SocketType::SEQPACKET,
        SocketFlags::CLOEXEC | SocketFlags::NONBLOCK,
        None,
    )
    .map_err(|_| TransportError::SocketFailed)?;
    configure_passcred(&fd)?;
    configure_packet_buffers(&fd)?;
    loop {
        match connect(&fd, &address) {
            Err(Errno::INTR) => continue,
            Err(Errno::AGAIN | Errno::INPROGRESS | Errno::ALREADY) => {
                return Err(TransportError::ConnectBusy);
            }
            Err(_) => return Err(TransportError::ConnectFailed),
            Ok(()) => break,
        }
    }
    let mut flags = fcntl_getfl(&fd).map_err(|_| TransportError::SocketConfigurationFailed)?;
    flags.remove(OFlags::NONBLOCK);
    fcntl_setfl(&fd, flags).map_err(|_| TransportError::SocketConfigurationFailed)?;
    let actual = peer_credentials(&fd)?;
    if !pin.matches(actual) {
        return Err(TransportError::PeerCredentialsMismatch {
            expected: pin,
            actual,
        });
    }
    SeqpacketChannel::from_connected(fd, pin, actual)
}
