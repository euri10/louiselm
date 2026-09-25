# Testing and acceptance

Required by [AGENTS.md](../AGENTS.md) for executable changes, gates and CI monitoring.
Read once before applicable work. Language policies remain in force.

Public site Markdown link changes require both `npm run site:test` and the full
`npm run site:build`. MyST exports relatively linked Markdown even outside the
public TOC. Link non-curated guides through repository URLs; adding a public
page requires deliberate TOC and byte-checked artifact allowlist updates.
The artifact fixtures alone do not exercise this link traversal
(louiselm-o7v6, louiselm-ejbu, louiselm-s29b).

## 4. Required Tooling

StyLua owns formatting, `lua-language-server` owns static analysis/LuaCATS, and
`mini.test` owns tests. Do not add overlapping tools.

The direct quality gates are:

```bash
stylua --check .
lua-language-server --check . --checklevel=Warning
nvim --headless --noplugin -u ./tests/minimal_init.lua \
  -c "lua MiniTest.run()" -c "qa!"
./scripts/generate-api-appendix --check
./scripts/generate-luacats --check
./scripts/generate-vimdoc --check
./scripts/generate-plugin-version --check
```

Usage benchmark harness changes also run `python3 scripts/test-usage-benchmark.py`.
It injects SQLite exits, signals and timeout into disposable synthetic captures,
checks redacted failure artifacts and retained timings, and runs the successful
small-data measurement path. CI runs it with the Lua gates. It does not diagnose
the original large-history capture failure (louiselm-tetp1).

Plugin release automation also runs `python3 scripts/check-release-commits.py`,
`python3 scripts/test-plugin-release.py`, and the pinned Release Please API
fixtures in `scripts/test-release-please.cjs` (temporary tool setup is in the
`plugin-release-contract` CI job). See [releases](releases.md) for activation
and hosted bot-PR acceptance; local fixtures cannot certify GitHub event delivery.
Capture release changes also run `python3 scripts/capture_release.py --check`
and `python3 scripts/test-capture-release.py`; CI enforces both. A native download
is built only from an exact approved Git commit with the pinned Rust toolchain.

