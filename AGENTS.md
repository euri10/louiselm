# louiselm.nvim Development Contract

## 1. Project Scope

louiselm.nvim is an early-stage Neovim plugin with no external users. Breaking
changes are acceptable. Change the code directly; do not add compatibility
shims, deprecated aliases, or migration paths unless explicitly requested.

Priority: **correctness (including safety and security) → clarity → simplicity →
performance**.

Fix root causes at the narrowest shared boundary. Prefer deletion, existing
code, Lua/Neovim facilities, and plain functions over new abstractions. Do not
build extension points for hypothetical consumers. The established extension
boundaries are the headless session API, typed events, permission policies, and
agent configuration.

## 2. Work Tracking

Work is tracked in beads (`br`). `.beads/` is committed; its JSONL export is the
shared record across sessions, agents, and adapters.

When asked to recommend or list the next best task(s), that is a read-only
triage answer: report issue ID(s), priority, and one-line reasoning, and stop.
Do not claim, implement, or commit work unless asked to proceed.

- Run `br robot-docs guide` for command syntax. Do not restate it here.
- Discover work with `br ready`. Triage with `bvr --robot-*` flags only; bare
  `bvr` opens a blocking TUI.
- Pass `--actor "<your session id>"` to every mutating `br` command — `create`,
  `update`, `close`, `delete`, `comments add`, `dep add`, `label add` — not just
  claims. The actor is your adapter's session identifier (Claude Code exports
  `CLAUDE_CODE_SESSION_ID`; other adapters may name their session, e.g.
  `deepseek/session-<uuid>`). It must point at a transcript someone can actually
  open. If your adapter exposes no session id, ask the user before mutating. Do
  not use the OS username, `br config`'s `_computed.actor`, the br skill's
  `BR_ACTOR:-assistant` fallback, or `robot-docs`' `$AGENT_NAME` — none of
  those identify your session.

Adapter lookup recipes:

- Claude Code: use `claude/<CLAUDE_CODE_SESSION_ID>` when that variable is
  present. Claude's `--resume` and `--continue` options refer to persisted
  sessions, but do not replace the current session ID with one copied from a
  session picker.
- LouiseLM ACP sessions (including OpenCode, Codex, DeepSeek, and Copilot):
  query the live Chat controller described in Live-instance introspection and
  use its exact `<agent>/<ACP-session-id>` result. This is the current-session
  source even when the adapter exports only process markers.
- OpenCode outside LouiseLM: `OPENCODE=1`, `OPENCODE_PID`, and
  `OPENCODE_CLIENT=acp` identify the process, not the current session.
  `opencode session list --format json` is an inventory, not proof of which
  session is current; use an ID from the current interaction or ask.
- Codex outside LouiseLM: `codex agents` lists persisted local sessions, but
  a listed ID is not current-session attribution by itself. Use the current
  interaction's `codex/<id>` or ask when it is not exposed.
- DeepSeek, Copilot, and any other adapter: a historical `deepseek/<id>` or
  `copilot/<id>` actor proves only the stored shape. Use the exact ID exposed
  by the current interaction or live ACP Chat; never synthesize one from a
  process name, PID, or old Beads record.

Bare UUIDs and `assistant` actors are legacy records, not valid sources for a
new mutation. If no recipe yields an attributable current ID, stop and ask the
maintainer rather than falling back to one.
- Claim work with `br update <id> --claim --actor "<your session id>"`, never
  with a bare `--status=in_progress`. `--claim` atomically sets the assignee to
  the actor, which is the only thing that makes a claim attributable, and it
  refuses to overwrite a live claim. `bvr` suggests the bare
  `--status=in_progress` form in its `claim_command` field; do not copy it.
- Read the graph through `br` and `bvr`, never by parsing `.beads/*.jsonl`.
- File findings as beads before fixing them, including work deliberately
  deferred. File before the fix, while the failing evidence still exists: the
  log line, payload, or hung process that proves it disappears the moment it is
  repaired. A finding that survives only in a transcript is lost, and deferred
  work that is not filed becomes work that never happens.
- Emit, don't narrate: if you could not complete the work you claimed, or you
  noticed something outside it, file or update a beads issue before ending the
  session. Do not leave either as a comment, a handoff summary, or a transcript
  note — those are read by no one and no command. The test for label versus
  blocker: does something have to change before anyone can do this? Yes is a
  dependency; model it as one. No — a standing requirement or missing
  capability rather than something to unblock — is a label. `needs-capability:*`
  is the namespace for the second case (e.g. `needs-capability:image-generation`
  for work only an agent with raster image generation can do); attach it and
  move on, do not restate the requirement in prose.
