//! Fixed manager-started first-registration operation.

use std::{
    fs::File,
    io::{self, IoSliceMut, Read, Write},
    mem::MaybeUninit,
    os::{
        fd::{AsFd, AsRawFd},
        unix::fs::MetadataExt,
    },
    path::PathBuf,
};

use rustix::{
    fs::{Gid, Mode, OFlags, Uid, fchmod, fchown, openat},
    net::{self, RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{TlsIdentity, hex::lower_hex};

use super::evidence::first_init::{
    FirstStoreInput, StoreOwner, initialize_first_stores, verify_first_stores,
};

/// Run the fixed manager-owned phone registration program.
///
/// # Errors
///
/// Returns an error if administrator setup, manager descriptors, or the fixed
/// registration request cannot be validated or completed.
pub fn run_managed_registration_writer() -> io::Result<()> {
    let args: Vec<_> = std::env::args_os().collect();
    match args.get(1).and_then(|value| value.to_str()) {
        Some("prepare") if args.len() == 3 => {
            let fd: u32 = args[2]
                .to_str()
                .ok_or_else(|| io::Error::other("private descriptor unavailable"))?
                .parse()
                .map_err(io::Error::other)?;
            if fd < 3 {
                return Err(io::Error::other("private descriptor unavailable"));
            }
            let private = File::open(format!("/proc/self/fd/{fd}"))?;
            let installed = File::open("/var/lib/louiselm-phone-authority")?;
            let actual = private.metadata()?;
            let expected = installed.metadata()?;
            if !actual.is_dir()
                || actual.uid() != 0
                || actual.mode() & 0o7777 != 0o700
                || (actual.dev(), actual.ino()) != (expected.dev(), expected.ino())
            {
                return Err(io::Error::other("private state unavailable"));
            }
            let result = prepare_from_private(&private)?;
            let mut output = io::stdout().lock();
            serde_json::to_writer(&mut output, &result).map_err(io::Error::other)?;
            output.write_all(b"\n")
        }
        Some("serve") if args.len() == 2 => serve_manager(),
        _ => Err(io::Error::other("invalid writer operation")),
    }
}

#[derive(Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct CandidateReceipt {
    v: u8,
    binding: String,
    receiver: String,
    certificate_sha256: String,
    key_sha256: String,
}

#[derive(Clone, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct DirectoryBinding {
    filesystem_uuid: String,
    handle_type: u32,
    handle: String,
}

