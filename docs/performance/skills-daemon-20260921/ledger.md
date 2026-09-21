# Privileged daemon runtime optimization — louiselm-klm35

User-authorized campaign, 2026-09-21. Maximum candidate passes: **3**.
Consumed candidate passes: **1**. Local repairs per candidate: at most **2**.
Candidate 1 **accepted**. Stop after one successful, narrow change; no further
candidate is needed. No repairs to the candidate itself.

## Scope and input

- Original revision: `cea617d71c2b6983818866707dde793c6c0dbb60`.
- Initial worktree: clean; no relevant dirty diff. Subsequent Beads and this
  campaign's evidence are owned by this session.
- Target: the existing serial privileged skills-core integration workload,
  starting with the activated-daemon state upgrade/adoption scenario. This is
  a shorter CPU-dominated representative than the lifecycle scenario with real
  retry waits. Preserve all assertions, trust validation, process/lifecycle
  boundaries, timeouts and fixture isolation.
- This is not a restart of the stopped `oi9d6`/`ljf7w` CI-splitting campaign.
  No sharding, dependency installation, remote benchmark, commit, or push.
- Prior evidence: `../skills-ci-20260918/ledger.md` and its existing
  `digest-benchmark.rs`. The September 21 observation in
  `/home/lotso/.cache/louiselm-ci-timing.3w9MA2/report.md` found 744.955s for the
  whole enabled step, but a single cross-machine comparison is not an A/B.

## Protocol fixed before candidates

- Same isolated Debian 13 VM, 2 vCPUs / 4 GiB guest RAM, 200% CPU cap,
  no swap, nice 10. No physical token, shared host mounts or guest egress.
- Rust 1.97.1; all features; locked/offline dependencies; existing
  `CARGO_PROFILE_DEV_DEBUG=line-tables-only`. Build time recorded separately.
- Primary fixture:
  `launch_supervisor::system::installed_tests::daemon::state::privileged_activated_daemon_upgrades_and_adopts_state`.
  Use the CI-required flag, initial root identity, private mount namespace,
  umask 022 and existing 240-second deadline. One discarded warmup followed by
  **3 serial samples** per baseline/candidate. Record wall/user/system seconds,
  median, minimum, maximum and spread. Preserve raw sample logs.
- Attribution: reuse the existing digest diagnostic (known SHA-256 vector,
  one warmup and five hashes per executable), inspect callers, and use available
  profiling tools without adding dependencies. Distinguish target timings from
  function-level attribution; do not infer all fixture time is hashing.
- Before the first candidate, calculate and record the numeric noise bound.
  Acceptance requires primary median reduction greater than **10%**, **2s**,
  and **twice the larger baseline/candidate sample range**. Reject unresolved
  noise. Secondary check: no regression in the complete enabled privileged
  block, with certification compared only on the same OS path.
- Correctness before timing acceptance: focused affected checks, full ordinary
  skills-core suite, fmt, strict Clippy, Rustdoc, browser tests, and the existing
  privileged gates affected by the change. The unchanged baseline's full
  enabled privileged block passed in the preceding measurement. New baseline
  ordinary/static gates must pass before an optimization candidate.
- Stop on unresolved correctness failure, two consecutive no-gain candidates,
  the three-pass cap, interruption, or lack of an evidence-backed hypothesis.
  No removal of checks, broader authority, or performance claim from partial
  workloads. Roll back only the candidate's own changes.

## Candidate ledger

| Pass | Hypothesis / lever | Base / diff | Gates | Measurements | Decision |
| --- | --- | --- | --- | --- | --- |
| — | Baseline and read-only attribution | Original revision above | Ordinary/static/browser and additional privileged gates pass | 45.706, 48.464, 45.871s; median 45.871s; range 2.758s | Characterization green |
| 1 | Optimize existing SHA-256 dependency only | Baseline + `candidate-1.patch` | All required ordinary/static/browser and privileged gates pass | Median 2.818s; range 0.110s; 43.053s / 93.86% lower | Keep: exceeds 5.516s noise/usefulness floor; full step also improves |