- Model aggregate completion with one dependency direction. Do not combine a
  child's `parent-child` dependency on its aggregate with the aggregate's
  explicit dependency on that child: `br ready` treats both as blockers while
  `br dep cycles` does not report the mixed-edge deadlock.
- Prefer finishing **your own** in-progress work to starting new work, even
  when triage ranks an unstarted issue higher. Scoring rewards unblocking
  leverage and cannot see that a claimed issue is half-done. This preference
  covers only issues you claimed; another agent's claim is not your backlog.
- Before working an issue that is already `in_progress`, run
  `./scripts/agent-liveness-snapshot --status`. This project does not use
  Agent Mail and will not; the script supplies the reservation snapshot
  `br coordination status` needs by reading each session's transcript mtime,
  and without it every claim classifies as `no_mail_snapshot` and no claim can
  ever be released. Do not call `br coordination status` bare, and never hand
  it an empty snapshot: an empty file asserts that no session is alive, which
  makes age the only evidence and takes work away from an agent that is simply
  busy. A session counts as gone after 45 minutes of transcript silence
  (`LOUISELM_LIVENESS_WINDOW_MINUTES`). An assignee the script reports as
  unresolved on stderr has no transcript to judge, so treat its claim as live.
  A live claim belongs to its holder: pick something else. A claim is eligible
  for takeover only when its classification is `abandoned_likely` and
  `reclaim_allowed_by_policy` is true. That flag alone is not enough — br also
  sets it at `stale_candidate`, two hours in, and this project requires the
  eight-hour `abandoned_likely` bar as well, so that a takeover needs both a
  silent transcript and a silent issue. Age alone is not permission either:
  `no_mail_snapshot`, `ambiguous`, `fresh`, and
  `blocked_by_active_reservation` remain protected regardless of age. Record
  the command's evidence summary in a comment, then have the maintainer requeue
  the issue with `br update <id> --status open --assignee "" --actor "<session id>" --json`;
  the next Agent claims it normally with `--claim`, which refuses to overwrite
  an intervening holder. `br scheduler` already excludes claimed work and
  explains its ranking, so prefer it over hand-scanning
  `br list --status in_progress`. Exception:
  `br scheduler --json` returns empty labels (louiselm-scheduler-labels-xw1i),
  so capability-aware selection cannot filter on its output — use
  `br ready --label-any needs-capability:<name>` instead. Drop this carve-out
  once that bug closes.
- Keep durable context in the repository, never in an agent's private memory.
  Context anchored to one issue belongs in `br comments`; a standing convention
  belongs in this file. Anything an agent knows that another adapter cannot
  read is a defect in the record.
- Before running `br close` on a `bug`, `task`, or `feature`, route what the
  work taught: issue-local evidence and false starts to a `br comment`, a rule
  that should bind future work to this file, an unresolved question to a
  `needs-design` question. Name the destination or say none is warranted;
  silence is the failure mode, because the lesson is legible only while the
  work is still fresh. Chores and mechanical closes are exempt from
  lesson-routing and prior-lesson search, but still need a close verdict —
  `none:mechanical` makes that exemption a checkable claim instead of a silent
  assumption; see the Close verdict gate below.
- Every `br close` on this project must carry exactly one typed verdict naming
  what makes the close checkable by someone other than its author, enforced by
  `.beads/policy.yaml`'s `close_policy.require_typed_references` gate. The five
  kinds are a partition, not a menu — pick the one that actually applies:
  `consumer:` (production code reaches this, e.g.
  `consumer:lua/louiselm/ui/chat/init.lua:11`), `gate:` (a check fails if it
  stops being true, e.g. `gate:scripts/generate-luacats`), `live:` (the
  maintainer confirmed it running, e.g.
  `live:codex/01a050f8-93e9-7f00-847f-be08e6920e77`), `inert:` (not reachable
  yet — requires a filed follow-up issue), and `none:` (nothing to verify, e.g.
  `none:mechanical`). A reason whose only typed reference is a built-in kind
  such as `commit:` does not satisfy the gate — built-ins are not in
  `required_kinds`, deliberately, so a stray commit citation cannot stand in
  for naming a verdict. A parent's verdict may be no stronger than its weakest
  child's: if any child closed `inert:`, the parent closes `inert:` too. This
  is a convention the gate cannot see, not a rule `br` enforces — a violation
  is a visible contradiction in the shared record, not a rejected command.