#[derive(Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct PrivateSetup {
    version: u8,
    filesystem_uuid: String,
    handle_type: u32,
    handle: String,
    owner_parent: DirectoryBinding,
    capture_stage: DirectoryBinding,
    parent_stage: DirectoryBinding,
    operator_uid: u32,
    operator_gid: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RegisteredCatalog {
    version: u8,
    filesystem_uuid: String,
    handle_type: u32,
    handle: String,
    owner_parent: DirectoryBinding,
    capture_stage: DirectoryBinding,
    parent_stage: DirectoryBinding,
    operator_uid: u32,
    operator_gid: u32,
    receiver: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HostReference {
    version: u8,
    boot_id: String,
    net_device: u64,
    net_inode: u64,
    catalog_sha256: String,
}

struct ManagerFiles {
    root: File,
    parent: File,
    capture_stage: File,
    parent_stage: File,
    private: File,
    catalog: File,
    reference: File,
    hostproc: File,
    hostnet: File,
}

fn manager_files() -> io::Result<ManagerFiles> {
    let invalid = || io::Error::other("manager descriptors unavailable");
    if std::env::var("LISTEN_PID").ok().as_deref() != Some(&std::process::id().to_string())
        || std::env::var("LISTEN_FDS").ok().as_deref() != Some("9")
    {
        return Err(invalid());
    }
    let names = std::env::var("LISTEN_FDNAMES").map_err(|_| invalid())?;
    let names = names.strip_prefix("connection:").unwrap_or(&names);
    let names: Vec<_> = names.split(':').collect();
    if names.len() != 9 {
        return Err(invalid());
    }
    let open = |name: &str| -> io::Result<File> {
        let mut indexes = names
            .iter()
            .enumerate()
            .filter(|(_, value)| **value == name);
        let index = indexes.next().ok_or_else(invalid)?.0;
        if indexes.next().is_some() {
            return Err(invalid());
        }
        File::open(format!("/proc/self/fd/{}", index + 3))
    };
    Ok(ManagerFiles {
        root: open("root")?,
        parent: open("parent")?,
        capture_stage: open("capture_stage")?,
        parent_stage: open("parent_stage")?,
        private: open("authority")?,
        catalog: open("catalog")?,
        reference: open("reference")?,
        hostproc: open("hostproc")?,
        hostnet: open("hostnet")?,
    })
}

fn same_object(left: &File, right: &File) -> io::Result<bool> {
    let left = left.metadata()?;
    let right = right.metadata()?;
    Ok((left.dev(), left.ino()) == (right.dev(), right.ino()))
}

fn manager_record(file: &File) -> io::Result<Vec<u8>> {
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.nlink() != 1
        || metadata.mode() & 0o7777 != 0o444
        || metadata.len() > 65536
    {
        return Err(io::Error::other("manager record unavailable"));
    }
    let mut bytes = Vec::new();
    file.take(65537).read_to_end(&mut bytes)?;
    if bytes.len() > 65536 {
        return Err(io::Error::other("manager record unavailable"));
    }
    Ok(bytes)
}

fn manager_context(files: &ManagerFiles) -> io::Result<(PrivateSetup, String, String)> {
    let private = files.private.metadata()?;
    if !private.is_dir() || private.uid() != 0 || private.mode() & 0o7777 != 0o700 {
        return Err(io::Error::other("private state unavailable"));
    }
    let setup_bytes = private_read(&files.private, "setup.json", 0)?;
    let setup = validate_setup(&setup_bytes)?;
    let catalog_bytes = manager_record(&files.catalog)?;
    let catalog: RegisteredCatalog =
        serde_json::from_slice(&catalog_bytes).map_err(io::Error::other)?;
    super::fixed::<32>(&catalog.receiver).map_err(|_| io::Error::other("catalog unavailable"))?;
    let selected = PrivateSetup {
        version: catalog.version,
        filesystem_uuid: catalog.filesystem_uuid,
        handle_type: catalog.handle_type,
        handle: catalog.handle,
        owner_parent: catalog.owner_parent,
        capture_stage: catalog.capture_stage,
        parent_stage: catalog.parent_stage,
        operator_uid: catalog.operator_uid,
        operator_gid: catalog.operator_gid,
    };
    if setup != selected {
        return Err(io::Error::other("catalog binding changed"));
    }
    let reference_bytes = manager_record(&files.reference)?;
    let reference: HostReference =
        serde_json::from_slice(&reference_bytes).map_err(io::Error::other)?;
    let proc = files.hostproc.metadata()?;
    let actual_proc = std::fs::metadata("/proc")?;
    let hostnet = files.hostnet.metadata()?;
    let current_net = File::from(openat(
        &files.hostproc,
        "self/ns/net",
        OFlags::RDONLY | OFlags::CLOEXEC,
        Mode::empty(),
    )?)
    .metadata()?;
    let mut boot = String::new();
    File::from(openat(
        &files.hostproc,
        "sys/kernel/random/boot_id",
        OFlags::RDONLY | OFlags::CLOEXEC,
        Mode::empty(),
    )?)
    .take(128)
    .read_to_string(&mut boot)?;
    if reference.version != 1
        || reference.catalog_sha256 != lower_hex(&Sha256::digest(catalog_bytes))
        || reference.boot_id != boot.trim()
        || !proc.is_dir()
        || (proc.dev(), proc.ino()) != (actual_proc.dev(), actual_proc.ino())
        || (hostnet.dev(), hostnet.ino()) != (current_net.dev(), current_net.ino())
        || (reference.net_device, reference.net_inode) != (hostnet.dev(), hostnet.ino())
    {
        return Err(io::Error::other("host reference unavailable"));
    }
    let root = files.root.metadata()?;
    let parent = files.parent.metadata()?;
    let capture_stage = files.capture_stage.metadata()?;
    let parent_stage = files.parent_stage.metadata()?;
    let child = File::from(openat(
        &files.parent,
        "capture",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    if !root.is_dir()
        || !parent.is_dir()
        || !same_object(&files.root, &child)?
        || !capture_stage.is_dir()
        || !parent_stage.is_dir()
        || capture_stage.uid() != 0
        || parent_stage.uid() != 0
        || capture_stage.mode() & 0o7777 != 0o700
        || parent_stage.mode() & 0o7777 != 0o700
        || root.dev() != capture_stage.dev()
        || parent.dev() != parent_stage.dev()
    {
        return Err(io::Error::other("registered directories unavailable"));
    }
    Ok((
        setup,
        catalog.receiver,
        lower_hex(&Sha256::digest(setup_bytes)),
    ))
}

fn verified_first_origin(_files: &ManagerFiles) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "independent first-origin verification unavailable",
    ))
}

