# Fixture blockers: red/green evidence

Issues: `louiselm-h157`, `louiselm-0s97h`. The maintainer authorized fixing
these two blockers before resuming the split and cache experiments. These are
test-only correctness changes, not another performance candidate. Campaign
budget remains maximum **3**, consumed **1** until a candidate is reapplied.

Base: `8738362` in `/var/tmp/louiselm-ci.5TZCSf`. The only executable diff is
`skills-core/tests/admission.rs` and
`skills-core/src/launch_supervisor/installed_tests.rs`; its `git diff` SHA-256
is `b7c1f4f0767f565da11cbb6c6d3a17f2547a40a6002204c78f61f58d0de82124`.
No production locking, broker behavior, permissions, deadlines or dependencies
changed. Original lock-held refusals and final exact-effect assertion remain.
Fix commits: `82d5484` (admission lock) and `41afe0d` (delegated effect).

Guest: restricted disposable Debian 13 launcher VM, 2 vCPU, 4 GiB, Rust
1.97.1; source `/home/vm/ci-8nblj.jpyc2k`, target
`/var/tmp/louiselm-skills-target`. All Cargo checks use
`CARGO_PROFILE_DEV_DEBUG=line-tables-only`, `CARGO_NET_OFFLINE=true` and the
guest Rust toolchain on PATH. No privileged tests run on the desktop.

## Admission lock

Keep `lock.try_clone()` alive past the fixture's `drop(lock)`. This retains
the same open-file description as a fork before exec, without timing or unsafe
code. Before adding the explicit unlock, the exact test fails deterministically:

```text
tests/admission.rs:648:43: called Result::unwrap() on an Err value:
Trust(Busy("/tmp/.../store/trust"))
0 passed; 1 failed; finished in 0.12s (Cargo exit 101)
```

With `flock(&lock, FlockOperation::Unlock)` before `drop(lock)`, the same
test passes in 0.07s while the clone remains open. Command, from guest repo:

```sh
bash scripts/test-skills-core --test admission \
  a_held_trust_lock_refuses_activation_readers_and_other_writers -- --exact
```

The actual child retaining a descriptor in historical CI was not traced.
This proves the fixture lifetime defect through the repository-mandated
fork-equivalent reproduction. Production `LockedTrust::drop` already unlocks
explicitly and already has its own cloned-descriptor regression. Standing
lesson remains in `docs/agent-rust.md`; this issue needs no new global rule.

## Delegated effect

The Agent fixture forwards delegation (`Granted`), while the helper receives
completion on its separate channel. The controller cannot observe completion
through the existing Agent channel. Keep the existing 10s deadline and 5ms
poll interval, but observe complete `authorized` bytes instead of file existence.
Missing files remain pending; other read errors fail immediately.

The extracted old `path.exists()` predicate fails the new deterministic test
on a newly created empty file (0 passed, 1 failed, 0.00s, Cargo exit 101).
The corrected predicate passes absent, empty, partial, complete and wrong-byte
cases. A second test proves unexpected read errors are not swallowed:

```sh
bash scripts/test-skills-core --lib helper_effect_
```

Result: 2 passed, 0 failed, 0.00s. No fixed-delay workaround, completion-channel
abstraction or production change was needed. The issue-local lesson is that
creation and delegation are not proof of the asserted effect's completion.

## Broader gates

- `cargo fmt --check`: pass.
- `cargo clippy --all-targets --all-features --locked -- -D warnings`: pass.
- `RUSTDOCFLAGS=-Dwarnings cargo doc --no-deps --all-features --locked`: pass.
- `node --test --test-timeout=5000 skills-core/tests/recovery_browser.test.cjs`:
  13 passed.
- First complete parallel `bash scripts/test-skills-core`: **failed** in
  unchanged workspace capture; 333 passed, 1 failed, 3 ignored in library tests.
  Both new helper tests passed. The remaining two planned repetitions did not
  run. No full-suite success is claimed.
- Full privileged fixture inventory: **21/21 invocations passed**, serially in
  one disposable VM (20 Rust selectors and the two-test Python systemd suite).
  This includes the installed broker/effects, provider credentials, receipt
  history, daemon, tracker-routing, four cold-resume and failure scenarios.
  `fixture-privileged.json` preserves each result and timing. It is correctness
  evidence, not a candidate speedup measurement.
- All integration targets (`bash scripts/test-skills-core --test '*'`): pass.
  Existing ignored cross-service cases remain ignored; this is not a claim to
  have rerun their separate peer-dependent gates. Two further executions of
  `bash scripts/test-skills-core --test admission` pass: the admission suite
  passes 27/27 on all three parallel runs (0.59s, 0.58s, 0.62s).

Privileged verification used the archived dispatcher from `60aa9e8`, already
proved to preserve all original arguments/flags and checked byte-for-byte in
the guest (SHA-256 recorded in the JSON). Both groups ran serially, with the
existing gate flags, timeouts, namespaces and masks. The dispatcher and split
workflow have **not** been reinstated in the executable branch.

After the guest checks, both changed source files match the committed host
files byte-for-byte. No fixture passwd/group entries for 60000, 4019000 or
4020000, loaded LouiseLM units, or matching processes remain. The VM was
stopped after validation. No hosted CI rerun or performance measurement was
started on the known-failing baseline.

An initial lint attempt stopped because the guest copy omitted three external
JSON inputs. Copying the repository's complete `tests/fixtures` and
`docs/recovery-ceremony.md` repaired the test setup; no source change was made
to work around missing includes.

New baseline blocker: `louiselm-tf62f`, exact diagnostic:

```text
workspace::tree::tests::mutation_replacement_addition_and_removal_between_scans_are_refused
src/workspace/tree.rs:218:13: accepted mutation 0
test result: FAILED. 333 passed; 1 failed; 3 ignored; 0 measured;
0 filtered out; finished in 8.84s
error: test failed, to rerun pass --lib
```

`workspace/tree.rs` is unchanged (Git blob
`06a06e5d1cdeacba1b3b7c632d3378c531c392bf`). The test overwrites a six-byte file
with different six-byte contents between scans. Capture compares metadata
stamps, not the contents of both scans; timestamp resolution is a hypothesis,
not yet an established cause. The optimization baseline is failing, so neither
candidate can be resumed. The separate defect is filed before any fix.

Handoff: the two fixes are implemented, but their issues remain open for the
filed full-suite acceptance. Claims are released with `louiselm-tf62f` as the
blocker. Fixing workspace capture requires the maintainer's separate go-ahead;
no expansion of scope, test weakening or optimization retry was inferred.
