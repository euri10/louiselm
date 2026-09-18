# skills-core CI investigation — louiselm-8nblj

Investigation only, 2026-09-18. **No CI or runtime optimization applied.**
Recommended execution task: **louiselm-ljf7w**, one additional isolated runner
for the daemon, cold-resume and launch-failure group. The timing model predicts
about **10.5–15.2 minutes**, compared with **18.4–27.0 minutes** today, at about
two additional runner minutes. Those are projections requiring hosted acceptance,
not a measured speedup. Cache follow-up: **louiselm-edon3** (P3).

## Baseline and protocol

- Revision: `c55c092657a4dffe6d1441eb452d31ca299c11f4`. Initial dirty path:
  `.beads/issues.jsonl` only; no relevant source diff. The four September 18
  revisions below and local HEAD share skills-core tree
  `419bb0bb9d40639b133ca1cbe0e870602cc13c01`.
- Target: wall time to complete the skills-core CI job, with summed runner
  minutes as the cost metric. Workload: the enabled privileged commands in
  `Test measured Agent and isolated tool grants`, plus the surrounding job's
  setup/build/quality gates when modeling total time.
- Bounds: **maximum 0 optimization candidates; consumed 0**. No accepted or
  rejected production candidate, and no original-to-final speedup claim.
- Hosted samples: four successful current-source runs, plus the older run cited
  in the issue. These are observational samples, not randomized repetitions on
  controlled hardware. No warmup; the old sample misses cache, the four newer
  samples hit the same archive. Raw step timestamps, outer libtest results,
  Cargo finishes and cache evidence are in `ci-timings.json` beside this report.
- Source: `gh api repos/euri10/louiselm/actions/runs/<run>/jobs` and
  `gh api --allow-escape-sequences repos/euri10/louiselm/actions/jobs/<job>/logs`.
  Strip ANSI color, isolate the measured step's rendered `Run` block, and pair
  nested `running N tests` / `test result:` records with a stack. Count only
  outer results. Child inspector/worker timings overlap their parent's result.
  API step timestamps have one-second precision; do not select log records by
  those timestamps alone because the adjacent step can finish in the same second.
- Correctness baseline: all five hosted workflows succeeded. The current-source
  suite retains all quality gates and privileged assertions. No source or test
  behavior changed, so no artificial red test was added for this report.
- Gain/noise threshold: not set for candidate acceptance because this is an
  investigation. Recent hosted job spread is 514 seconds; a follow-up must
  measure its own repeated distributions and reject gains indistinguishable
  from runner variation. A small single-run improvement is insufficient.

## Where the hosted time goes

Seconds below are job step durations, including process/setup overhead. “Setup”
is job setup, checkout, apt and Rust installation. “Test” includes compilation
and the ordinary suite. Everything else includes admission, conformance,
documentation, later privileged gates and job/post-step overhead.