enum RegistrationFailure {
    Context,
    Exclusion,
    Origin,
    Candidate,
    Commit,
}

impl RegistrationFailure {
    fn code(&self) -> &'static str {
        match self {
            Self::Context => "context_unavailable",
            Self::Exclusion => "exclusion_unavailable",
            Self::Origin => "origin_unavailable",
            Self::Candidate => "candidate_unavailable",
            Self::Commit => "commit_unavailable",
        }
    }
}

fn register_first(files: &ManagerFiles) -> Result<String, RegistrationFailure> {
    let (setup, receiver, binding) =
        manager_context(files).map_err(|_| RegistrationFailure::Context)?;
    let startup = super::barrier::namespace_exclusion::Startup::try_acquire()
        .map_err(|_| RegistrationFailure::Exclusion)?;
    let registrar = startup
        .registrar()
        .map_err(|_| RegistrationFailure::Exclusion)?;
    verified_first_origin(files).map_err(|_| RegistrationFailure::Origin)?;
    // A missing prepared record is not permission to generate a replacement.
    private_read(&files.private, "candidate.prepared", 0)
        .map_err(|_| RegistrationFailure::Candidate)?;
    let candidate =
        prepare_candidate(&files.private, &binding).map_err(|_| RegistrationFailure::Candidate)?;
    if candidate.receiver != receiver {
        return Err(RegistrationFailure::Candidate);
    }
    let generation = super::authority::random_hex(16).map_err(|_| RegistrationFailure::Commit)?;
    let input = FirstStoreInput {
        parent: &files.parent,
        capture: &files.root,
        private_state: &files.private,
        parent_stage: &files.parent_stage,
        capture_stage: &files.capture_stage,
        receiver: &receiver,
        generation: &generation,
        owner: StoreOwner {
            uid: setup.operator_uid,
            gid: setup.operator_gid,
        },
        _startup: &startup,
        _registrar: &registrar,
    };
    perform_first_registration(&input, &candidate).map_err(|_| RegistrationFailure::Commit)?;
    Ok(receiver)
}

fn serve_manager() -> io::Result<()> {
    let files = manager_files()?;
    let requested = receive_root(&io::stdin())?;
    let response = if same_object(&files.root, &requested)? {
        match register_first(&files) {
            Ok(receiver) => {
                serde_json::json!({"v": 1, "result": "committed", "receiver_sha256": receiver})
            }
            Err(reason) => {
                eprintln!("managed phone registration: {}", reason.code());
                serde_json::json!({"v": 1, "result": "unavailable"})
            }
        }
    } else {
        serde_json::json!({"v": 1, "result": "root_mismatch"})
    };
    let mut output = io::stdout().lock();
    serde_json::to_writer(&mut output, &response).map_err(io::Error::other)?;
    output.write_all(b"\n")?;
    output.flush()
}