- Write close reasons, comments, and commit bodies in the fewest words that
  stay greppable: name the decision, state the outcome, cite `file:line`
  where one applies. Do not restate the work as prose or re-explain what the
  diff already shows. Compress, never omit — the routing rule above still
  binds, and a lesson dropped to save a line costs far more than the line
  saved.
- `br search` excludes closed issues unless passed `-a`, and lessons live on
  closed issues almost by definition. Always search prior art with `-a`, and
  never read a zero-result search as absence without it. `br list --json`
  carries no comments field at all, so it cannot scan comment text however it
  is filtered (louiselm-o2jh).
- File a design question worth a full `louiselm-grill-me` session as a beads
  `question` issue, labeled `needs-design`, titled "Grill-me needed: <question>". Open
  with a line telling the grill not to resolve the question in passing, then
  the question itself, the context/evidence that raised it, and the threads
  worth pulling — not a pre-baked answer.
- When claimed work reveals multiple independently completable deliverables,
  create child tasks instead of hiding them as serial "next slices" inside the
  original issue. If a clean slice depends on an unresolved product or
  architecture decision, create a blocking `needs-design` question and pause
  implementation for a focused grill-me session.
- Run `br sync --flush-only` before committing, and commit `.beads/` alongside
  the work it describes.
- Stage the paths you changed by name. Never `git add -A`/`git add .`. Other
  sessions work this tree at the same time, and a blanket stage silently sweeps
  their in-flight edits into your commit — the work is not lost, but it lands
  under someone else's message and their own commit then looks incomplete. This
  happened in commit `7b7dd90`, which absorbed a `tests/session/api_spec.lua`
  case belonging to the fix committed separately as `c5a7046`. Run
  `git status --short` before staging and account for every path you did not
  write.

### Project vocabulary

The canonical project vocabulary lives in Beads as `vocabulary/reference`
records. Definitions, usage distinctions, and provenance belong there; do not
duplicate term definitions in this file. For any semantic work session —
planning, implementation, review, or issue filing — load the applicable terms
once before proposing work:

```bash
br list --type vocabulary --status all --format toon
```

Consume only records whose issue type is `vocabulary` and whose status is
`reference`. The all-status query is intentional: a project with no vocabulary
yet must return an empty result even though `reference` is a custom status.
Pure housekeeping and mechanical changes may skip the load. Do not scan the
repository, history, or backlog for missing terms, and do not treat absent
entries as debt.

Use canonical terms when harmless naming drift appears. If competing meanings
would materially change architecture, lifecycle, protocol behavior, authority,
or user-visible behavior, stop and ask for clarification. When vocabulary,
code, a work item, or current user intent materially contradicts another
source, neither source wins automatically: create or reuse a `needs-design`
question, resolve the conflict with the user, and then promote the agreed
definition. Re-read affected records immediately before promotion to detect
concurrent semantic changes. Never initialize Beads implicitly, mutate
vocabulary silently, or promote a term before the user's shared-understanding
confirmation.

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

The maintainer's own loop — louiselm-grill-me, louiselm-to-beads, sessions,
louiselm-qa-review — is described in [docs/example-workflow.md](docs/example-workflow.md).
That document is an example, not a contract. It binds nobody, and other loops
are expected.

## 3. Runtime and Dependencies

- Target the latest stable Neovim at release and its embedded LuaJIT/Lua 5.1.
  Do not support standalone Lua or add Lua 5.2+ syntax, APIs, or version shims.
- Lua runtime dependencies are forbidden; use LuaJIT and stable Neovim APIs.
  One named exception: `lyaml`, which `skills/discovery.lua` uses to parse
  SKILL.md frontmatter. Hand-rolling a YAML subset is explicitly rejected —
  frontmatter parsing is load-bearing rather than incidental, since a
  persisted workflow may itself be a frontmatter document discovered by the
  same mechanism (louiselm-4gif, louiselm-uqip). This is one exception with a
  rationale, not an open door; the bullet below still governs everything else.
