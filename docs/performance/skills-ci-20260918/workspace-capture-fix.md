# Workspace capture baseline blocker

Issue: `louiselm-tf62f`, authorized after the two fixture fixes. Base `666343c`,
clean isolated implementation worktree. This is a correctness fix, not an
optimization candidate; campaign budget remains maximum 3, consumed 1.

## Observed cause

Restricted disposable launcher VM: Debian 13, kernel
`6.12.107+deb13-amd64`, `/tmp` is tmpfs, 2 vCPU, 4 GiB. Rust 1.97.1,
`CARGO_PROFILE_DEV_DEBUG=line-tables-only`, offline cached dependencies.

Before any production fix, a bounded diagnostic repeated the existing
between-scan overwrite and compared complete metadata stamps. Attempt 1
reproduced the problem:

```text
collision attempt=1 mtime=(1789722097, 808000000)
ctime=(1789722097, 808000000) length=6 actual=[97, 102, 116, 101, 114, 33]
metadata collision accepted changed bytes
0 passed; 1 failed; finished in 0.00s; Cargo exit 101
```

The original bytes were `before`; actual bytes became `after!`. Both stamps
also agreed on device, inode, mode and link count. Capture returned success
because it read contents only in scan 1 and compared metadata in scan 2.
Equal timestamps and lengths are not evidence that bytes stayed unchanged.

The temporary timing-dependent diagnostic was replaced, not retained as a
flaky gate. The permanent test
`workspace::tree::tests::same_size_mutation_with_equal_metadata_is_refused`
models the observed equal stamps through the existing private between-scan
probe. It first proves unchanged capture succeeds, then changes the same-size
file and makes the stored stamp equal the rewritten file's stamp. This does
not depend on the current host's clock resolution.

Red command from guest repository:

```sh
bash scripts/test-skills-core --lib \
  workspace::tree::tests::same_size_mutation_with_equal_metadata_is_refused \
  -- --exact --nocapture
```

Before the fix: `equal metadata must not hide changed bytes`, 0 passed,
1 failed, 0.00s, Cargo exit 101. After the fix, both tree tests pass, including
the original mutation/replacement/addition/removal regression. All 21
`workspace_cli` tests pass, preserving unsafe-file, bounds, publication and
verification checks.

## Narrow fix

The second walk now rereads each validated pinned regular inode and compares
its bytes with the first capture. It uses the same bounded read and before/after
metadata checks. Each second-read buffer is discarded after comparison;
there is no second retained copy of the whole workspace. Existing path,
alias, type, size/count/depth and directory/file metadata checks remain.

This fixes the shared capture boundary used by bundle export, verification
input loading and promotion destination checks. Public export documentation
still requires callers to freeze writers: two scans are not an atomic
filesystem snapshot. No dependency, unsafe operation or permission change.
The lesson is routed here and to the issue, not a broader claim that metadata
checks or cooperative writer freezing can prove arbitrary concurrent snapshots.

## Validation

Initial broader checks exhausted the disposable guest disk. `cargo clean`
refused the old target directory because it lacked `CACHEDIR.TAG`; no override
or tag fabrication was used. After inspecting its exact path and contents,
only `/var/tmp/louiselm-skills-target/debug/incremental` (4.2 GiB of generated
build data) was removed. No source or unrelated guest data was removed.
Free space became 4.1 GiB. Checks restarted without code changes.

- Focused tree tests: 2 passed.
- Workspace CLI: 21 passed.
- Browser regressions: 13 passed.
- Format, Clippy all targets/features, warnings-denied Rustdoc: passed after
  build-cache cleanup.
- Three complete parallel suites: passed. Each includes 335 passing library
  tests (3 existing ignored), all integration/binary targets and doc tests.
- All 21 privileged invocations pass after the fix; `workspace-privileged.json`
  records each result. Guest source hashes match the committed files. No fixture
  IDs, units or processes remain; VM stopped (`not-found/inactive/dead`).
- Complete hosted baselines at `a127d9c`: all 13 jobs pass in each of
  `35328351435`, `35328353675`, `35328356265`; see `repaired-baseline.json`.

Fix commit: `4c31b8173b04ed35f1cad73f6402f10906682551`. Repaired skills-core tree:
`cb21e42b65614d728f0fb046bba6c2ef04388aeb`. These are correctness/baseline results,
not an accepted optimization; the resumed campaign is tracked in `implementation.md`.
