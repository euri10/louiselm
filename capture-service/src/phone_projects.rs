//! Owner-selected project identities and retained approved-root relationships.
//!
//! These records establish only local identity relationships. They are not a
//! phone grant, proof of a source/checkout chain, or execution authority.

use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    RunProjectBinding, hex::lower_hex, permissions::set_private_permissions,
    prepared_runs::validate_binding,
};

const SCHEMA: &str = "louiselm.phone-projects/1";
const MAX_STORE: u64 = 1_048_576;
const MAX_RECORDS: usize = 1024;

/// A project identity or its private storage is invalid or unavailable.
#[derive(Debug, Error)]
pub enum PhoneProjectError {
    /// The input or retained relationship failed validation.
    #[error("phone project refused: {0}")]
    Invalid(&'static str),
    /// The selected directory or private state is unavailable.
    #[error("phone project storage unavailable")]
    Io(#[from] io::Error),
    /// A stored record could not be decoded or encoded.
    #[error("phone project storage is invalid")]
    Json(#[from] serde_json::Error),
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Directory {
    path: String,
    device: u64,
    inode: u64,
    created_seconds: u64,
    created_nanos: u32,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Root {
    id: String,
    directory: Directory,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Project {
    id: String,
    directory: Directory,
    roots: Vec<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct State {
    schema: String,
    roots: Vec<Root>,
    projects: Vec<Project>,
    sha256: String,
}

#[derive(Serialize)]
struct StoredState<'a> {
    schema: &'a str,
    roots: &'a [Root],
    projects: &'a [Project],
    sha256: String,
}

/// Private, durable identities selected by a trusted desktop operator.
///
/// Operations block on filesystem I/O. The caller must authenticate the local
/// operator, verify source and checkout evidence independently, and use current
/// grant and pairing state before delivering any content. Paths are never
/// automatically rebound to an existing identity.
#[derive(Clone, Debug)]
pub struct PhoneProjectStore {
    root: PathBuf,
}

impl PhoneProjectStore {
    /// Create a private owner store at a new path, or open an intact store.
    /// The parent must already exist and be owner-controlled.
    ///
    /// # Errors
    /// Refuses missing/damaged existing state, non-directory roots and I/O errors.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, PhoneProjectError> {
        let root = root.as_ref();
        let created = match fs::create_dir(root) {
            Ok(()) => true,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => false,
            Err(error) => return Err(error.into()),
        };
        if !fs::symlink_metadata(root)?.file_type().is_dir() {
            return Err(PhoneProjectError::Invalid("store root is not a directory"));
        }
        set_private_permissions(root, true)?;
        let store = Self { root: root.into() };
        store.locked(|| {
            if created {
                store.write(&State {
                    schema: SCHEMA.into(),
                    roots: Vec::new(),
                    projects: Vec::new(),
                    sha256: String::new(),
                })?;
            }
            store.load()?;
            Ok(())
        })?;
        Ok(store)
    }

    /// Record an explicitly selected approved root as a fresh private identity.
    /// Calling this again always creates a different identity, even at the same path.
    ///
    /// # Errors
    /// Refuses invalid/unavailable directories, exhausted storage and I/O errors.
    pub fn register_root(&self, path: &Path) -> Result<String, PhoneProjectError> {
        let selected = directory(path)?;
        self.locked(|| {
            let mut state = self.load()?;
            if state.roots.len() >= MAX_RECORDS {
                return Err(PhoneProjectError::Invalid("root limit reached"));
            }
            if directory(Path::new(&selected.path))? != selected {
                return Err(PhoneProjectError::Invalid("approved root identity changed"));
            }
            let id = fresh_id(&state)?;
            state.roots.push(Root {
                id: id.clone(),
                directory: selected,
            });
            state.roots.sort_by(|left, right| left.id.cmp(&right.id));
            self.write(&state)?;
            Ok(id)
        })
    }

    /// Record a fresh project identity and verified current parent-root relations.
    /// The operator must explicitly select the project and exact existing roots.
    ///
    /// # Errors
    /// Refuses missing, changed or foreign roots, invalid input and storage errors.
    pub fn register_project(
        &self,
        path: &Path,
        roots: &[String],
    ) -> Result<String, PhoneProjectError> {
        let selected = directory(path)?;
        if roots.is_empty()
            || roots.len() > 50
            || roots.iter().collect::<BTreeSet<_>>().len() != roots.len()
        {
            return Err(PhoneProjectError::Invalid("invalid approved roots"));
        }
        self.locked(|| {
            let mut state = self.load()?;
            if state.projects.len() >= MAX_RECORDS {
                return Err(PhoneProjectError::Invalid("project limit reached"));
            }
            for id in roots {
                let root = state
                    .roots
                    .iter()
                    .find(|root| &root.id == id)
                    .ok_or(PhoneProjectError::Invalid("approved root is unavailable"))?;
                if directory(Path::new(&root.directory.path))? != root.directory {
                    return Err(PhoneProjectError::Invalid("approved root identity changed"));
                }
                if !Path::new(&selected.path).starts_with(&root.directory.path) {
                    return Err(PhoneProjectError::Invalid(
                        "project is outside approved root",
                    ));
                }
            }
            if directory(Path::new(&selected.path))? != selected {
                return Err(PhoneProjectError::Invalid("project identity changed"));
            }
            let id = fresh_id(&state)?;
            let mut roots = roots.to_vec();
            roots.sort();
            state.projects.push(Project {
                id: id.clone(),
                directory: selected,
                roots,
            });
            state.projects.sort_by(|left, right| left.id.cmp(&right.id));
            self.write(&state)?;
            Ok(id)
        })
    }

    /// Check that an owner-selected project still names the originally recorded directory.
    /// A missing or changed directory does not erase historical relationships.
    ///
    /// # Errors
    /// Refuses unknown or changed identities and filesystem or stored-state errors.
    pub fn check_current_project(&self, id: &str) -> Result<(), PhoneProjectError> {
        self.locked(|| {
            let state = self.load()?;
            let project = state
                .projects
                .iter()
                .find(|project| project.id == id)
                .ok_or(PhoneProjectError::Invalid("project is unavailable"))?;
            if directory(Path::new(&project.directory.path))? != project.directory {
                return Err(PhoneProjectError::Invalid("project identity changed"));
            }
            Ok(())
        })
    }

    /// Compare a binding to retained root relationships and the complete Agent set.
    ///
    /// This is only a scope comparison. The caller must use the current grant,
    /// verify the original source/checkout chain and serialize protected delivery.
    /// Original directories need not still exist for a historical comparison.
    ///
    /// # Errors
    /// Refuses malformed bindings, missing project records and storage errors.
    pub fn matches_scope(
        &self,
        binding: &RunProjectBinding,
        granted_roots: &[String],
        granted_agents: &[String],
    ) -> Result<bool, PhoneProjectError> {
        validate_binding(binding).map_err(|_| PhoneProjectError::Invalid("invalid Run binding"))?;
        self.locked(|| {
            let state = self.load()?;
            let project = state
                .projects
                .iter()
                .find(|project| project.id == binding.project_id)
                .ok_or(PhoneProjectError::Invalid("project is unavailable"))?;
            Ok(project.roots.iter().any(|id| granted_roots.contains(id))
                && binding
                    .agents
                    .iter()
                    .all(|agent| granted_agents.contains(agent)))
        })
    }

    fn locked<T>(
        &self,
        action: impl FnOnce() -> Result<T, PhoneProjectError>,
    ) -> Result<T, PhoneProjectError> {
        let path = self.root.join(".lock");
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)?;
        set_private_permissions(&path, false)?;
        lock.lock_exclusive()?;
        let result = action();
        FileExt::unlock(&lock)?;
        result
    }

    fn load(&self) -> Result<State, PhoneProjectError> {
        let mut bytes = Vec::new();
        File::open(self.root.join("projects.json"))?
            .take(MAX_STORE + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_STORE {
            return Err(PhoneProjectError::Invalid("store size limit exceeded"));
        }
        let state: State = serde_json::from_slice(&bytes)?;
        validate_state(&state)?;
        if state.sha256 != state_hash(&state)? {
            return Err(PhoneProjectError::Invalid("store integrity check failed"));
        }
        Ok(state)
    }

    fn write(&self, state: &State) -> Result<(), PhoneProjectError> {
        validate_state(state)?;
        let bytes = serde_json::to_vec(&StoredState {
            schema: &state.schema,
            roots: &state.roots,
            projects: &state.projects,
            sha256: state_hash(state)?,
        })?;
        if bytes.len() as u64 > MAX_STORE {
            return Err(PhoneProjectError::Invalid("store size limit exceeded"));
        }
        let temporary = self.root.join(format!(".project-{}", Uuid::new_v4()));
        let result = (|| {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)?;
            set_private_permissions(&temporary, false)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&temporary, self.root.join("projects.json"))?;
            File::open(&self.root)?.sync_all()?;
            Ok(())
        })();
        if temporary.exists() {
            // Unpublished temporary files are never read as identity evidence.
            let _ = fs::remove_file(temporary);
        }
        result
    }
}

fn state_hash(state: &State) -> Result<String, serde_json::Error> {
    let bytes = serde_json::to_vec(&(&state.schema, &state.roots, &state.projects))?;
    Ok(lower_hex(&Sha256::digest(bytes)))
}

fn directory(path: &Path) -> Result<Directory, PhoneProjectError> {
    if !path.is_absolute() {
        return Err(PhoneProjectError::Invalid(
            "directory path must be absolute",
        ));
    }
    let path = fs::canonicalize(path)?;
    let opened = File::open(&path)?;
    let metadata = opened.metadata()?;
    let current = fs::metadata(&path)?;
    let created = metadata
        .created()?
        .duration_since(UNIX_EPOCH)
        .map_err(|_| PhoneProjectError::Invalid("directory creation time is invalid"))?;
    if !metadata.is_dir() || metadata.dev() != current.dev() || metadata.ino() != current.ino() {
        return Err(PhoneProjectError::Invalid("directory identity changed"));
    }
    let path = path
        .to_str()
        .filter(|path| !path.contains('\0') && path.len() <= 8192)
        .ok_or(PhoneProjectError::Invalid("directory path is invalid"))?;
    Ok(Directory {
        path: path.to_owned(),
        device: metadata.dev(),
        inode: metadata.ino(),
        created_seconds: created.as_secs(),
        created_nanos: created.subsec_nanos(),
    })
}

fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        && id.bytes().any(|byte| byte != b'0')
}

fn fresh_id(state: &State) -> Result<String, PhoneProjectError> {
    let id = Uuid::new_v4().simple().to_string();
    if state.roots.iter().any(|root| root.id == id)
        || state.projects.iter().any(|project| project.id == id)
    {
        return Err(PhoneProjectError::Invalid("identity collision"));
    }
    Ok(id)
}

fn validate_state(state: &State) -> Result<(), PhoneProjectError> {
    if state.schema != SCHEMA
        || state.roots.len() > MAX_RECORDS
        || state.projects.len() > MAX_RECORDS
    {
        return Err(PhoneProjectError::Invalid("invalid store header"));
    }
    let mut identities = BTreeSet::new();
    let mut prior = "";
    for root in &state.roots {
        if !valid_id(&root.id)
            || root.id.as_str() <= prior
            || !identities.insert(&root.id)
            || !valid_directory(&root.directory)
        {
            return Err(PhoneProjectError::Invalid("invalid root record"));
        }
        prior = &root.id;
    }
    let mut prior = "";
    for project in &state.projects {
        if !valid_id(&project.id)
            || project.id.as_str() <= prior
            || !identities.insert(&project.id)
            || !valid_directory(&project.directory)
            || project.roots.is_empty()
            || project.roots.len() > 50
            || !project.roots.windows(2).all(|pair| pair[0] < pair[1])
            || project.roots.iter().any(|id| {
                !state.roots.iter().any(|root| {
                    &root.id == id
                        && Path::new(&project.directory.path).starts_with(&root.directory.path)
                })
            })
        {
            return Err(PhoneProjectError::Invalid("invalid project record"));
        }
        prior = &project.id;
    }
    Ok(())
}

fn valid_directory(directory: &Directory) -> bool {
    Path::new(&directory.path).is_absolute()
        && !directory.path.contains('\0')
        && directory.path.len() <= 8192
        && directory.created_nanos < 1_000_000_000
}