- `mini.test` is the sole *direct* Lua test dependency. The suite additionally
  needs `lyaml` present, reached through the skills code rather than required
  by a test — see section 4, which gives the install commands and explains why
  its absence surfaces as scattered assertion failures.
- Any additional development dependency requires a demonstrated gap and
  explicit approval. Never vendor a dependency or utility for convenience.
- `sqlite3` >= 3.38 with JSON support is a required runtime/test executable
  (approved in louiselm-3x9p). Turn recording uses asynchronous CLI calls,
  rollback journal DELETE and synchronous EXTRA; do not enable WAL.

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
```

`capture-service/` and `skills-core/` are Rust crates, not Lua, and each carries
its own gates. Run them from the crate directory you touched:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-features --locked
RUSTDOCFLAGS='-D warnings' cargo doc --no-deps --all-features --locked
```

They are separate crates with separate lockfiles, not a workspace, so a gate run
in one says nothing about the other. CI runs both as separate jobs.

For Linux `skills-core` tests, use `./scripts/test-skills-core` from the
repository root (optional Cargo test arguments follow). It closes inherited
runner descriptors before starting Cargo; intentional sandbox descriptor
injections happen inside the tests. CI injects an extra descriptor to gate this
boundary. Direct Cargo under a polluted runner fails the bootstrap's correct
ambient-authority refusal (louiselm-u4c3).

Both manifests enforce the strict Rust policy in section 6. New Rust packages
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

### Live-instance introspection

When your shell is a child of the Neovim instance hosting the plugin — the
normal case here — `$NVIM` is that editor's RPC socket. Inspect live plugin
state through it before scraping environment variables, process tables, or
state files: those miss state owned by a running adapter and can reconstruct
a different answer than the live one.

```bash
nvim --server "$NVIM" --remote-expr 'luaeval("...")'
```

For multi-line queries, write a temporary Lua file and run
`luaeval("dofile('/tmp/query.lua')")` from the same remote expression.

`acp-proxy` splits one conversation across two log files. Everything sent
before the Agent returns a session id — `session/new` and its `_meta`, which is
where session construction is decided — lands in
`~/.local/state/acp-llm-adapter/proxy/{connections/<connection-id>.jsonl}`, not
in `sessions/<session-id>/log.jsonl`. Searching only the session directory
makes construction-time configuration look like it was never sent
(`louiselm-wh2l` was filed on exactly that mistake). Resolve a session back to
its connection with `grep -l <session-id> .../proxy/connections/*.jsonl`, which
works because the bind is recorded there. When checking whether a value reached
the Agent, match its structure (`'"thinking":{'`) rather than a word from it:
bare terms like `summarized` also appear in ordinary session prose and read as
false confirmation.

The shell-side equivalent of `:LouiselmSessionId` is to evaluate Lua in the
live instance and ask the chat controller for `session_id()`. Today that
controller is the `chat` upvalue of
`require("louiselm.ui.chat.command").winbar_click`; reach it with
`debug.getupvalue` and call `chat:session_id()`, which returns
`<agent>/<ACP-session-id>`. This is a debugging reflection path, not a public
API; if it becomes recurring tooling, promote it to a documented accessor
rather than encoding the upvalue walk.

Use that returned value verbatim as the Beads actor when it is available. This
is the authoritative identity of the current live Session and does not depend
on an adapter environment variable; in particular, an OpenCode result shaped
as `opencode/<ACP-session-id>` is usable even when the shell exports only
`OPENCODE=1`. Do not substitute an ID copied from an old Beads record, a
different Session, or a child-agent task. If the live query and current
interaction context expose no attributable ID, ask before mutating Beads.

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
  completion for eight months
  (`louiselm-luacats-generator-inert-5y8i`). Prove the artifact reaches its
  consumer: for a type file that means running `lua-language-server --check`
  against a scratch workspace that requires the plugin the way a user does, not
  asserting on the generator's return value.
- Async tests must model the production callback context, not only invoke the
  callback synchronously. Directly calling a process or transport callback is
  insufficient coverage for fast-event behavior.
- Keep tests deterministic: no network, credentials, or real agent binaries.
  Use the mock ACP agent for process/protocol integration tests.
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

## 6. Language Style

### Lua

- Use `snake_case` for files, modules, functions, and variables; `PascalCase`
  for LuaCATS types; `UPPER_SNAKE_CASE` only for true constants.
