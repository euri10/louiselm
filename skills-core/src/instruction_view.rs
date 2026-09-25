//! Immutable, Agent-scoped Instruction views derived from signed store supply.
//!
//! A view contains `view.json` and a `skills/` tree of digest-named packages.
//! Only `skills/` is intended for the native skill-root mount. The canonical
//! description binds Generation, Agent and package identities; host paths,
//! Providers and registry ordering never enter its digest. An empty view has
//! no Generation or Agent binding and is shared by `skills=off` and Agents
//! with no admitted members.
//!
//! As with Store, its owner must protect the root from untrusted writers.
//! Read-only modes prevent accidental edits; they do not constrain the owner
//! or replace a Session's read-only mount. Every materialization rechecks the
//! complete destination instead of trusting a previously returned handle.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{self, Read},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

use rustix::fs::{Mode, OFlags, RenameFlags};
use serde::Serialize;
use thiserror::Error;

use crate::{
    Digest, GenerationRecord, Package, Policy, Store, StoreError,
    admission::{self, AdmissionError},
    capture,
    generation::RECORD_SCHEMA,
    posture::DimensionName,
    quarantine::{self, QuarantineError},
    registry::Registry,
};

/// A materialized artifact; only this module constructs its identity and path.
#[derive(Clone, Debug)]
pub struct InstructionView {
    digest: Digest,
    root: PathBuf,
    skills_root: PathBuf,
    generation: Option<String>,
}

impl InstructionView {
    /// Generation verified under the materialization lock, including an Agent
    /// with no members. An explicit `skills=off` mask has no Generation.
    /// This binding does not change the shared empty tree's content address.
    #[must_use]
    pub fn generation(&self) -> Option<&str> {
        self.generation.as_deref()
    }

    /// Content address of the canonical description.
    #[must_use]
    pub fn digest(&self) -> &Digest {
        &self.digest
    }

    /// Directory holding the description and the mountable tree.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Directory to bind read-only at the Agent's native skill root.
    ///
    /// This is an artifact location, not proof of a live Session mount.
    #[must_use]
    pub fn skills_root(&self) -> &Path {
        &self.skills_root
    }
}

