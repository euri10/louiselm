//! Fixed local Git plumbing. Candidate filters, hooks, signing and network are off.

use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::{Read as _, Seek as _, Write as _},
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use super::{
    MAX_FILE_BYTES, MAX_FILES, MAX_RECORD_BYTES, MAX_TOTAL_BYTES, SourceFile, SourceFiles,
    WorkspaceError, validate_path,
};
use crate::Digest;

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

fn run_branch(root: &Path, run_id: &str) -> Result<String, WorkspaceError> {
    if run_id.is_empty()
        || run_id.len() > 128
        || !run_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(WorkspaceError::Invalid("invalid Run branch identity"));
    }
    let top = run(root, &["rev-parse", "--show-toplevel"], &[], 4096)?;
    if fs::canonicalize(
        std::str::from_utf8(&top)
            .map_err(|_| WorkspaceError::Git)?
            .trim(),
    )? != fs::canonicalize(root)?
    {
        return Err(WorkspaceError::Invalid(
            "Run destination must be the worktree root",
        ));
    }
    let branch = run(root, &["symbolic-ref", "HEAD"], &[], 4096)?;
    if branch != format!("refs/heads/run/{run_id}\n").as_bytes() {
        return Err(WorkspaceError::Invalid(
            "Run destination is on the wrong branch",
        ));
    }
    oid(&run(
        root,
        &["rev-parse", "--verify", "HEAD^{commit}"],
        &[],
        128,
    )?)
}

pub(super) fn clean_run_head(root: &Path, run_id: &str) -> Result<String, WorkspaceError> {
    let head = run_branch(root, run_id)?;
    clean_index(root, &head)?;
    let (_, baseline) = baseline(root)?;
    if super::entries(&super::tree::capture(&File::open(root)?)?) != super::entries(&baseline) {
        return Err(WorkspaceError::Invalid(
            "Run worktree contains untracked or changed bytes",
        ));
    }
    Ok(head)
}

fn clean_index(root: &Path, head: &str) -> Result<(), WorkspaceError> {
    let staged = oid(&run(root, &["write-tree"], &[], 128)?)?;
    let committed = oid(&run(
        root,
        &["rev-parse", &format!("{head}^{{tree}}")],
        &[],
        128,
    )?)?;
    if staged != committed {
        return Err(WorkspaceError::Invalid("Run index is dirty"));
    }
    Ok(())
}

pub(super) fn commit_run(
    root: &Path,
    run_id: &str,
    bead_id: &str,
    expected_head: &str,
    result_digest: &str,
) -> Result<String, WorkspaceError> {
    if bead_id.is_empty()
        || bead_id.len() > 128
        || !bead_id.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' || byte == b'.'
        })
        || !valid_oid(expected_head)
    {
        return Err(WorkspaceError::Invalid("invalid Run commit identity"));
    }
    if run_branch(root, run_id)? != expected_head {
        return Err(WorkspaceError::Invalid("Run branch advanced before commit"));
    }
    clean_index(root, expected_head)?;
    let files = super::tree::capture(&File::open(root)?)?;
    let inventory = super::entries(&files);
    if Digest::of(&serde_json::to_vec(&inventory)?).to_string() != result_digest {
        return Err(WorkspaceError::Invalid(
            "promoted bytes changed before commit",
        ));
    }
    run(root, &["read-tree", "--empty"], &[], 128)?;
    let mut index = Vec::new();
    for (path, file) in &files {
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
    let message = format!("Accept {bead_id}\n\nRefs {bead_id}");
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| WorkspaceError::Invalid("clock unavailable for Run commit"))?
        .as_secs();
    let date = format!("@{seconds} +0000");
    let commit = oid(&run_with_date(
        root,
        &["commit-tree", &tree, "-p", expected_head, "-m", &message],
        &[],
        128,
        &date,
    )?)?;
    run(
        root,
        &[
            "update-ref",
            &format!("refs/heads/run/{run_id}"),
            &commit,
            expected_head,
        ],
        &[],
        128,
    )?;
    if clean_run_head(root, run_id)? != commit {
        return Err(WorkspaceError::Invalid("Run commit left a dirty worktree"));
    }
    Ok(commit)
}

fn run(root: &Path, args: &[&str], input: &[u8], limit: usize) -> Result<Vec<u8>, WorkspaceError> {
    run_with_date(root, args, input, limit, "2000-01-01T00:00:00Z")
}

fn run_with_date(
    root: &Path,
    args: &[&str],
    input: &[u8],
    limit: usize,
    date: &str,
) -> Result<Vec<u8>, WorkspaceError> {
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
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
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

#[cfg(test)]
mod run_tests {
    #![allow(clippy::unwrap_used, reason = "Fixture failures abort tests.")]

    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn accepted_bead_commits_exact_bytes_and_next_snapshot_uses_new_head() {
        let root = tempfile::tempdir().unwrap();
        let checkout = root.path().join("checkout");
        fs::create_dir(&checkout).unwrap();
        fs::set_permissions(&checkout, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(checkout.join("file"), b"before").unwrap();
        let files = super::super::tree::capture(&File::open(&checkout).unwrap()).unwrap();
        initialize(&checkout, &files).unwrap();
        run(&checkout, &["branch", "-m", "run/test-run"], &[], 128).unwrap();
        let old = clean_run_head(&checkout, "test-run").unwrap();
        let old_snapshot =
            crate::workspace::prepare(&checkout, &[], &root.path().join("old-snapshot")).unwrap();

        fs::write(checkout.join("untracked"), b"local").unwrap();
        assert!(clean_run_head(&checkout, "test-run").is_err());
        fs::remove_file(checkout.join("untracked")).unwrap();
        assert_eq!(clean_run_head(&checkout, "test-run").unwrap(), old);

        fs::write(checkout.join("file"), b"staged").unwrap();
        run(&checkout, &["add", "file"], &[], 128).unwrap();
        fs::write(checkout.join("file"), b"before").unwrap();
        assert!(
            clean_run_head(&checkout, "test-run").is_err(),
            "staged-only changes refuse"
        );
        run(&checkout, &["read-tree", "HEAD"], &[], 128).unwrap();

        fs::write(checkout.join("file"), b"after").unwrap();
        assert!(clean_run_head(&checkout, "test-run").is_err());
        let changed = super::super::tree::capture(&File::open(&checkout).unwrap()).unwrap();
        let result_digest =
            Digest::of(&serde_json::to_vec(&super::super::entries(&changed)).unwrap()).to_string();
        assert!(
            commit_run(
                &checkout,
                "test-run",
                "bead-1",
                &old,
                &Digest::of(b"wrong").to_string()
            )
            .is_err()
        );
        let commit = commit_run(&checkout, "test-run", "bead-1", &old, &result_digest).unwrap();
        assert_eq!(
            crate::workspace::snapshot_base_commit(
                &root.path().join("old-snapshot"),
                &Digest::parse(&old_snapshot.snapshot_digest).unwrap(),
            )
            .unwrap(),
            old,
            "a pre-promotion snapshot must not pass the next HEAD check",
        );
        assert_eq!(clean_run_head(&checkout, "test-run").unwrap(), commit);
        assert_eq!(
            run(&checkout, &["log", "-1", "--format=%B"], &[], 1024).unwrap(),
            b"Accept bead-1\n\nRefs bead-1\n\n"
        );
        let snapshot =
            crate::workspace::prepare(&checkout, &[], &root.path().join("next-snapshot")).unwrap();
        assert_eq!(snapshot.base_commit, commit);
    }
}