`capture-service/`, `skills-core/`, and `usage-cli/` are Rust crates, not Lua, and each carries
its own gates. Run them from the crate directory you touched:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-features --locked
RUSTDOCFLAGS='-D warnings' cargo doc --no-deps --all-features --locked
```

They are separate crates with separate lockfiles, not a workspace, so a gate run
in one says nothing about the others. CI runs each as a separate job.
`usage-cli` links the system SQLite library (development headers and pkg-config
are required to build it); its offline tests never read the operator's histories.

The `skills-core` browser client also has a Node.js built-in test gate (no npm
dependencies), run from the repository root:
`node --test --test-timeout=5000 skills-core/tests/recovery_browser.test.cjs`.
CI runs it alongside the Rust gates; Cargo alone does not execute the client.

Sender guard build changes also run `python3 scripts/test-sender-guard-build.py`
and `python3 scripts/test-sender-guard.py <built-louiselm-launch>`. They check
Cargo source/header invalidation, compiler failure and the embedded object's
program/map inventory. The additional `--disposable-vm` mode runs all four
existing ownership/loss proofs on those exact bytes, including the actual
upstream-write race; it requires the disposable launcher KVM, never desktop
sudo. The `sender-guard-vm` CI job provisions that guest and requires the gate.
Missing KVM/BPF-LSM/BTF is failure, not a skipped pass. These checks do not
certify installed-host acceptance. Production guard loading additionally runs
`python3 scripts/test-sender-guard-loader.py <library-test-executable>` as root
only inside that disposable guest. It uses the real Rust loader, authenticated
enrollment channel and kernel sockets; eight scenarios cover loss at actual
upstream write, cross-Session isolation, frozen enrollment and complete cleanup.
The same CI job requires both gates. A skipped ignored Rust fixture is not a
loader pass, and neither gate enables Brokered or certifies a desktop Session.

Sender guard handoff changes additionally run
`python3 scripts/test-sender-guard-handoff.py <library-test-executable> <broker-test-executable>`
as root only in that disposable guest. This required CI gate uses the production
authenticated descriptor channel, Rust broker receiver, request admission and
relay with offline runtime/upstream and receipt/status doubles. It checks distinct
UIDs and network namespaces, helper and cross-Session descriptor denial, invalid
handoff, refused upstream acknowledgements, uncertain cleanup, stale sockets,
broker crash and zero-survivor cleanup. Both arguments may
name a clean Cargo `debug/deps` directory with exactly one matching test executable.
The job also runs the existing installed-certification fixture inside the guest.
It requires the production Sender guard observation, measured libbpf/BTF/LSM
bytes, actual loader refusal after removing the probe thread's capabilities,
non-waivable guard admission, cancellation and uncertain-cleanup refusals.

Broker Skill Admission request changes also run
`python3 scripts/test-skill-requests` from the repository root. It builds both
crates' existing integration-test targets and connects separate broker and
capture-service processes over disposable authenticated sockets and starts fresh
headless Neovim observers of the resulting durable Attention. Stable Neovim must
be on PATH; this gate needs no mini.test dependency. The fixture
tests are ignored by standalone Cargo runs; this explicit cross-crate gate,
also enforced in CI, runs them with their required peer.

Linked Admission additionally runs `scripts/test-broker-admission.py` inside a
disposable VM/private mount namespace with `LOUISELM_REQUIRE_BROKER_ADMISSION=1`,
`LOUISELM_TEST_SKILLS` naming the built skills CLI and
`LOUISELM_TEST_ADMISSION_BROKER` naming the broker integration-test executable.
The skills CI job requires this distinct-UID gate. It tests the actual CLI,
read-only provisioning, durable broker handler and restart after delivery failure;
it does not certify physical signing or an installed desktop Session.

The installed Attention gate runs `scripts/test-broker-attention.py` only inside
a disposable VM/private mount namespace with `LOUISELM_REQUIRE_BROKER_ATTENTION=1`
and `LOUISELM_TEST_CAPTURE` naming the built capture CLI. It requires OverlayFS
to isolate account/provisioning writes without eagerly copying the runner's
unrelated `/etc` files (louiselm-2ar2u). Keep the CI watchdog at 120s. Static phase
markers and one 90s stack dump identify stalls without recording payloads;
`python3 scripts/test-broker-attention-diagnostics.py` tests these diagnostics
without privileges. A skipped installed gate is not privileged acceptance.

Configured capture/broker deployment also requires `scripts/test-capture-systemd.py`
with `LOUISELM_REQUIRE_CAPTURE_SYSTEMD=1` and `LOUISELM_TEST_CAPTURE` in a disposable
VM with systemd PID 1. Run as root outside a private mount namespace: the real
system manager must see the provisioned unit and policy. The gate refuses existing
deployment files and exercises the shipped hardening with distinct kernel UIDs;
the setpriv/private-mount gate alone cannot verify this identity view
(louiselm-y2j3o). Neither gate certifies the maintainer's installed deployment.

The manual ecosystem-radar workflow uses the Python standard-library gate
`python3 scripts/test-ecosystem-radar.py`. It checks routing and receipt
validation against offline fixtures; it does not certify external project
claims or make Agent research immune to hostile source text. CI runs the same
gate without network access from the test command.

The opt-in ACP backup command uses a Python standard-library suite:
`python3 scripts/test-acp-log-backup.py`. Its real encrypted backup/deletion/restore
and copy/retention/corruption tests require Restic 0.19.1; report a skip when that runtime is
missing, not a complete pass. The `acp-local-backup` CI job supplies the pinned,
checksum-verified runtime and verifies the disabled systemd unit templates.
The cloud tests replace only the transport with disposable local repositories;
passing them is not evidence of live GCS credentials, scheduling or recovery.
See [ACP log backups](acp-log-backups.md) for scope and live-acceptance limits.

For Linux `skills-core` tests, use `./scripts/test-skills-core` from the
repository root (optional Cargo test arguments follow). It closes inherited
runner descriptors and isolates Git configuration before starting Cargo;
intentional sandbox descriptor injections happen inside the tests. CI injects
an extra descriptor and synthetic Git signing configuration to gate these
boundaries. Direct Cargo under a polluted runner can trigger the bootstrap's
ambient-authority refusal (louiselm-u4c3) or personal signing in witness tests
(louiselm-8zdb).

The `skills-core` CI job sets `CARGO_PROFILE_DEV_DEBUG=line-tables-only`;
Cargo's test profile inherits it. Use the same environment when reproducing
its measured-binary gates. Full debug metadata bloats the repeatedly hashed
executables and can exhaust fixture deadlines on slow runners
(louiselm-cjpep). Line tables retain backtrace locations. The crate's dev profile
optimizes only the existing `sha2` dependency, inherited by tests, because these
fixtures repeatedly hash executables (louiselm-klm35). Project code remains
unoptimized; debug assertions, overflow checks, and security verification stay
enabled. Release settings are unchanged.

Run that suite from outside your own ACP Session. `acp-proxy` is a child
subreaper that never reaps adopted orphans, so a killed descendant lingers as a
zombie, keeps its process group probeable, and fails three `bounded_system_runner`
group-death tests on a clean tree — deterministically, with no source change
involved (louiselm-2fb5). Six sessions have now rediscovered this and one nearly
attributed it to the launcher. Wrap the script in a transient service, which
double-forks under the user manager and out of the proxy's reach:
`systemd-run --user --pipe --wait --collect --working-directory="$PWD" --setenv PATH="$PATH" ./scripts/test-skills-core`.
`--scope` does not work: it leaves the process in the same parent chain.

Fix formatting with `rustfmt --edition 2024 <the files you changed>`, not bare
`cargo fmt`. The crate-wide command rewrites every unformatted file, and in this
tree that includes whatever another session is editing right now — the same
hazard as `git add -A`, one step earlier. For the same reason, a `cargo fmt
--check` or full-suite failure in a file you did not touch is someone else's
work in progress: report it, do not fix it.

All manifests enforce the strict [Rust policy](agent-rust.md). New Rust packages
must configure the same lints and CI gates from their first implementation.

These went unenforced for `capture-service`'s whole life until
`louiselm-ci-missing-rust-gates-5o5h`: the 56 tests included the three
regressions guarding `louiselm-capture-receiver-reachability-rnjc`, a defect that
cost a multi-hour physical Android QA round to verify, and CI would have stayed
green through a reintroduction. `android/` runs its separate
`./gradlew test lint assembleDebug` gate in CI; Kotlin/Android policy and
Robolectric requirements live in `android/AGENTS.md`.

`generate-luacats --check` fails whenever `config.lua`'s schema changed without
regenerating `lua/louiselm/types.lua`, the `louiselm.Config` class that gives
users completion inside `setup({...})`. Run `./scripts/generate-luacats` (no
`--check`) and commit the result alongside the schema change. Note the direction:
this generator reads the schema and writes annotations, the opposite of
`generate-api-appendix`, which reads annotations and writes `doc/api.md`.

A generator that shells out to an external exporter must validate that
export before trusting its exit status. An incomplete export can render as a
well-formed but shorter artifact: `--check` reports stale documentation, while
an unchecked write can discard valid entries. Check a property the export
cannot plausibly lose, fail with the exporter named, and leave the artifact
untouched.

An export can also be complete but rendered in the wrong mode. The captured
`louiselm-qbr.9.9.7.1` recurrence (CI run 35958957156) had every entry, but its
aliases were rendered by name. LuaLS 3.19.1 `--doc` sets `hover.expandAlias` to
false only on its fallback config scope. It loads the folder config, where the
default is true, asynchronously, so either value can apply at render time.
Replaying `hover.expandAlias=false` through `--configpath` reproduced the CI diff
line for line. The gate therefore rejects an alias whose view is its own name,
and the generator retries the export (three attempts). Do not pin the setting
to false instead: the export is then deterministic, but every alias body is
lost. When a tool races its own configuration, prove the mechanism by forcing
the setting before you choose a fix.

Check it at the granularity the exporter truncates at. This gate first asked
only that each curated *section* hold an entry, but LuaLS truncates per file:
an export missing whole files still populated every section, exited 0, and
deleted 133 lines from `doc/api.md` (louiselm-809u1). Where a unit legitimately
exports nothing, name it in data the check reads and reject that exemption once
it stops being true — an allowlist nobody rechecks is a hole in the gate.

`generate-api-appendix --check` fails whenever a public LuaCATS annotation
(`---@field`, `---@class`, exported function signature, etc.) changed without
regenerating `doc/api.md`. Run `./scripts/generate-api-appendix` (no `--check`)
and commit the result in the same commit as the annotation change — a doc
regen split into a follow-up commit is the failure mode this gate exists to
catch (commit 849fc67 added `session.Options.env` without one, breaking CI on
main until a follow-up commit regenerated it).

Use a repository wrapper if it becomes the documented entrypoint. CI must
enforce all gates once their configuration/harness exists. Report unavailable
gates; never claim they passed or add unrelated tooling merely to run them.

Manual test workflow from the repository root:

```bash
./scripts/install-test-deps
nvim --headless --noplugin -u "$PWD/tests/minimal_init.lua" \
  -c 'lua MiniTest.run()' -c 'qa!'
