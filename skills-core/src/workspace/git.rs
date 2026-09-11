//! Fixed local Git plumbing. Candidate filters, hooks, signing and network are off.

use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::{Read as _, Seek as _, Write as _},
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use super::{
    MAX_FILE_BYTES, MAX_FILES, MAX_RECORD_BYTES, MAX_TOTAL_BYTES, SourceFile, SourceFiles,
    WorkspaceError, validate_path,
};

pub(super) fn valid_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn oid(bytes: &[u8]) -> Result<String, WorkspaceError> {
    let value = std::str::from_utf8(bytes)
        .map_err(|_| WorkspaceError::Git)?
        .trim_end_matches('\n');
    if !valid_oid(value) {
        return Err(WorkspaceError::Git);
    }
    Ok(value.to_owned())
}

pub(super) fn baseline(repository: &Path) -> Result<(String, SourceFiles), WorkspaceError> {
    let top = run(
        repository,
        &["rev-parse", "--show-toplevel"],
        &[],
        MAX_RECORD_BYTES,
    )?;
    let top = std::str::from_utf8(&top)
        .map_err(|_| WorkspaceError::Invalid("repository path is not UTF-8"))?;
    if fs::canonicalize(top.trim_end_matches('\n'))? != repository {
        return Err(WorkspaceError::Invalid(
            "repository must name the top-level checkout",
        ));
    }
    let commit = oid(&run(
        repository,
        &["rev-parse", "--verify", "HEAD^{commit}"],
        &[],
        128,
    )?)?;
    let listing = run(
        repository,
        &["ls-tree", "-rz", "-l", "--full-tree", &commit],
        &[],
        MAX_RECORD_BYTES,
    )?;
    let mut files = SourceFiles::new();
    let mut total = 0_usize;
    for entry in listing
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let entry = std::str::from_utf8(entry)
            .map_err(|_| WorkspaceError::Invalid("non-UTF-8 source path"))?;
        let (header, path) = entry.split_once('\t').ok_or(WorkspaceError::Git)?;
        validate_path(path)?;
        let header: Vec<_> = header.split_ascii_whitespace().collect();
        let [mode, "blob", object, size] = header.as_slice() else {
            return Err(WorkspaceError::Invalid(
                "submodules and non-blob source entries are unsupported",
            ));
        };
        let executable = match *mode {
            "100644" => false,
            "100755" => true,
            _ => {
                return Err(WorkspaceError::Invalid(
                    "committed links or unsupported file modes",
                ));
            }
        };
        if !valid_oid(object) {
            return Err(WorkspaceError::Git);
        }
        let size: usize = size.parse().map_err(|_| WorkspaceError::Git)?;
        total = total
            .checked_add(size)
            .filter(|total| *total <= MAX_TOTAL_BYTES)
            .ok_or(WorkspaceError::Invalid("source exceeds total size limit"))?;
        if size > MAX_FILE_BYTES || files.len() >= MAX_FILES {
            return Err(WorkspaceError::Invalid(
                "source file or inventory exceeds size limit",
            ));
        }
        // cat-file without --filters/--textconv returns the actual object bytes.
        let bytes = run(repository, &["cat-file", "blob", object], &[], size)?;
        if bytes.len() != size {
            return Err(WorkspaceError::Git);
        }
        if files
            .insert(path.to_owned(), SourceFile { bytes, executable })
            .is_some()
        {
            return Err(WorkspaceError::Invalid("duplicate committed source path"));
        }
    }
    Ok((commit, files))
}

pub(super) fn working_paths(
    repository: &Path,
) -> Result<(BTreeSet<String>, BTreeSet<String>), WorkspaceError> {
    let listed = run(
        repository,
        &[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ],
        &[],
        MAX_RECORD_BYTES,
    )?;
    let ignored = run(
        repository,
        &[
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
            "-z",
        ],
        &[],
        MAX_RECORD_BYTES,
    )?;
    Ok((paths(&listed, false)?, paths(&ignored, true)?))
}

fn paths(bytes: &[u8], directories: bool) -> Result<BTreeSet<String>, WorkspaceError> {
    let mut paths = BTreeSet::new();
    for path in bytes
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let path = std::str::from_utf8(path)
            .map_err(|_| WorkspaceError::Invalid("non-UTF-8 working-copy path"))?;
        validate_path(if directories {
            path.trim_end_matches('/')
        } else {
            path
        })?;
        paths.insert(path.to_owned());
        if paths.len() > MAX_FILES {
            return Err(WorkspaceError::Invalid("too many working-copy paths"));
        }
    }
    Ok(paths)
}