- Keep variables/functions local unless exported in the module API. Accidental
  globals are defects. Return one small public API table per module.
- Use dot calls for stateless module functions and colon methods only for
  instance state. Prefer guard clauses and early returns over deep nesting.
- Keep functions and modules cohesive; do not enforce numeric line limits.
- Prefer plain tables and functions. Use metatables only when they materially
  simplify identity or lifecycle.
- One implementation does not justify an interface or factory.
- Comments explain constraints and non-obvious reasons, not syntax. Do not
  leave speculative TODO scaffolding.

#### Table Semantics

- Do not mutate caller-owned tables unless documented. Copy only at ownership
  boundaries; do not deep-copy defensively everywhere.
- Keep array-like tables dense. `#table` is valid only for dense sequences.
- Never depend on `pairs()` order. Only `false` and `nil` are falsey; `0` and
  `""` are truthy.
- Do not silently coerce strings, numbers, booleans, or missing values.
- Make in-place mutation explicit in its name or API documentation.

### Rust

These rules apply to both Rust crates, including their binaries and tests.
Keep shared policy here; crate-local instructions may add concrete constraints
but must not silently weaken it.

#### Toolchain and lints

- Use stable Rust matching `RUST_VERSION` in CI and default rustfmt. Keep the
  edition explicit in each manifest. Declare an MSRV only when it is tested;
  do not claim compatibility from an untested `rust-version` field.
- Configure package-wide Cargo lints so libraries, binaries, and tests are all
  covered. Deny `clippy::all` and `clippy::pedantic`, with group priority `-1`;
  individual lints keep priority `0`. Also deny `missing_docs`,
  `unsafe_op_in_unsafe_fn`, `clippy::unwrap_used`, `clippy::expect_used`,
  `clippy::panic`, `clippy::todo`, `clippy::unimplemented`, and
  `clippy::undocumented_unsafe_blocks`. The unsafe-code level is specified below.
- Fix lint findings. Never disable `warnings`, `all`, or `pedantic`, or add
  crate/module-wide production suppressions. A narrow exception must name the
  lint and explain why the code is correct and clearer as written. Prefer
  `#[expect(..., reason = "...")]` when the lint is expected to fire; use a
  justified `#[allow]` only when an expectation is inappropriate, such as a
  configuration-specific diagnostic. "Clippy is noisy" is not a justification.
- Do not suppress missing API/error/panic documentation or increase lint
  thresholds to avoid fixing code. Split mixed responsibilities; do not create
  artificial helpers merely to satisfy a length lint. A cohesive function may
  receive a narrow, justified exception under the preceding rule.

#### Unsafe and failure handling

- Forbid `unsafe_code` in capture-service. Deny it by default in skills-core;
  exceptions are limited to reviewed platform operations. Before adding or
  expanding one, record why safe stdlib/existing-dependency APIs do not suffice,
  the alternatives considered, and the evidence supporting the chosen boundary.
  Existing unsafe code receives no automatic exemption.
- Each unsafe operation needs a precise `// SAFETY:` argument covering its
  actual obligations, including descriptor ownership and post-fork restrictions
  where applicable. Keep unsafe blocks minimal and encapsulate them behind a
  safe API that enforces its invariants. Tests supplement the argument; a green
  test or the absence of the keyword does not establish soundness.
- No `static mut`. Raw-pointer operations, manual `Send`/`Sync`, FFI, and
  lifetime manipulation follow the same unsafe exception policy; they are not
  shortcuts around Rust's ownership model.
- Expected input, I/O, configuration, and protocol failures return typed
  `Result` errors. Preserve their causes and sanitize them at presentation
  boundaries. No production `unwrap()`. An invariant-only `expect()` or panic
  needs a narrow lint exception explaining why external input cannot violate
  the invariant; document any exposed panic conditions. Never use assertions
  as external-input validation.
- Assertions, `unwrap()`, `expect()`, and explicit panic branches are permitted
  in tests. Their lint allowances belong only to test functions, `#[cfg(test)]`
  modules, or integration-test crates, with a test-fixture justification. Never
  disable panic/unwrap lints for a library merely because it is built for tests.
  `todo!()` and `unimplemented!()` are forbidden in committed code, including tests.
- Do not swallow errors or substitute defaults for failed validation. Ignoring
  a best-effort cleanup error requires a comment explaining why correctness and
  security remain intact. Cleanup that proves containment or permits identity
  reuse is not best-effort.