/// Failure to derive or publish a complete Instruction view.
#[derive(Debug, Error)]
pub enum ViewError {
    /// Generation authority could not be read or verified.
    #[error("managed_supply: {0}")]
    Admission(#[from] AdmissionError),
    /// A stored package could not be read.
    #[error("managed_supply: {0}")]
    Store(#[from] StoreError),
    /// Quarantine state is unreadable.
    #[error("managed_supply: {0}")]
    Quarantine(#[from] QuarantineError),
    /// A known supply invariant is unsatisfied.
    #[error("managed_supply: {0}")]
    Refused(&'static str),
    /// A view or package tree differs from its canonical description.
    #[error("managed_supply: {reason} at '{path}'")]
    Tree {
        /// Path at which verification failed.
        path: PathBuf,
        /// Stable refusal reason, containing no file content.
        reason: &'static str,
    },
    /// A filesystem operation failed.
    #[error("managed_supply: I/O at '{path}': {source}")]
    Io {
        /// Path being operated on.
        path: PathBuf,
        /// Underlying failure.
        source: io::Error,
    },
}

impl ViewError {
    /// Dimension this refusal belongs to.
    #[must_use]
    pub const fn dimension(&self) -> DimensionName {
        DimensionName::ManagedSupply
    }
}

#[derive(Serialize)]
struct Description<'a> {
    schema: &'static str,
    generation: Option<&'a str>,
    agent: Option<&'a str>,
    packages: Vec<String>,
}

/// Publishes one complete view for every registered Agent, in identifier order.
///
/// Blocking local I/O and signature verification. The current witnessed
/// Generation is verified under the Admission lock, and all its packages are
/// checked before any view is published. Membership names absent from the
/// registry produce no view. Each publication is atomic; a later failure may
/// leave earlier complete artifacts, but returns no partial result or fallback.
/// Call again before using a stored artifact; a handle does not grant authority
/// or remain current across later activation, quarantine, or store mutation.
///
/// # Errors
/// Refuses missing, invalid, unwitnessed or quarantined supply, a mismatched
/// policy, altered package/view bytes, unsafe filesystem entries or I/O failure.
pub fn materialize(
    store: &Store,
    policy: &Policy,
    registry: &Registry,
) -> Result<BTreeMap<String, InstructionView>, ViewError> {
    let (_locked, current) = admission::locked_current(store)?;
    let record = current.ok_or(ViewError::Refused("no_current_generation"))?;
    validate_generation(store, policy, &record)?;
    require_directory(&store.root().join("packages"), false)?;
    let mut packages = BTreeMap::new();
    for member in &record.payload.members {
        let digest = Digest::parse(&member.package_digest).map_err(StoreError::from)?;
        let package = store.open_package(&digest, policy)?;
        require_directory(&package.root, false)?;
        if package.manifest.digest() != digest {
            return Err(ViewError::Refused("package_digest_mismatch"));
        }
        read_file(
            &package.root.join("manifest.json"),
            &Entry::File {
                digest: digest.clone(),
                size: package.manifest.canonical_bytes().len() as u64,
                executable: false,
            },
            false,
        )?;
        let expected = package_tree(&package, policy)?;
        verify_tree(&package.root.join("files"), &expected, false)?;
        packages.insert(member.package_digest.as_str(), package);
    }

    let mut views = BTreeMap::new();
    for agent in registry.agent_ids() {
        let selected = record
            .payload
            .members
            .iter()
            .filter(|member| member.agents.contains(&agent))
            .map(|member| &packages[member.package_digest.as_str()])
            .collect::<Vec<_>>();
        let mut view = if selected.is_empty() {
            empty(store)?
        } else {
            publish(
                store,
                Some(&record.generation),
                Some(&agent),
                &selected,
                policy,
            )?
        };
        view.generation = Some(record.generation.clone());
        views.insert(agent, view);
    }
    Ok(views)
}

/// Publishes the canonical empty mask, even when no valid Generation exists.
///
/// Its `skills/` directory is present and empty. Failures return an error,
/// never an absent root that could let native discovery fall back elsewhere.
/// This blocking operation grants no supply or launch authority.
///
/// # Errors
/// Refuses an altered existing empty view, unsafe entries, or I/O failure.
pub fn empty(store: &Store) -> Result<InstructionView, ViewError> {
    publish(store, None, None, &[], &Policy::embedded())
}

fn validate_generation(
    store: &Store,
    policy: &Policy,
    record: &GenerationRecord,
) -> Result<(), ViewError> {
    if record.schema != RECORD_SCHEMA || record.invalid_reason.is_some() {
        return Err(ViewError::Refused("invalid_generation_record"));
    }
    if record.witness.is_none() {
        return Err(ViewError::Refused("generation_not_witnessed"));
    }
    if record.payload.policy_digest != policy.digest().to_string() {
        return Err(ViewError::Refused("policy_digest_mismatch"));
    }
    let quarantine = quarantine::load(store)?;
    if let Some(quarantine) = &quarantine {
        if quarantine.schema != quarantine::QUARANTINE_SCHEMA {
            return Err(ViewError::Refused("unknown_quarantine_schema"));
        }
        if !quarantine::partition(
            Some(quarantine),
            &record.generation,
            &record.payload.member_digests(),
        )
        .1
        .is_empty()
        {
            return Err(ViewError::Refused("generation_quarantined"));
        }
    }
    let mut previous = None;
    for member in &record.payload.members {
        if previous.is_some_and(|earlier| earlier >= member.package_digest.as_str())
            || member.agents.is_empty()
            || member.agents.iter().any(|agent| agent.trim().is_empty())
        {
            return Err(ViewError::Refused("invalid_generation_members"));
        }
        previous = Some(member.package_digest.as_str());
    }
    Ok(())
}

#[derive(Clone)]
enum Entry {
    Directory,
    File {
        digest: Digest,
        size: u64,
        executable: bool,
    },
}

type Tree = BTreeMap<PathBuf, Entry>;

fn package_tree(package: &Package, policy: &Policy) -> Result<Tree, ViewError> {
    let mut tree = BTreeMap::from([(PathBuf::new(), Entry::Directory)]);
    let mut total = 0_u64;
    if package.manifest.entries.len() > policy.limits().max_entries {
        return Err(ViewError::Refused("package_entry_limit"));
    }
    for entry in &package.manifest.entries {
        total = total
            .checked_add(entry.size)
            .ok_or(ViewError::Refused("package_size_limit"))?;
        if entry.size > policy.limits().max_file_bytes || total > policy.limits().max_total_bytes {
            return Err(ViewError::Refused("package_size_limit"));
        }
        let path = PathBuf::from(&entry.path);
        let digest = Digest::parse(&entry.sha256).map_err(StoreError::from)?;
        if tree
            .insert(
                path.clone(),
                Entry::File {
                    digest,
                    size: entry.size,
                    executable: entry.executable,
                },
            )
            .is_some()
        {
            return Err(ViewError::Refused("package_path_conflict"));
        }
        for parent in path.ancestors().skip(1) {
            match tree.entry(parent.to_path_buf()).or_insert(Entry::Directory) {
                Entry::Directory => (),
                Entry::File { .. } => return Err(ViewError::Refused("package_path_conflict")),
            }
        }
    }
    Ok(tree)
}

fn publish(
    store: &Store,
    generation: Option<&str>,
    agent: Option<&str>,
    packages: &[&Package],
    policy: &Policy,
) -> Result<InstructionView, ViewError> {
    let description = Description {
        schema: "louiselm.skills.instruction-view/1",
        generation,
        agent,
        packages: packages
            .iter()
            .map(|package| package.digest.to_string())
            .collect(),
    };
    let bytes = serde_json::to_vec(&description)
        .map_err(|_| ViewError::Refused("description_serialization"))?;
    let digest = Digest::of(&bytes);
    require_directory(store.root(), false)?;
    let views = store.root().join("views");
    fs::create_dir_all(&views).map_err(|source| io_error(&views, source))?;
    require_directory(&views, false)?;
    let root = views.join(digest.directory_name());
    let mut tree = BTreeMap::from([
        (PathBuf::new(), Entry::Directory),
        (PathBuf::from("skills"), Entry::Directory),
        (
            PathBuf::from("view.json"),
            Entry::File {
                digest: digest.clone(),
                size: bytes.len() as u64,
                executable: false,
            },
        ),
    ]);
    for package in packages {
        let prefix = Path::new("skills").join(package.digest.directory_name());
        for (path, entry) in package_tree(package, policy)? {
            tree.insert(prefix.join(path), entry);
        }
    }
    match fs::symlink_metadata(&root) {
        Ok(_) => verify_tree(&root, &tree, true)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let staging = tempfile::Builder::new()
                .prefix(".view-")
                .tempdir_in(&views)
                .map_err(|source| io_error(&views, source))?;
            let result = (|| {
                write_tree(staging.path(), &tree, &bytes, packages)?;
                verify_tree(staging.path(), &tree, true)?;
                match rustix::fs::renameat_with(
                    rustix::fs::CWD,
                    staging.path(),
                    rustix::fs::CWD,
                    &root,
                    RenameFlags::NOREPLACE,
                ) {
                    Ok(()) => (),
                    Err(rustix::io::Errno::EXIST) => verify_tree(&root, &tree, true)?,
                    Err(source) => return Err(io_error(&root, source.into())),
                }
                Ok(())
            })();
            // Only this unique staging tree is ours. Leftovers are inert and
            // never read as views, so cleanup cannot affect published authority.
            capture::discard_staging(staging.path());
            result?;
        }
        Err(source) => return Err(io_error(&root, source)),
    }
    sync_directory(&views)?;
    Ok(InstructionView {
        digest,
        skills_root: root.join("skills"),
        root,
        generation: generation.map(str::to_owned),
    })
}

fn write_tree(
    root: &Path,
    tree: &Tree,
    description: &[u8],
    packages: &[&Package],
) -> Result<(), ViewError> {
    for (relative, entry) in tree {
        if matches!(entry, Entry::Directory) {
            let path = root.join(relative);
            fs::create_dir_all(&path).map_err(|source| io_error(&path, source))?;
        }
    }
    write_file(&root.join("view.json"), description, false)?;
    for package in packages {
        for entry in &package.manifest.entries {
            let source = package.file_path(&entry.path);
            let expected = Entry::File {
                digest: Digest::parse(&entry.sha256).map_err(StoreError::from)?,
                size: entry.size,
                executable: entry.executable,
            };
            let bytes = read_file(&source, &expected, false)?;
            let destination = root
                .join("skills")
                .join(package.digest.directory_name())
                .join(&entry.path);
            write_file(&destination, &bytes, entry.executable)?;
        }
    }
    for (relative, entry) in tree.iter().rev() {
        if matches!(entry, Entry::Directory) {
            let path = root.join(relative);
            fs::set_permissions(&path, fs::Permissions::from_mode(0o555))
                .map_err(|source| io_error(&path, source))?;
            sync_directory(&path)?;
        }
    }
    Ok(())
}

fn write_file(path: &Path, bytes: &[u8], executable: bool) -> Result<(), ViewError> {
    capture::write_immutable(path, bytes, executable).map_err(StoreError::from)?;
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|source| io_error(path, source))
}

fn verify_tree(root: &Path, tree: &Tree, readonly: bool) -> Result<(), ViewError> {
    let mut found = BTreeSet::new();
    visit_tree(root, Path::new(""), tree, readonly, &mut found)?;
    if let Some(missing) = tree.keys().find(|path| !found.contains(*path)) {
        return Err(tree_error(&root.join(missing), "missing_entry"));
    }
    Ok(())
}

fn visit_tree(
    root: &Path,
    relative: &Path,
    tree: &Tree,
    readonly: bool,
    found: &mut BTreeSet<PathBuf>,
) -> Result<(), ViewError> {
    let path = root.join(relative);
    match tree.get(relative) {
        Some(Entry::File { .. }) => {
            read_file(&path, &tree[relative], readonly)?;
        }
        expected => {
            // Empty source directories are not part of a package manifest.
            // A published view, however, permits exactly its described tree.
            if expected.is_none() && readonly {
                return Err(tree_error(&path, "unexpected_entry"));
            }
            require_directory(&path, readonly)?;
            for entry in fs::read_dir(&path).map_err(|source| io_error(&path, source))? {
                let entry = entry.map_err(|source| io_error(&path, source))?;
                visit_tree(
                    root,
                    &relative.join(entry.file_name()),
                    tree,
                    readonly,
                    found,
                )?;
            }
        }
    }
    found.insert(relative.to_path_buf());
    Ok(())
}

fn require_directory(path: &Path, readonly: bool) -> Result<(), ViewError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| io_error(path, source))?;
    if !metadata.is_dir() || (readonly && metadata.mode() & 0o7777 != 0o555) {
        return Err(tree_error(path, "directory_type_or_mode"));
    }
    Ok(())
}