pub(super) fn initialize(root: &Path, files: &SourceFiles) -> Result<(), WorkspaceError> {
    run(
        root,
        &["init", "--quiet", "--template=", "--initial-branch=session"],
        &[],
        MAX_RECORD_BYTES,
    )?;
    run(
        root,
        &["config", "--local", "core.hooksPath", "/dev/null"],
        &[],
        MAX_RECORD_BYTES,
    )?;
    run(
        root,
        &["config", "--local", "commit.gpgsign", "false"],
        &[],
        MAX_RECORD_BYTES,
    )?;
    let mut index = Vec::new();
    for (path, file) in files {
        let hash = oid(&run(
            root,
            &["hash-object", "-w", "--no-filters", "--stdin"],
            &file.bytes,
            128,
        )?)?;
        let mode = if file.executable { "100755" } else { "100644" };
        write!(index, "{mode} {hash}\t{path}\0")?;
    }
    run(
        root,
        &["update-index", "-z", "--index-info"],
        &index,
        MAX_RECORD_BYTES,
    )?;
    let tree = oid(&run(root, &["write-tree"], &[], 128)?)?;
    let commit = oid(&run(
        root,
        &["commit-tree", &tree, "-m", "LouiseLM source snapshot"],
        &[],
        128,
    )?)?;
    run(
        root,
        &["update-ref", "refs/heads/session", &commit],
        &[],
        MAX_RECORD_BYTES,
    )?;
    Ok(())
}

fn run(root: &Path, args: &[&str], input: &[u8], limit: usize) -> Result<Vec<u8>, WorkspaceError> {
    let mut stdin = tempfile::tempfile()?;
    stdin.write_all(input)?;
    stdin.rewind()?;
    let mut stdout = tempfile::tempfile()?;
    let stderr = tempfile::tempfile()?;
    let mut child = Command::new("/usr/bin/git")
        .args([
            "--no-pager",
            "--no-replace-objects",
            "--literal-pathspecs",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.attributesFile=/dev/null",
            "-c",
            "core.excludesFile=/dev/null",
            "-c",
            "protocol.allow=never",
            "-c",
            "gc.auto=0",
            "-c",
            "maintenance.auto=false",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.quotePath=true",
        ])
        .args(args)
        .current_dir(root)
        .env_clear()
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_AUTHOR_NAME", "LouiseLM")
        .env("GIT_COMMITTER_NAME", "LouiseLM")
        .env("GIT_AUTHOR_EMAIL", "snapshot@louiselm.invalid")
        .env("GIT_COMMITTER_EMAIL", "snapshot@louiselm.invalid")
        .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z")
        .stdin(Stdio::from(stdin))
        .stdout(Stdio::from(stdout.try_clone()?))
        .stderr(Stdio::from(stderr.try_clone()?))
        .spawn()?;
    // Fixed plumbing cannot spawn hooks, filters, helpers or network fetches.
    // Anonymous files bound captured output without pipe deadlocks or workers.
    let result = wait(&mut child, &stdout, &stderr, limit);
    if result.is_err() {
        if child.try_wait()?.is_none() {
            child.kill()?;
        }
        child.wait()?;
    }
    result?;
    stdout.rewind()?;
    let mut bytes = Vec::new();
    stdout.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(WorkspaceError::Invalid("Git output exceeds size limit"));
    }
    Ok(bytes)
}

fn wait(
    child: &mut std::process::Child,
    stdout: &File,
    stderr: &File,
    limit: usize,
) -> Result<(), WorkspaceError> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if stdout.metadata()?.len() > limit as u64 || stderr.metadata()?.len() > 64 * 1024 {
            return Err(WorkspaceError::Invalid("Git output exceeds size limit"));
        }
        if let Some(status) = child.try_wait()? {
            return if status.success() {
                Ok(())
            } else {
                Err(WorkspaceError::Git)
            };
        }
        if Instant::now() >= deadline {
            return Err(WorkspaceError::Invalid("local Git operation timed out"));
        }
        thread::sleep(Duration::from_millis(2));
    }
}