#### Ownership, boundaries, and concurrency

- Prefer `&str`, `&[T]`, and `&Path` when ownership is unnecessary. Clone only
  for a concrete ownership requirement; do not add `Arc<Mutex<_>>` to bypass a
  design problem. Use enums for mutually exclusive states and checked
  conversions/arithmetic for external sizes, indices, and identifiers.
- Keep helpers private or `pub(crate)` unless consumers need them. Separate
  policy and state transitions from filesystem, process, and transport effects.
  Use plain functions and existing dependencies before new traits, layers, or
  crates. Do not add dependencies solely to hide an unsafe block.
- Keep Tokio as capture-service's async runtime; do not introduce a second
  runtime or require one in synchronous skills-core code. Blocking filesystem,
  process, and network work must stay off async executor threads. Do not hold
  synchronous locks across `.await`; keep lock scopes short and document lock
  ordering when multiple locks are acquired.
- Spawn processes with argument arrays and explicit environment/cwd where
  relevant. Every task and child process has an owner responsible for completion,
  cancellation, and cleanup. Disposal must handle late callbacks and prove that
  confined descendants are gone before releasing their authority or identity.
- Document exported APIs with Rustdoc, including meaningful `# Errors` and
  `# Panics` sections where applicable. Document ownership, blocking behavior,
  and lifecycle obligations when callers must act on them. `cargo doc` alone
  does not enforce missing documentation; keep the lint enabled.
- Apply section 5's behavior-first tests to Rust too. Preserve real concurrency,
  failure, and privileged-boundary coverage. A passing unit suite does not
  replace required host conformance tests or maintainer acceptance of the actual
  installed workflow. No mandatory new property-test or benchmark dependency.

## 7. Types and Documentation

For Lua, LuaCATS is mandatory for public APIs, configuration, protocol values,
callbacks, and events. Do not annotate trivial locals when inference is clear.
Rust uses the Rustdoc and lint requirements in section 6.

Every exported function documents its purpose, parameters, returns, and failure
behavior. Comment internals only where intent or an invariant is not obvious.
Change generated files through their source or generator, never by hand.

Treat diagnostics as errors: no unresolved globals, undefined fields, unchecked
nullable values, type mismatches, or uncertainty hidden behind `any`. Never
disable a diagnostic for a file/project. A narrow
`---@diagnostic disable-next-line` requires an unrepresentable external API and
a comment explaining why.

## 8. Architecture and Lifecycle

- Separate transformation, validation, and state transitions from Neovim UI,
  filesystem, and process I/O.
- `require()` must only define and return APIs. Importing a module must not
  start processes, create buffers, register autocmds/keymaps, or mutate options.
- Explicit `setup`, construction, start, cancel, and dispose own side effects.
- Mutable state has a clear owner/lifecycle, never a hidden module singleton;
  sessions live in the explicit registry.
- Repeated setup/dispose cycles must not leak handlers, buffers, or processes.
- Avoid circular module dependencies. Split modules when responsibilities
  diverge, not in anticipation of future growth.
- A seam that will eventually be backed by process, socket, or filesystem I/O
  takes an asynchronous signature from the start, even while its only
  implementation is an in-memory double. A double that answers synchronously
  freezes a synchronous contract into the interface, and nothing surfaces the
  mistake until the real backend arrives and the signature cannot express it —
  at which point every caller written against it has to change too. Give the
  double the production callback shape as well, per section 5.
  `louiselm-qbr.3.2.6.5.2` rewrote `workflow/executor.lua`'s just-merged ledger
  API for exactly this reason; it was cheap only because the seam still had zero
  production callers.

Core modules never notify, print, open UI, or choose presentation. They return
structured errors and typed events; setup, health, and UI modules display them.

## 9. Errors and Validation

- Expected failures return explicit values such as `nil, err` or `false, err`.
  Check both results of every `pcall`; never swallow its error.
- Reserve `error()` for violated internal invariants or clear API misuse.
- Use `assert()` freely in tests, not as production input validation.
- Preserve context internally, sanitize user messages, and make user, config,
  protocol, and process failures specific and actionable.

Configuration uses a closed schema:

- Validate the untouched user table before defaults. Unknown keys/types are
  errors; collect all errors in one pass.