fn read_file(path: &Path, expected: &Entry, readonly: bool) -> Result<Vec<u8>, ViewError> {
    let Entry::File {
        digest,
        size,
        executable,
    } = expected
    else {
        return Err(tree_error(path, "expected_regular_file"));
    };
    let file = File::from(
        rustix::fs::open(
            path,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|source| io_error(path, source.into()))?,
    );
    let metadata = file.metadata().map_err(|source| io_error(path, source))?;
    let mode = if *executable { 0o555 } else { 0o444 };
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.len() != *size
        || (metadata.mode() & 0o111 != 0) != *executable
        || (readonly && metadata.mode() & 0o7777 != mode)
    {
        return Err(tree_error(path, "file_type_size_or_mode"));
    }
    let mut bytes = Vec::new();
    file.take(
        size.checked_add(1)
            .ok_or(ViewError::Refused("file_size_overflow"))?,
    )
    .read_to_end(&mut bytes)
    .map_err(|source| io_error(path, source))?;
    if bytes.len() as u64 != *size || Digest::of(&bytes) != *digest {
        return Err(tree_error(path, "file_content_mismatch"));
    }
    Ok(bytes)
}

fn sync_directory(path: &Path) -> Result<(), ViewError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error(path, source))
}

fn io_error(path: &Path, source: io::Error) -> ViewError {
    ViewError::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn tree_error(path: &Path, reason: &'static str) -> ViewError {
    ViewError::Tree {
        path: path.to_path_buf(),
        reason,
    }
}