fn validate_directory(binding: &DirectoryBinding) -> bool {
    let Ok(uuid) = uuid::Uuid::parse_str(&binding.filesystem_uuid) else {
        return false;
    };
    !uuid.is_nil()
        && uuid.hyphenated().to_string() == binding.filesystem_uuid
        && (1..=2_147_483_647).contains(&binding.handle_type)
        && (2..=256).contains(&binding.handle.len())
        && binding.handle.len().is_multiple_of(2)
        && binding
            .handle
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_setup(bytes: &[u8]) -> io::Result<PrivateSetup> {
    let setup: PrivateSetup = serde_json::from_slice(bytes).map_err(io::Error::other)?;
    let capture = DirectoryBinding {
        filesystem_uuid: setup.filesystem_uuid.clone(),
        handle_type: setup.handle_type,
        handle: setup.handle.clone(),
    };
    if setup.version != 3
        || setup.operator_uid == 0
        || setup.operator_uid == u32::MAX
        || setup.operator_gid == 0
        || setup.operator_gid == u32::MAX
        || !validate_directory(&capture)
        || !validate_directory(&setup.owner_parent)
        || !validate_directory(&setup.capture_stage)
        || !validate_directory(&setup.parent_stage)
        || capture == setup.owner_parent
        || setup.capture_stage == capture
        || setup.parent_stage == setup.owner_parent
        || setup.capture_stage.filesystem_uuid != capture.filesystem_uuid
        || setup.parent_stage.filesystem_uuid != setup.owner_parent.filesystem_uuid
        || ((capture.filesystem_uuid == setup.owner_parent.filesystem_uuid)
            != (setup.capture_stage == setup.parent_stage))
    {
        return Err(io::Error::other("private setup is invalid"));
    }
    Ok(setup)
}

fn private_read(directory: &File, name: &str, owner: u32) -> io::Result<Vec<u8>> {
    let file = File::from(openat(
        directory,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )?);
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != owner
        || metadata.nlink() != 1
        || metadata.mode() & 0o7777 != 0o600
        || metadata.len() > 16384
    {
        return Err(io::Error::other("private candidate record unavailable"));
    }
    let mut bytes = Vec::new();
    file.take(16385).read_to_end(&mut bytes)?;
    if bytes.len() > 16384 {
        return Err(io::Error::other("private candidate record unavailable"));
    }
    Ok(bytes)
}

fn private_create(directory: &File, name: &str, bytes: &[u8]) -> io::Result<()> {
    let mut file = File::from(openat(
        directory,
        name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )?);
    fchmod(&file, Mode::RUSR | Mode::WUSR)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    directory.sync_all()
}

fn persist_receipt(directory: &File, name: &str, value: &impl Serialize) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    if bytes.len() > 16384 {
        return Err(io::Error::other("private receipt is too large"));
    }
    private_create(directory, name, &bytes)?;
    if private_read(directory, name, directory.metadata()?.uid())? != bytes {
        return Err(io::Error::other("private receipt changed"));
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum TlsPart {
    Certificate,
    Key,
}

fn publish_candidate_file(
    directory: &File,
    part: TlsPart,
    bytes: &[u8],
    owner: StoreOwner,
) -> io::Result<()> {
    let name = match part {
        TlsPart::Certificate => "receiver-cert.pem",
        TlsPart::Key => "receiver-key.pem",
    };
    let mut file = File::from(openat(
        directory,
        name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )?);
    fchmod(&file, Mode::RUSR | Mode::WUSR)?;
    file.write_all(bytes)?;
    let metadata = file.metadata()?;
    if metadata.nlink() != 1 {
        return Err(io::Error::other("published TLS file changed"));
    }
    if metadata.uid() != owner.uid || metadata.gid() != owner.gid {
        fchown(
            &file,
            Some(Uid::from_raw(owner.uid)),
            Some(Gid::from_raw(owner.gid)),
        )?;
    }
    file.sync_all()?;
    directory.sync_all()?;
    let actual = private_read(directory, name, owner.uid)?;
    let reopened = File::from(openat(
        directory,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    let expected = file.metadata()?;
    let actual_metadata = reopened.metadata()?;
    if actual != bytes
        || actual_metadata.gid() != owner.gid
        || (expected.dev(), expected.ino()) != (actual_metadata.dev(), actual_metadata.ino())
    {
        return Err(io::Error::other("published TLS file changed"));
    }
    Ok(())
}

fn prepare_candidate(private: &File, binding: &str) -> io::Result<CandidateReceipt> {
    super::fixed::<32>(binding).map_err(|_| io::Error::other("candidate binding is invalid"))?;
    let metadata = private.metadata()?;
    if !metadata.is_dir() || metadata.mode() & 0o7777 != 0o700 {
        return Err(io::Error::other("private candidate state unavailable"));
    }
    let owner = metadata.uid();
    let intent = format!("v1:{binding}\n");
    match private_read(private, "candidate.prepared", owner) {
        Ok(bytes) => {
            let receipt: CandidateReceipt =
                serde_json::from_slice(&bytes).map_err(io::Error::other)?;
            if receipt.v != 1
                || receipt.binding != binding
                || private_read(private, "candidate.intent", owner)? != intent.as_bytes()
            {
                return Err(io::Error::other("private candidate binding changed"));
            }
            validate_candidate(private, &receipt, owner)?;
            Ok(receipt)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let path = PathBuf::from(format!("/proc/self/fd/{}/candidate", private.as_raw_fd()));
            match std::fs::symlink_metadata(&path) {
                Ok(_) => return Err(io::Error::other("private candidate already exists")),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            // An interrupted intent is ambiguous. It is not permission to generate another key.
            private_create(private, "candidate.intent", intent.as_bytes())?;
            let identity = TlsIdentity::load_or_create(&path).map_err(io::Error::other)?;
            let candidate = File::from(openat(
                private,
                "candidate",
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )?);
            let certificate = private_read(&candidate, "receiver-cert.pem", owner)?;
            let key = private_read(&candidate, "receiver-key.pem", owner)?;
            let receipt = CandidateReceipt {
                v: 1,
                binding: binding.to_owned(),
                receiver: identity.public_key_sha256().to_owned(),
                certificate_sha256: lower_hex(&Sha256::digest(certificate)),
                key_sha256: lower_hex(&Sha256::digest(key)),
            };
            let bytes = serde_json::to_vec(&receipt).map_err(io::Error::other)?;
            private_create(private, "candidate.prepared", &bytes)?;
            validate_candidate(private, &receipt, owner)?;
            Ok(receipt)
        }
        Err(error) => Err(error),
    }
}

fn prepare_from_private(private: &File) -> io::Result<serde_json::Value> {
    let setup = private_read(private, "setup.json", private.metadata()?.uid())?;
    validate_setup(&setup)?;
    let binding = lower_hex(&Sha256::digest(setup));
    let candidate = prepare_candidate(private, &binding)?;
    Ok(serde_json::json!({"v": 1, "receiver": candidate.receiver}))
}

fn validate_candidate(private: &File, receipt: &CandidateReceipt, owner: u32) -> io::Result<()> {
    let candidate = File::from(openat(
        private,
        "candidate",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    let metadata = candidate.metadata()?;
    if metadata.uid() != owner || metadata.mode() & 0o7777 != 0o700 {
        return Err(io::Error::other("private candidate directory changed"));
    }
    let certificate = private_read(&candidate, "receiver-cert.pem", owner)?;
    let key = private_read(&candidate, "receiver-key.pem", owner)?;
    let path = PathBuf::from(format!("/proc/self/fd/{}", candidate.as_raw_fd()));
    if lower_hex(&Sha256::digest(certificate)) != receipt.certificate_sha256
        || lower_hex(&Sha256::digest(key)) != receipt.key_sha256
        || TlsIdentity::existing_public_key_sha256(&path).map_err(io::Error::other)?
            != receipt.receiver
    {
        return Err(io::Error::other("private candidate changed"));
    }
    Ok(())
}

fn matches_tls_child(parent: &File, capture: &File, tls: &File) -> io::Result<()> {
    let root = File::from(openat(
        parent,
        "capture",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    if !same_object(&root, capture)? {
        return Err(io::Error::other("capture directory changed"));
    }
    let child = File::from(openat(
        capture,
        "tls",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    if !same_object(&child, tls)? {
        return Err(io::Error::other("TLS directory changed"));
    }
    Ok(())
}

// The caller must independently authenticate the manager-held descriptors and
// prove first origin before invoking this operation; this function does not.
fn perform_first_registration(
    input: &FirstStoreInput<'_>,
    candidate: &CandidateReceipt,
) -> io::Result<()> {
    if input.receiver != candidate.receiver
        || prepare_candidate(input.private_state, &candidate.binding)? != *candidate
    {
        return Err(io::Error::other("private candidate changed"));
    }
    let receipt = initialize_first_stores(
        input,
        |value| persist_receipt(input.private_state, "barrier.prepared", value),
        |value| persist_receipt(input.private_state, "stores.committed", value),
    )
    .map_err(io::Error::other)?;
    let tls = verify_first_stores(input, &receipt).map_err(io::Error::other)?;
    matches_tls_child(input.parent, input.capture, &tls)?;
    let committed = serde_json::to_vec(&receipt).map_err(io::Error::other)?;
    let owner = input.private_state.metadata()?.uid();
    if private_read(input.private_state, "stores.committed", owner)? != committed {
        return Err(io::Error::other("private committed receipt changed"));
    }
    validate_candidate(input.private_state, candidate, owner)?;
    let staged = File::from(openat(
        input.private_state,
        "candidate",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    let certificate = private_read(&staged, "receiver-cert.pem", owner)?;
    let key = private_read(&staged, "receiver-key.pem", owner)?;
    publish_candidate_file(&tls, TlsPart::Certificate, &certificate, input.owner)?;
    publish_candidate_file(&tls, TlsPart::Key, &key, input.owner)?;
    matches_tls_child(input.parent, input.capture, &tls)?;
    validate_candidate(input.private_state, candidate, owner)
}

fn receive_root(socket: &impl AsFd) -> io::Result<File> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "invalid registration request");
    let mut byte = [0];
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
    let mut ancillary = RecvAncillaryBuffer::new(&mut space);
    let message = net::recvmsg(
        socket,
        &mut [IoSliceMut::new(&mut byte)],
        &mut ancillary,
        RecvFlags::CMSG_CLOEXEC,
    )?;
    if message.bytes != 1
        || byte != *b"R"
        || message
            .flags
            .intersects(ReturnFlags::TRUNC | ReturnFlags::CTRUNC)
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
    let [descriptor]: [_; 1] = descriptors.try_into().map_err(|_| invalid())?;
    let root = File::from(descriptor);
    if !root.metadata()?.is_dir() {
        return Err(invalid());
    }
    Ok(root)
}

#[cfg(test)]
#[allow(
    clippy::panic,
    clippy::unwrap_used,
    reason = "test fixture setup and assertions"
)]
mod tests {
    use std::{
        fs::File,
        io::IoSlice,
        mem::MaybeUninit,
        os::{
            fd::AsFd,
            unix::{
                fs::{MetadataExt, PermissionsExt},
                net::UnixStream,
            },
        },
    };

    use rustix::net::{self, SendAncillaryBuffer, SendAncillaryMessage, SendFlags};

    use super::{
        TlsPart, matches_tls_child, perform_first_registration, prepare_candidate,
        prepare_from_private, private_create, publish_candidate_file, receive_root,
    };
    use crate::phone_control::{
        barrier::namespace_exclusion::Startup,
        evidence::first_init::{FirstStoreInput, StoreOwner},
    };

    fn send_root(socket: &UnixStream, byte: u8, root: &File) {
        let rights = [root.as_fd()];
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
        let mut ancillary = SendAncillaryBuffer::new(&mut space);
        assert!(ancillary.push(SendAncillaryMessage::ScmRights(&rights)));
        assert_eq!(
            net::sendmsg(
                socket,
                &[IoSlice::new(&[byte])],
                &mut ancillary,
                SendFlags::empty()
            )
            .unwrap(),
            1
        );
    }

    #[test]
    fn registration_request_carries_the_opened_root() {
        let directory = tempfile::tempdir().unwrap();
        let root = File::open(directory.path()).unwrap();
        let (client, server) = UnixStream::pair().unwrap();
        send_root(&client, b'R', &root);
        let received = receive_root(&server).unwrap();
        assert_eq!(
            (
                received.metadata().unwrap().dev(),
                received.metadata().unwrap().ino()
            ),
            (
                root.metadata().unwrap().dev(),
                root.metadata().unwrap().ino()
            )
        );
    }

    #[test]
    fn registration_request_rejects_an_unsupported_operation() {
        let directory = tempfile::tempdir().unwrap();
        let root = File::open(directory.path()).unwrap();
        let (client, server) = UnixStream::pair().unwrap();
        send_root(&client, b'Q', &root);
        assert_eq!(
            receive_root(&server).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn private_candidate_is_created_once_and_bound_to_the_setup() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let private = File::open(directory.path()).unwrap();
        let binding = "19".repeat(32);
        let first = prepare_candidate(&private, &binding).unwrap();
        let second = prepare_candidate(&private, &binding).unwrap();
        assert_eq!(first.receiver, second.receiver);
        assert_eq!(first.receiver.len(), 64);
        assert_eq!(
            crate::TlsIdentity::existing_public_key_sha256(&directory.path().join("candidate"))
                .unwrap(),
            first.receiver
        );
        assert_eq!(
            directory
                .path()
                .join("candidate/receiver-key.pem")
                .metadata()
                .unwrap()
                .mode()
                & 0o777,
            0o600
        );
        assert!(prepare_candidate(&private, &"29".repeat(32)).is_err());
    }

    #[test]
    fn interrupted_candidate_setup_does_not_generate_a_replacement_key() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let private = File::open(directory.path()).unwrap();
        let binding = "19".repeat(32);
        std::fs::write(
            directory.path().join("candidate.intent"),
            format!("v1:{binding}\n"),
        )
        .unwrap();
        let error = prepare_candidate(&private, &binding).err().unwrap();
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert!(!directory.path().join("candidate/receiver-key.pem").exists());
    }

    #[test]
    fn installer_preparation_returns_only_the_public_receiver_fingerprint() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let private = File::open(directory.path()).unwrap();
        let record = |handle: &str| {
            serde_json::json!({
                "filesystem_uuid": "11111111-1111-4111-8111-111111111111", "handle_type": 1, "handle": handle
            })
        };
        let setup = serde_json::json!({
            "version": 3, "filesystem_uuid": "11111111-1111-4111-8111-111111111111",
            "handle_type": 1, "handle": "01", "owner_parent": record("02"),
            "capture_stage": record("03"), "parent_stage": record("03"),
            "operator_uid": 1001, "operator_gid": 1001
        });
        private_create(&private, "setup.json", &serde_json::to_vec(&setup).unwrap()).unwrap();
        let result = prepare_from_private(&private).unwrap();
        let object = result.as_object().unwrap();
        assert_eq!(object.len(), 2);
        assert_eq!(object["v"], 1);
        let receiver = object["receiver"].as_str().unwrap();
        assert_eq!(receiver.len(), 64);
        assert_eq!(
            crate::TlsIdentity::existing_public_key_sha256(&directory.path().join("candidate"))
                .unwrap(),
            receiver
        );
    }

    #[test]
    fn fixed_transaction_commits_both_stores_before_publishing_the_candidate() {
        let fixture = tempfile::tempdir().unwrap();
        let parent_path = fixture.path().join("louiselm");
        let capture_path = parent_path.join("capture");
        let private_path = fixture.path().join("private");
        let stage_path = fixture.path().join("stage");
        std::fs::create_dir_all(&capture_path).unwrap();
        for path in [&private_path, &stage_path] {
            std::fs::create_dir(path).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let parent = File::open(&parent_path).unwrap();
        let capture = File::open(&capture_path).unwrap();
        let private = File::open(&private_path).unwrap();
        let stage = File::open(&stage_path).unwrap();
        let candidate = prepare_candidate(&private, &"19".repeat(32)).unwrap();
        assert!(!capture_path.join("tls/receiver-key.pem").exists());
        let startup = Startup::for_fixture(fixture.path()).unwrap();
        let registrar = startup.registrar().unwrap();
        let generation = "29".repeat(16);
        let metadata = private.metadata().unwrap();
        let owner = StoreOwner {
            uid: if metadata.uid() == 0 {
                1000
            } else {
                metadata.uid()
            },
            gid: if metadata.gid() == 0 {
                1000
            } else {
                metadata.gid()
            },
        };
        let input = FirstStoreInput {
            parent: &parent,
            capture: &capture,
            private_state: &private,
            parent_stage: &stage,
            capture_stage: &stage,
            receiver: &candidate.receiver,
            generation: &generation,
            owner,
            _startup: &startup,
            _registrar: &registrar,
        };
        perform_first_registration(&input, &candidate).unwrap();
        assert!(private_path.join("barrier.prepared").is_file());
        assert!(private_path.join("stores.committed").is_file());
        assert!(
            parent_path
                .join("workflow/phone-action-owner/actions.json")
                .is_file()
        );
        assert!(
            capture_path
                .join("phone-control/action-evidence/actions.json")
                .is_file()
        );
        assert_eq!(
            crate::TlsIdentity::existing_public_key_sha256(&capture_path.join("tls")).unwrap(),
            candidate.receiver
        );
        for name in ["receiver-cert.pem", "receiver-key.pem"] {
            let published = std::fs::metadata(capture_path.join("tls").join(name)).unwrap();
            assert_eq!((published.uid(), published.gid()), (owner.uid, owner.gid));
        }
    }

    #[test]
    fn published_tls_requires_the_registered_parent_and_capture() {
        let fixture = tempfile::tempdir().unwrap();
        let first = fixture.path().join("first");
        let second = fixture.path().join("second");
        std::fs::create_dir_all(first.join("capture/tls")).unwrap();
        std::fs::create_dir_all(second.join("capture")).unwrap();
        let parent = File::open(&first).unwrap();
        let other_parent = File::open(&second).unwrap();
        let capture = File::open(first.join("capture")).unwrap();
        let tls = File::open(first.join("capture/tls")).unwrap();
        assert!(matches_tls_child(&parent, &capture, &tls).is_ok());
        assert!(matches_tls_child(&other_parent, &capture, &tls).is_err());
    }

    #[test]
    fn tls_publication_never_replaces_an_existing_file() {
        let fixture = tempfile::tempdir().unwrap();
        let directory = File::open(fixture.path()).unwrap();
        let metadata = directory.metadata().unwrap();
        let owner = StoreOwner {
            uid: metadata.uid(),
            gid: metadata.gid(),
        };
        publish_candidate_file(&directory, TlsPart::Certificate, b"certificate", owner).unwrap();
        let path = fixture.path().join("receiver-cert.pem");
        assert_eq!(std::fs::read(&path).unwrap(), b"certificate");
        assert_eq!(path.metadata().unwrap().mode() & 0o777, 0o600);
        assert_eq!(path.metadata().unwrap().uid(), metadata.uid());
        let error = publish_candidate_file(&directory, TlsPart::Certificate, b"replacement", owner)
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(path).unwrap(), b"certificate");
    }
}