```

`install-test-deps` provisions mini.nvim and nothing else. The skills specs
additionally need `lyaml`, which the script deliberately does not install:

```bash
luarocks --lua-version 5.1 install lyaml
eval "$(luarocks path --lua-version 5.1 --no-bin)"   # in the shell that starts Neovim
```

Without it the suite fails 36 cases across four spec files rather than
reporting one missing dependency, because `skills/discovery.lua` degrades to a
`missing_dependency` diagnostic and the specs compare it against parsed skill
data. CI installs `lyaml` itself, so this gap is only ever hit locally
(louiselm-zpsj).

Run one file with `MiniTest.run_file("tests/schema/dsl_spec.lua")`. For
interactive debugging, start `nvim -u ./tests/minimal_init.lua` and run
`:lua MiniTest.run()`; starting Neovim without `--headless` intentionally keeps
the editor open.

## 5. Test-Driven Development

Use red-green-refactor for meaningful behavior changes: write the smallest
failing test, confirm it fails for the intended reason, implement only enough
to pass, then simplify while green. Pure refactors start from passing
characterization coverage.

Documentation, formatting, generated output, and trivial mechanical changes do
not need an artificial red test. State why when test-first work is impractical.

Deleting or regenerating tests is justified only when a recorded behavior
contract intentionally changes and the old tests primarily describe obsolete
behavior; implementation churn alone is not enough. Before removing coverage:

- Record the obsolete behavior and its replacement contract.
- Preserve or port coverage for unchanged public APIs, protocol validation,
  security, lifecycle/disposal, async/fast-event scheduling, UI boundaries,
  and error paths.
- Where practical, show that replacement tests fail against the displaced or
  otherwise wrong behavior. Judge preserved behavior, not test count or a green
  check alone.

For all tests:

- Test observable behavior through public APIs, not implementation details.
- Cover non-trivial branches, parsers, state transitions, cancellation, and
  error paths without requiring a test per function or coverage percentage.
- Assert which refusal a test got, not only that it failed. An `is_err()` or
  `is_ok()` check on a policy lets an unrelated guard — an expired deadline, a
  revoked capability — satisfy the assertion and report the wrong defect. Where
  the case crosses a real deadline, size that deadline so load cannot close it,
  and keep expiry its own case. `grant_authority.rs` asserted only `is_ok()`
  while executing 65 durably audited operations against a 1000ms grant, so a
  loaded runner failed count policy with `Expired` and read as a budget bug
  (louiselm-6bnxq).
- A feature gated on optional configuration needs a test for the **unset**
  case, and you must check what the maintainer's real configuration actually
  does before reporting the feature as working. Testing both branches of a gate
  proves only that the gate works, never that it fires: `louiselm-5tuq` shipped
  green, with passing tests on either side of its condition, and was inert for
  months of wall-clock because every real agent entry omitted the optional field
  the default was keyed to. Green tests are not evidence that a conditional
  feature is reachable.
- A generator is not done when its output function is tested; it is done when
  its artifact is committed, consumed by something, and held current by a
  `--check` gate. `gen_luacats.lua` passed its unit tests from 2026-08-06 while
  writing no file and being referenced by nothing, so users got no `setup()`
  completion until the artifact was connected to its consumer
  (`louiselm-luacats-generator-inert-5y8i`). Prove the artifact reaches its
  consumer: for a type file that means running `lua-language-server --check`
  against a scratch workspace that requires the plugin the way a user does, not
  asserting on the generator's return value.
- Never pump input with `getchar()` in a test that observes `vim.on_key()`.
  Neovim 0.13 stopped invoking `on_key` callbacks for keys `getchar()` consumes,
  so the observer silently never runs and the feature looks broken on nightly
  while stable stays green (louiselm-dhqgv). Feed the key instead, and use
  `nvim_feedkeys(key, "nx!", false)`: plain `"x"` completes the pending command
  with a trailing `<Esc>`, which arrives after your key and supersedes whatever
  work it queued. `nvim_input_mouse` still sets the position `getmousepos()`
  reports, so it stays the way to place the cursor.
- Async tests must model the production callback context, not only invoke the
  callback synchronously. Directly calling a process or transport callback is
  insufficient coverage for fast-event behavior.
- Keep tests deterministic: no network, credentials, or real agent binaries.
  Use the mock ACP agent for process/protocol integration tests.
- Dispose every Session a test creates through `MiniTest.finally`, never as the
  last statements of the case body. Session registries are process-wide
  (`lua/louiselm/session/registry.lua`), so a case that fails before its own
  cleanup leaks a live Session into every later cross-API case — `exit_verdict`
  and `identity` then fail with counts nobody changed, and the real failure
  looks like three unrelated regressions. Cleanup belongs where a failed
  expectation cannot skip it.
- When a change depends on a real ACP peer, derive its fixture from a captured
  log frame rather than inventing the payload. Cite the log path and Session ID
  in the test or its comment so observed shapes are distinguishable from
  assumptions.
- Fixtures must encode observed event order as well as field shape. If the
  order was not observed, state that explicitly in the test.
- The mock ACP agent is a test double, not a protocol specification. If it
  accepts input that a real Agent rejects, treat that permissiveness as a
  defect and tighten the mock.
- Give UI code behavioral headless smoke tests, not pixel assertions.
- UI async tests must cover queued work arriving after disposal and prove that
  editor operations occur only after the required scheduling boundary.
- Run focused tests while developing and the complete suite before handoff.

### Live acceptance

For a defect reproducible in the maintainer's running application, state the
acceptance in the maintainer's observable terms before implementation. Inspect
the live instance and trace the affected value end to end, then encode the real
structural event ordering in a regression without copying sensitive payloads.

Automated gates do not close such a defect while the exact live reproduction is
available: keep it open until the maintainer confirms the acceptance. A
diagnostic- or tracking-only commit is not a build to retest; say so explicitly
and do not sync it as though behavior changed.

### Background CI monitoring

A `Monitor` loop that greps a CLI's human-readable status words to decide when
to stop is exposed to a silent-hang failure: guess the wrong token (e.g.
`passed` when the CLI actually emits `success`) and the loop never exits, the
promised report never fires, and neither the agent nor the user gets any
signal until someone gets impatient and interrupts (louiselm-w726).

- Before starting a Monitor loop that greps a CLI's status output, run the
  command once first and read the actual token vocabulary rather than
  guessing.
- Prefer a machine-readable check (exit code, `--json` field) over grepping
  human-facing status words, where the CLI supports it.
- Always emit a heartbeat line inside the loop body on every poll (e.g.
  `echo "poll: $(date +%s) status=..."`), regardless of match, so Monitor's
  stdout-driven notifications fire periodically even before the exit
  condition is met — a loop that only prints after exiting produces zero
  notifications the entire time it runs.
- Treat "I'll report back once it finishes" as a promise that needs a
  periodic self-check-in (re-poll via `TaskOutput`) rather than pure trust
  that the notification will fire.