| Run | Job total | Setup | Cache restore | Lint | Test | Measured privileged step | Everything else |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| [34958590297](https://github.com/euri10/louiselm/actions/runs/34958590297), Sep 15 | 839 | 32 | 1 | 66 | 152 | 544 | 44 |
| [35299188724](https://github.com/euri10/louiselm/actions/runs/35299188724), Sep 18 | 1566 | 26 | 15 | 23 | 95 | 1324 | 83 |
| [35308123019](https://github.com/euri10/louiselm/actions/runs/35308123019), Sep 18 | 1618 | 22 | 25 | 18 | 89 | 1375 | 89 |
| [35313980920](https://github.com/euri10/louiselm/actions/runs/35313980920), Sep 18 | 1104 | 31 | 20 | 20 | 88 | 856 | 89 |
| [35315696761](https://github.com/euri10/louiselm/actions/runs/35315696761), Sep 18 | 1582 | 22 | 20 | 23 | 97 | 1329 | 91 |

The four current-source job samples have median **1574s**, range **1104–1618s**;
the privileged step has median **1326.5s**, range **856–1375s**. Other jobs finish
within 194–252 seconds in these workflows, so skills-core is the critical path.

The 514-second job spread is explained by a 519-second privileged-step spread;
the remainder of the job differs by only five seconds between the extremes.
Most individual scenarios scale together: slow/fast median **1.679×**, range
**1.584–1.726×**, excluding the daemon lifecycle case with substantial retry waits.
This supports a runner-throughput explanation; it does not identify the CPU
model, contention or instruction-set cause. Those were not recorded in hosted
logs. Compilation is not the source of that spread.

The older 9m04s privileged step is a different workload: 13 outer Rust
invocations versus 20 now, plus the Python systemd invocation. Seven added
groups (provider credentials, history isolation, revocation, cleanup, daemon
state, daemon inspection and tracker routing) consume **304.92s** of the newer
fast sample. That accounts for almost all its increase from 544 to 856 seconds.
Do not call added coverage a performance regression.

### Current expensive groups

Outer Rust times; Python and shell overhead are excluded here.

| Group | Faster run 35313980920 | Slower run 35308123019 |
| --- | ---: | ---: |
| Daemon lifecycle / restart | 122.14s | 142.98s |
| Daemon state | 58.47s | 98.87s |
| Daemon inspection | 54.05s | 90.67s |
| Tracker routing | 54.52s | 90.56s |
| Four cold resumes, combined | 137.36s | 231.01s |
| Five launch-failure scenarios | 80.94s | 136.26s |
| Revocation group | 73.44s | 125.63s |
| Exact-job verification | 66.57s | 112.34s |
| Three installed launch/effect scenarios | 62.11s | 104.28s |

## Resources and cache

All four current hosted logs report Ubuntu 24.04.5, image
`ubuntu-24.04 / 20260907.300.1`, Rust 1.97.1, and
`CARGO_PROFILE_DEV_DEBUG=line-tables-only`. Regions differ (eastus,
westcentralus, westus); region alone is not a CPU measurement. The repository
API returned `private=false` during this investigation. Current documented
public `ubuntu-latest` resources are 4 CPUs / 16 GB; private standard Linux
runners are 2 CPUs / 8 GB. Historical job logs do not prove which hardware or
visibility applied when those jobs started.
[GitHub runner specifications](https://docs.github.com/en/actions/reference/runners/github-hosted-runners).

All four restore **1,243,187,453 bytes**, key
`cargo-skills-line-tables-Linux-1.97.1-9189695eae0c894fe0019982bc826d3e24db30854d1518ddfcbef816e941b181`,
created September 17 at 07:16:45 UTC on `refs/heads/main`. Restore takes 15–25s.
The key tracks the lockfile but not source changes. Each run recompiles the
crate during `Test` (roughly 38–45s), then reports an exact cache hit and does
not save its updated build. The cache saves dependency work; it is not a fresh
snapshot of the last successful source build. Existing cache contents cannot
be replaced under the same key.
[GitHub cache behavior](https://docs.github.com/en/actions/reference/workflows-and-actions/dependency-caching).

Later Cargo calls inside/after the privileged block take about **0.07–0.09s**.
Repeated `cargo build` commands look redundant but removing them cannot recover
the privileged block's minutes. A source-sensitive cache may recover roughly a
minute of build work, less its upload/restore cost; it cannot explain or eliminate
the 14–23 minute enabled fixture workload.

## Local guest comparison

The existing `scripts/launcher-vm` guest was initially stopped. It was started
with restricted networking and no USB attachment. Committed sources and test
scripts were archived into `/home/vm/ci-8nblj.jpyc2k`; no host checkout or
credentials were mounted. Guest: Debian 13, kernel `6.12.107+deb13-amd64`,
2 vCPUs exposing AMD Ryzen AI 7 PRO 350, 3915 MiB RAM, no swap. The wrapper caps
CPU at 200%, memory at 5 GiB and runs at nice 10. Host frequencies and desktop
load were not controlled; one observed host load was 2.04/1.23/0.78.

Offline Rust 1.97.1 builds used the same line-table profile and
`CARGO_TARGET_DIR=/var/tmp/louiselm-skills-target`: bins built in 24.20s, the
library test in 21.31s, with existing dependency caches. These are preparation
costs, not a claim about a clean guest build.

The exact measured workflow block was extracted, preserving all command
arguments, namespaces, umasks and required-gate variables. Bash `time` was
added to each privileged invocation and existing `TIMEFORMAT` assignments were
changed to emit wall/user/system seconds. Commands ran serially in the guest.
No test filter or assertion was weakened. Certification necessarily exercises
different OS branches: Ubuntu must refuse unsupported certification; Debian 13
must complete the installed positive certification gate.

Extraction/timing command used from the archived host revision (guest already
started and sources already copied to the named directory):

```sh
sed -n '/^      - name: Test measured Agent and isolated tool grants$/,/^      - name: Test privileged launch supervisor composition$/{ /^          /s/^          //p; }' .github/workflows/ci.yml |
  sed -e 's/^sudo /time sudo /' \
    -e "s/^TIMEFORMAT=.*/TIMEFORMAT='CI_TIMING wall=%3R user=%3U sys=%3S'/" |
  ./scripts/launcher-vm exec env PATH=/home/vm/.cargo/bin:/usr/bin:/bin \
    CARGO_TARGET_DIR=/var/tmp/louiselm-skills-target \
    CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_NET_OFFLINE=true \
    'TIMEFORMAT=CI_TIMING wall=%3R user=%3U sys=%3S' \
    bash -c 'cd /home/vm/ci-8nblj.jpyc2k/skills-core && bash -s'
```

All **21 external privileged invocations passed**, including the Python systemd
suite's two tests. No top-level required gate was skipped. Their summed Bash
timings are **728.720s wall / 645.293s user / 21.607s system** (12m09s wall);
warm build calls add 0.10s each. These are fixture-command sums, not a full CI
job time. Raw samples are in `guest-timings.json` beside this report.

| Same enabled scenario | Guest wall | Guest user + system | CI faster / slower |
| --- | ---: | ---: | ---: |
| Installed launch/effects | 52.991s | 49.925s | 62.11 / 104.28s |
| Revocation group | 59.912s | 58.435s | 73.44 / 125.63s |
| Daemon lifecycle | 86.634s | 54.627s | 122.14 / 142.98s |
| Daemon state | 46.165s | 47.375s | 58.47 / 98.87s |
| Daemon inspection | 43.360s | 44.228s | 54.05 / 90.67s |
| Tracker routing | 44.714s | 44.751s | 54.52 / 90.56s |
| Four cold resumes | 119.533s | 111.700s | 137.36 / 231.01s |
| Launch failures | 63.878s | 62.239s | 80.94 / 136.26s |
| Exact-job verification | 55.348s | 52.492s | 66.57 / 112.34s |
| Certification, different OS branches | 39.638s | 36.314s | 7.60 / 12.77s |

Excluding certification, the guest command sum is **689.082s**. Certification
must be separated in any comparison: the extra local work is useful acceptance
evidence, not a benchmark handicap to silently erase. CPU sums cover the timed
processes and reaped children and can exceed wall time with concurrent work;
they are not a sampled function profile. Most groups are dominated by CPU work.
The daemon's roughly 32-second wall-minus-CPU gap is consistent with its real
retry/coordination waits. Guest versus CI is the same enabled workload on
different OS/hardware, not a controlled optimization A/B.

The finite cold-resume case was run twice more, serially after the full block:
**30.500, 30.367, 30.027s**, median **30.367s**, spread **0.473s**. There was no
separate discarded fixture warmup; earlier suite cases had already warmed the
binaries. This small sample indicates local repeatability, not hosted noise.

A read-only `Digest::of` diagnostic then linked the already-built guest library
and hashed each existing measured executable in memory. One untimed hash warms
each input, followed by five samples. A standard SHA-256 `abc` vector and equal
digests across samples check the harness. File reads are outside the timer;
none of these measurements changes the fixture or production hashing.

| Executable | Bytes | Median hash | Sample range |
| --- | ---: | ---: | ---: |
| louiselm-launch | 50,609,864 | 0.657350s | 0.657234–0.657521s |
| louiselm-control | 60,016,608 | 0.779275s | 0.778912–0.779514s |
| louiselm-tool-test-agent | 37,598,032 | 0.488091s | 0.487655–0.491486s |
| louiselm-tool-test-helper | 37,530,008 | 0.487148s | 0.486857–0.487691s |

One warm pass over those four files costs **2.412s**. This establishes that
repeated hashing has material CPU cost with the actual profile and binaries;
it does **not** measure invocation counts or the fraction of the whole suite
spent hashing. Installed fixture structure and the earlier controlled debug
comparison support that hypothesis; this investigation does not claim a sampled
function profile. The library-test executable adds 82,566,864 bytes, so even a
minimal shared artifact containing it and these four binaries is about 268 MB
uncompressed, before any other required files.

The diagnostic source is `digest-benchmark.rs` beside this report. After building
the archived revision inside the disposable guest, reproduce the diagnostic
from the guest's crate directory (place the diagnostic source in its parent):

```sh
export CARGO_TARGET_DIR=/var/tmp/louiselm-skills-target
export CARGO_PROFILE_DEV_DEBUG=line-tables-only
cargo build --lib --bins --all-features --locked --offline
rustc --edition=2024 -C debuginfo=line-tables-only \
  -L dependency=/var/tmp/louiselm-skills-target/debug/deps \
  --extern louiselm_skills=/var/tmp/louiselm-skills-target/debug/liblouiselm_skills.rlib \
  ../digest-benchmark.rs -o ../digest-benchmark
../digest-benchmark /var/tmp/louiselm-skills-target/debug/louiselm-launch \
  /var/tmp/louiselm-skills-target/debug/louiselm-control \
  /var/tmp/louiselm-skills-target/debug/louiselm-tool-test-agent \
  /var/tmp/louiselm-skills-target/debug/louiselm-tool-test-helper
```

After all samples, fixture passwd/group identities 60000, 4019000 and 4020000
were absent, as were LouiseLM systemd units and processes. The guest was stopped;
its source/artifacts remain for reproduction. No packages were installed and
no host privileged fixture was run.

## Causes and constraints

- `skills-core/src/launch_supervisor/installed_tests.rs:198` rebuilds a private
  release/authority fixture: copies binaries, hashes their complete bytes,
  generates signing authority, provisions a broker account, validates runtime
  configuration and stages workspace state. It is called repeatedly, including
  once per cold-resume scenario and per injected launch-failure case. Each new
  process also revalidates the authority it consumes.
- `skills-core/src/canonical.rs:155` uses `sha2::Sha256` through `Digest::of`.
  Prior controlled evidence in **louiselm-cjpep** showed a 2.9× change in an
  installed fixture merely by reducing debug metadata at the same CPU quota.
  The fix is already active. Repeated measured-byte work remains real; removing
  validations or trusting a stale path-to-digest cache is not an acceptable
  optimization.
- These fixtures share more than UID 60000: installed tests also use broker
  UID 4019000, Agent UID 4020000 and `louiselm-broker-gate`; daemon tests install
  fixed service/state paths inside private mount namespaces. Threads or shell
  background jobs on the same VM do not provide independent account/systemd/
  cgroup state. Separate fresh VMs are the existing safe isolation boundary.
- Daemon lifecycle deliberately leaves the Attention receiver absent, rejects
  an ACK, then restarts delivery. `skills-core/src/bin/control/attention.rs:10`
  implements exponential retry from 1s to 30s. Hosted markers place 64–75s
  between Agent inspection and final restart/disposal; that interval includes
  more work as well as waits, so it is not all attributable to sleep. Do not
  lower production retry bounds or remove failure assertions to shorten a test.
- Ordinary `scripts/test-skills-core` leaves the privileged flags unset. Its
  passing/fast result is not a comparable benchmark. Release-only fixtures are
  likewise not the workload under investigation.

## Candidate evaluation and recommendation

**First: one additional isolated job, independent exact-source builds.** Move
daemon lifecycle/state/inspection, tracker routing, all four cold resumes and
installed launch failures together; retain every other gate in the original
job. This moves 507.48–790.35s of measured work. Keep both groups serial.

Model: for original job time `B`, moved workload `W`, and additional worker
setup/cache/build cost `H`, completion is `max(B-W, W+H)` and aggregate runner
time is `B+H`. At `H=120s`, the four samples predict 627.48–910.35s completion
(10m27s–15m11s), using 20.4–29.0 runner minutes in total. Sensitivity to
`H=90–150s` gives about 10.0–15.7 minutes and adds 1.5–2.5 runner minutes.
The measured setup/cache is 41–51s and current test compilation roughly 38–45s;
the allowance includes additional bin compilation. A standalone worker's build
topology, queue delay and cold-cache overhead have not been measured. These
must be recorded by **louiselm-ljf7w**, not hidden behind the estimate.

The current public repository's standard hosted compute is free under GitHub's
published terms; additional runner minutes still consume capacity. If the
repository operates as the private release authority required by
`scripts/publish-plugin-release.py`, billing uses that actual account's plan and
remaining allowance. Visibility was not changed or historical billing inferred.
[GitHub public/private runner terms](https://docs.github.com/en/actions/reference/runners/github-hosted-runners).

Alternatives:

| Candidate | Expected effect / cost | Constraint |
| --- | --- | --- |
| Source-sensitive cache, **louiselm-edon3** | At most about a minute of observed build work before save/restore costs; could reduce runner minutes | Immutable keys, PR merge-ref scope, cache size/eviction; always validate exact sources with Cargo |
| One builder, two privileged artifact consumers | Idealized `outside + privileged/2`: 11.3–15.5m **before** transfer/provisioning; total baseline work plus worker setup/transfers | Measure archive/upload/download first; preserve executable modes, sibling binary paths, revision/profile identity, and current-run trust. Current cache restore alone takes 15–25s for 1.24 GB |
| More independent jobs | Can reduce the critical path further; every added build job pays another estimated 1.5–2.5 runner minutes | One job per 21 invocation groups could add roughly 30–50 runner minutes, encounter concurrency limits and multiply cache traffic. Start with one extra job |
| Change-scoped jobs | Eligible unrelated PRs could save the entire 18–27m job; release-bound commits still need full coverage | Workflow-level path filters can leave required checks pending. Shared scripts/fixtures/workflows and uncertain diffs must trigger coverage. Preserve exact main-push and actual release-PR-head checks; release automation currently validates overall CI success, not the presence of every constituent job |

Path filtering is therefore not the first change for the release wait that
prompted this investigation. A skipped job must never masquerade as verified
skills-core coverage on a release commit.
[GitHub workflow filtering semantics](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax).

The investigation leaves no implementation dependency between the two filed
follow-ups. The split is P2; cache tuning is independently measurable P3 work.
No production hashing, security, permission policy, deadlines, CI triggers or
release guards were changed.

## Acceptance and handoff

Evidence checks: five successful hosted samples retained; 21 enabled guest
invocations passed, plus two additional cold-resume passes; all 20 hash samples
passed the diagnostic's digest checks. JSON sample counts and timing sums were
checked, the diagnostic passes `rustfmt --check`, and `git diff --check` is
clean. The saved diagnostic differs from the measured source only by comments
and formatting. The report is not added to the public-site TOC; no public page
or link into this report was changed.

Stop reason: investigation acceptance complete; implementation is explicitly
separate. Lessons live in this report and the original Beads issue: distinguish
new coverage from throughput changes, exclude nested timings from sums, and
compare enabled fixtures with matched profiles. The two follow-ups consume this
evidence. No new standing repository rule is warranted.