## Baseline evidence and first hypothesis

`baseline-build.log`, `baseline-samples.log`, `baseline-suite.log` and
`baseline-static.log` retain build/sample output and concise gate results.
`measure.sh` is the exact guest sampling harness, validated on the unchanged
baseline. The warmup passed in 46.918s. Baseline median user/system times are
46.302/0.937s (reaped child CPU can overlap). The initial numeric acceptance
floor is **5.516s**: max(4.5871s useful 10% gain, 2s, twice 2.758s range).
If the candidate range is larger, twice that range replaces this noise floor.

Five-sample median `Digest::of` times on actual executables are 0.672452s
(launch), 0.796700s (control), 0.498358s (Agent), and 0.496432s (helper):
**2.463942s** for one pass over all four. Input sizes and every sample are in
the raw log. The guest has strace but neither perf nor gdb; no dependency was
installed. This is direct hashing attribution, not a sampled fixture CPU
profile or a claim about invocation counts. `canonical.rs::Digest::of`,
`launcher_install.rs::hash_file`, `release.rs::hash`, and installed fixture
construction repeatedly use the existing SHA-256 implementation over binaries.

Read-only simplification audit: those hashes establish authority at separate
trust boundaries; neither dropping validation nor caching by pathname is
justified. First hypothesis, recorded before applying it: compile only the existing `sha2`
dependency with `opt-level = 3` in dev/test builds. No source algorithm, hash
input, assertion, timeout, isolation or release-profile change. Cargo's
[profile overrides](https://doc.rust-lang.org/cargo/reference/profiles.html#overrides)
provide the existing facility; generic instantiation can limit its benefit,
so the effect must be measured. Rollback would remove only that manifest table.

## Current status

Completed with **1 of 3** candidate passes consumed. Retain only the five-line
manifest addition, its testing-policy explanation, and campaign evidence.
No production behavior is intentionally changed, so existing passing
characterization and security refusal coverage replace an artificial failing
behavior test. No commits, pushes, dependency installs, or CI changes.

## Accepted results

| Metric | Original | Candidate 1 |
| --- | ---: | ---: |
| State fixture wall samples, seconds | 45.706, 48.464, 45.871 | 2.818, 2.764, 2.874 |
| Median / range, seconds | 45.871 / 2.758 | 2.818 / 0.110 |
| Median user / system CPU, seconds | 46.302 / 0.937 | 1.868 / 0.682 |
| Full privileged block, seconds | 744.955 | 92.725 |

Primary improvement: **43.053s / 93.86%**, or **16.28×** faster, above the
predeclared 5.516s acceptance floor. All four candidate runs passed, including
the discarded 2.851s warmup. `candidate-1-samples.log` retains every sample.

Secondary: all enabled privileged scenarios passed in matched serial batches
of **31.367 + 21.162 + 40.196 = 92.725s**, an **87.55% / 8.03×** improvement
over the previous local same-code baseline. Includes Debian's full certification
path on both sides. The whole-block comparison is one run per state, not a
repeated statistical estimate; the primary repeated A/B is the acceptance
metric. Batch 1's Zsh timer emits both the 0.001s AWK segment and 31.367s VM
segment; use the latter, not their overlapping sum. Batches 2–3 use Bash's
pipeline timer. Logs are `candidate-1-step-batch{1,2,3}.log`.

To isolate hashing from compiler-induced input-size changes, the baseline's
four executables were copied before rebuilding. The candidate diagnostic on
those **same bytes** has five-sample medians of 0.024271, 0.026160, 0.016865,
and 0.016962s: **0.084258s total versus 2.463942s**, or **29.24×** faster.
See `candidate-1-same-input-digests.log`. The standard SHA-256 vector and
within-input digest consistency assertions pass. The actual rebuilt executables
also pass the same diagnostic and all installed hash-mutation refusal gates.
Their combined size increases by 432,080 bytes (0.23%); this is not a
binary-size optimization. No clean-build or peak-memory improvement is claimed.

No assertions, hashes, timeouts, retries, isolation boundaries, signing behavior,
or release settings were removed or weakened. Hosted GitHub timing is not yet
measured; the result above is a local controlled daemon comparison plus a local
whole-step observation, not a promised hosted duration.

## Reproduction and gate accounting

Host gates (Rust 1.97.1; Node 24.16.0): from `skills-core`, `cargo fmt --check`,
`cargo clippy --all-targets --all-features --locked -- -D warnings`, and
`RUSTDOCFLAGS='-D warnings' cargo doc --no-deps --all-features --locked`, with
`CARGO_PROFILE_DEV_DEBUG=line-tables-only`. From the repository root:
`node --test --test-timeout=5000 skills-core/tests/recovery_browser.test.cjs`.
The baseline ordinary suite used `scripts/test-skills-core` in a transient
user service. The candidate additionally exercises CI's synthetic Git config
and descriptor injection:

```sh
systemd-run --user --pipe --wait --collect --working-directory="$PWD" \
  --setenv="PATH=$PATH" --setenv=CARGO_PROFILE_DEV_DEBUG=line-tables-only \
  bash -c 'exec bash ./scripts/test-skills-core-git-isolation 142</dev/null'
```

An initial candidate invocation mistakenly placed the multi-digit descriptor
redirection in the outer Zsh command. It ran zero selected tests and is **not**
a passing suite gate. The corrected Bash-inside-service invocation above ran
the full suite successfully (including 336 library tests and 3 ignored cases).
This invocation correction changed no candidate code or benchmark harness.

Guest source is `git archive cea617d` of `skills-core`, `scripts`, `tests/fixtures`
and the previous digest diagnostic, unpacked in `/home/vm/daemon-klm35`.
Candidate copies only the changed manifest into that checkout. All guest Cargo
commands run as `vm`, with `PATH=/home/vm/.cargo/bin:/usr/bin:/bin`,
`CARGO_TARGET_DIR=/var/tmp/louiselm-skills-target`,
`CARGO_PROFILE_DEV_DEBUG=line-tables-only`, `CARGO_NET_OFFLINE=true`, and
`CARGO_BUILD_JOBS=2`. Run `cargo build --lib --bins --all-features --locked`
and `cargo test --lib --all-features --locked --no-run`; then compile the
unchanged previous `digest-benchmark.rs` with `rustc --edition=2024
-C debuginfo=line-tables-only -L dependency=$CARGO_TARGET_DIR/debug/deps
--extern louiselm_skills=$CARGO_TARGET_DIR/debug/liblouiselm_skills.rlib`.
Output is `../digest-benchmark`. Stream `measure.sh` into Bash from the guest's
crate directory. No other heavy work runs during performance sampling.

Other privileged gates stream the unchanged CI shell bodies at baseline
`.github/workflows/ci.yml` lines 453–459, 471–511 and 576–632, plus
`../scripts/launcher-conformance --disposable-guest`, into `bash -e -s` in the
guest. Both baseline and candidate pass Linked Admission, hostile conformance,
root cgroup escape prevention, relay cleanup, supervisor composition,
post-configuration Bubblewrap mutation refusal, installed release and registry.
The measured-Agent block is exactly lines 515–572 with indentation removed,
also streamed into `bash -e -s`. No privileged tests run on the desktop.
Baseline block gate/timing evidence from the preceding same-code local run is
retained in `baseline-step.log`; its three batches total 744.955s.

Build logs are recorded separately, but the candidate preparation overlapped
host correctness builds and baseline dependency artifacts were already cached;
these are **not** a controlled clean-build time or memory comparison.

Candidate patch SHA-256 (zero-context diff, avoiding trailing-space context lines):
`c205971788f90899e7fcf7523156ea5fdc1908f5c351b0123efc53092c13bae6`.
Final `git diff --check` and diagnostic `bash -n` passed. The temporary VM was
stopped and its generated disks, copied key and boot metadata removed only
after confirming the unit was inactive/not found. Logs remain; the original
retained QA VM disk is unchanged and stopped. Temporary images can be recreated
from that original, but this campaign's discarded guest state is not retained.