- Never hide invalid input with permissive deep merging or mutate user config.
  Apply defaults only after validation succeeds.
- Deprecations warn with an exact replacement path; do not silently translate
  old keys. Do not keep a removed key in the schema merely to carry that
  message: a declared field is emitted into `lua/louiselm/types.lua` and
  `doc/louiselm.txt`, so the migration note becomes a completion entry
  advertising a key `setup()` always rejects
  (`louiselm-types-advertise-full-content-naz6`). Delete the field and let the
  closed schema reject it as an unknown key, which section 1 already prefers.

For ACP/JSON-RPC, validate consumed fields and reject malformed/contradictory
messages, but ignore unknown optional fields from newer peers.

## 10. Neovim APIs, Async Work, and Processes

Prefer APIs in this order:

1. Stable, non-deprecated `vim.*` Lua APIs
2. Structured `vim.api.nvim_*` calls
3. `vim.fn` when no proper Lua API exists
4. Ex-command strings only as a last resort

Never use private `vim._*` APIs. Do not monkey-patch globals or Neovim APIs,
mutate `package.path` at runtime, or depend on deprecated APIs.

- Around 44 modules reference Neovim nowhere — the schema, workflow-definition,
  routing, permission, skills, and doc-generator layers. That is what lets them
  be tested as pure logic. Do not introduce the first `vim.*` call into such a
  module to replace a local helper: swapping a six-line `trim` for `vim.trim`
  buys nothing and costs the module its independence. Check with
  `grep -L 'local nvim = vim' <file>` before reaching for the stdlib in an
  unfamiliar file. Where a module already uses Neovim, prefer `vim.*` over a
  hand-rolled equivalent (`louiselm-stdlib-helper-cleanup-kw5m`).
- Never block Neovim's main loop with waits, polling, sleeps, or heavy work.
- Spawn processes with `vim.system()` and argument arrays, never shell-built
  command strings, `os.execute`, or `io.popen`.
- Pass cwd/environment explicitly; never interpolate untrusted command text.
- Treat callbacks from `vim.system()`, RPC/stdout handlers, libuv, and process
  exits as fast-event callbacks by default. They may validate or copy data, but
  must not call editor/UI APIs such as `nvim.api`, buffer/window operations, or
  notifications directly. Marshal editor/UI work to the main loop with
  `vim.schedule()` at the narrowest shared boundary, and test that boundary.
- Cancellation/disposal releases resources. Ignore late results for disposed
  sessions; they must not revive closed state.
- A completion callback or terminal event must fire at most once.

## 11. Security

- Validate all external input before changing state.
- Never log tokens, environments, prompts, tool payloads, or sensitive data by
  default.
- Do not execute generated Lua or use `load`, `loadstring`, the `debug` library,
  LuaJIT FFI, or global/package monkey-patching.
- Use `dofile` only for trusted project development files when a module cannot.

## 12. Editing and Completion Discipline

- Preserve existing user changes. Make the smallest root-cause change; avoid
  unrelated refactors.
- Revise existing files instead of creating versioned copies.
- Never run destructive Git or filesystem commands without explicit instruction.
- Do not stash user work to manufacture a clean test baseline.
- Update documentation and LuaCATS contracts with public behavior changes.

Before completing a code task:

- [ ] The new test failed first for the intended reason, when TDD applies.
- [ ] For Lua changes, focused tests, the full `mini.test` suite, and
      `stylua --check .` pass; `lua-language-server` reports zero diagnostics.
- [ ] For Rust changes, each affected crate's section 4 gates pass and its
      section 6 lint policy is checked; any existing adoption debt is recorded.
- [ ] Public APIs/failures are documented and typed; no error is swallowed or
      sensitive value logged.
- [ ] If `config.lua`'s schema changed, `./scripts/generate-luacats` was re-run
      and `lua/louiselm/types.lua` is part of this commit.
- [ ] If a public LuaCATS annotation changed, `./scripts/generate-api-appendix`
      was re-run and `doc/api.md` is part of this commit.
- [ ] Async UI/process tests exercise the production callback context; a
  synchronous fake alone is not evidence that fast-event boundaries are safe.
- [ ] No unnecessary dependency, compatibility shim, or abstraction was added.
- [ ] Any unavailable check is called out explicitly in the handoff.

If it passes tests but violates an invariant, it is still wrong. If it is
correct but needlessly complicated, simplify it before handoff.
