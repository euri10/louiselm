# Sequential contained Bead workers

`require("louiselm.workflow").new_bead_executor(options)` constructs the headless
controller for an exact ordered Bead list. `controller:start(callback)` approves
the operator-selected Run envelope through `louiselm-control run authorize --json`,
admits its capture-service ledger, and starts one Contained Session at a time.
It requires an installed launcher and broker, their registered Codex runtime,
and previously staged source/cache inputs. It does not install those components.
The trusted preparation callback can stage those inputs through the authenticated
`louiselm-control launch-inputs stage --json` command; its exact binding response
does not authorize a launch. Each snapshot must retain the supplied current HEAD.

The `envelope` is the complete closed `RunEnvelope` defined in
`skills-core/src/broker/run_envelope.rs`. Its Bead scope, Provider cap, fixed
verification-plan digest, expiry and revision must already reflect the operator's
selection. Calling `start` authorizes those exact bytes through the authenticated
operator CLI. Construct a new Run ID for each execution; restarting this Lua
object does not replay or renew approval.

The operator caller below persists an inactive draft instead. Its zero-expiry
envelope cannot authorize a Session. Opening or reconstructing that draft starts
no process and reserves no authority, budget or Session. Keep the private JSON
file until explicit Start or cancellation; no human-wait timer expires it.
The caller defaults to thirty minutes of execution from dispatch. An explicit
`execution = {kind = "stop_by", expires_at_ms = ...}` retains an absolute cutoff.
An elapsed cutoff refuses Start without creating authority.

For headless callers, `draft` is the closed `RunDraft` alongside its matching
inactive `envelope`. Start prepares the first current-HEAD snapshot and stages
its exact verification plan before requesting `kind = "dispatch"`. The broker
samples its clock after checks, fixes every capability expiry, and returns the
activated envelope and its separate approval digest. The Run ID is single-use,
including after restart or an uncertain reply. Retry, Park, resume and policy
reapproval cannot extend that deadline or reset spending. The existing explicit
absolute `envelope` API remains useful for separately approved headless callers.

`envelope.bead_scope.issue_ids` is the canonical, sorted, unique permission set.
The execution list (`bead_ids`, or `beads` in the operator caller) carries the
reviewed order independently. The operator caller requires exactly the same
members in both lists and preserves the selected execution order.

```lua
local controller = assert(require("louiselm.workflow").new_bead_executor({
  envelope = selected_envelope,
  bead_ids = selected_order,
  agent_id = registered_codex_id,
  prepare = prepare_next_snapshot,
  on_worker = function(result, continue)
    continue(result.verification_passed == true)
  end,
  worktree = { path = run_worktree, journal_parent = private_journal, head = initial_head },
  on_promotion = require("louiselm.ui.run_promotion").prompt,
}))
assert(controller:start(function(ok, err, summary)
  -- Present summary.text; transfer it to the host for a VM Beads copy.
end))
-- The owner calls controller:dispose() when releasing the Run.
```

The two callbacks are controller responsibilities:

- `prepare(bead_id, done, expected_head)` starts asynchronous snapshot preparation
  from the current `run/<run-id>` HEAD and returns
  `true`, or `false, error`. It calls `done({grant = grant, prompt = instruction,
  verification = inputs, base_commit = expected_head})`
  when the inputs are staged, or `done(nil, error)`. `grant` is the complete
  closed `GrantRequest` in `skills-core/src/broker/authorization.rs`. Its launch
  must name this Run, revision, envelope and Agent, with fresh Session,
  authorization and request IDs. Its Beads scope must use the worker role and
  exactly this one Bead. The broker validates the remaining authority fields.
  The Run's `max_sessions` must reserve two Sessions per selected Bead: its
  worker and a distinct verifier.
  `inputs` names a private baseline `snapshot`, its `snapshot_digest`, the exact
  `plan`, its `plan_digest`, and a distinct `verifier_grant` without ordinary
  command, Beads, Provider, dependency or skill grants. The plan digest must
  match the operator-approved Run envelope. The envelope's Beads mutation
  budget must reserve one failure comment per selected Bead.
- `on_worker(result, continue)` receives the producer handle, assigned Bead,
  exact launch request, separate Run-envelope and launch-request digests, and
  any worker error. A finished worker also supplies durable `verification`
  status, ordered command observations and `verification_passed`.
  `continue(true)` sends passing work to the promotion preview when a Run
  worktree is configured. `continue(false)` rejects it.
  With a Run worktree, passing verification first publishes the exact baseline,
  candidate, plan and evidence under the broker's private saved-result store.
  Only confirmed publication followed by authenticated launcher Disposal permits
  releasing the finished producer. Publication failure stops with its original
  work retained. Review then reads the saved copy without a live producer.
  Known failed or rejected Beads receive one broker-mediated comment containing
  the Run ID, outcome category and worker turn/verification request IDs when
  available (falling back to broker Session IDs), then the next
  snapshot is prepared. `continue(nil, error)` stops the Run. Unknown or
  quarantined verification outcomes stop the Run regardless of the continuation;
  no outcome is retried automatically.

A worker that cannot initialize is an infrastructure failure, not a failed Bead.
The Run stops before a successor, verifier, promotion or task-failure comment.
Its `summary.startup_failure` retains the Bead and broker Session identities plus
a normalized phase/code. The installed launcher emits a bounded
`louiselm.launch-failure/1` diagnostic prefixed by `LOUISELM_LAUNCH_FAILURE` on
stderr; the controller accepts only its closed fields, known categories and exact
Run/Session binding. Missing or invalid diagnostics become
`worker_startup/initialization_failed`; raw stderr is never copied to the summary.
This observation does not prove terminal cleanup or permit a new launch. Inspect
the consumed authorization and real receipts before any separately approved Run.

When `worktree` is supplied, the controller requires the snapshot's `base_commit`
to match the current committed HEAD; the broker also validates that field in
the digest-selected snapshot before staging. The dedicated checkout must be clean and on
`run/<run-id>`; its journal parent is private and outside the checkout. After a
passing verification and `on_worker` continuation, the operator CLI rechecks the
broker record, destination identity, clean tree and branch. `on_promotion`
receives the exact changed paths and stable approval digest, with no review
deadline. `a` accepts that exact preview; `r` explicitly discards the saved
result. Escape, closing the review and `q` leave saved work pending and stop this
controller without starting a successor. Acceptance reopens the saved selection,
rechecks the digest, current rules/toolchain, destination and policy, then uses a
fresh four-minute exact application transaction (the broker caps it at five
minutes and checks every effect). Preview transport is bounded and closes before
human review; it spends no application authority. Historical matching verification
is reused, not rerun. Changed inputs require explicit Refresh; uncertain prior
effects require inspection, not Refresh or replay. Application uses the existing
broker protocol and creates one LouiseLM Git commit with
`Refs <bead-id>`. A changed or uncertain operation stops the Run for inspection.
Only the new commit becomes the `expected_head` of the next Bead.
If original Run authority has expired, acceptance applies only this result and
`summary.continuation_required` marks remaining work for separate approval with
fresh limits. It never renews the old Run or refunds spent budgets.

Each published saved result also owns a durable Run-scoped `promotion_pending`
Attention item. It survives Session disposal, Run completion or expiry, editor
and receiver restarts, and notification acknowledgement. Only explicit Accept
or Discard resolves it. The broker reconciles interrupted delivery from durable
saved-decision metadata; retries preserve the same condition identity. Existing
enabled Attention channels receive only bounded Run/result identifiers and a
fixed label, never source, prompts, verification output or approval authority.
Neovim and Android show the result identifier and a local review hint; neither
display nor acknowledgement accepts a result or starts execution.
The callback's decision may be `nil, error_message` to stop review without
rejection. Its optional return is an idempotent UI cancellation function,
called on decision, completion or disposal without granting approval.

Optional `on_progress(progress)` receives payload-free updates on Neovim's main
loop: Run ID, Bead ID, one-based index, total and phase (`authorizing`,
`preparing`, `working`, `verifying`, `awaiting_promotion`, `promoting`). Queued
updates are suppressed after completion or disposal. The private-TTY operator
view displays the current Bead and phase, then accepted/rejected/failed counts.
Traversal ending with failed or rejected tasks is labelled accordingly;
`Run complete` means every traversed task was accepted. Closing the Run buffer
or editor disposes its controller. A Provider hold retains Sessions until the
operator explicitly stops it.

Promotion CLI failures return a `louiselm.run-promotion-error/1` JSON diagnostic
on stderr, containing a payload-free `message`. The controller displays that
specific refusal, distinguishing expiry from uncertain application or a Git
commit failure. Unstructured diagnostics are withheld; an unavailable outcome
requires inspection before any retry. A pre-application refusal creates no
promotion journal.

`louiselm-control promotion save --json` publishes a `selection` containing the
Run/Bead/Session/result IDs, checkout, private journal parent, exact expected HEAD
and operator-selected `plan` path, with no expiry. It confirms publication plus
producer Disposal. `promotion preview`, `inspect` and `discard` can reopen with
`{"result_id":"<saved-id>"}`; `commit` additionally requires the preview's
`approval_digest`. Review actions use the saved ID, not replacement context.
These operator-only actions need no live producer. Inspection includes the
durable application status and confirmed accepted commit. Duplicate confirmed
Accept returns that same commit without a new transaction; a lost/uncertain Git
commit acknowledgement requires inspection. Saved
baseline/result bytes and evidence are separate from seven-day Session storage;
editor/VM restart and ordinary Session cleanup do not remove unresolved work.
Inspection grants no application or execution permission. Discard is durable and
idempotent, applies nothing, and refuses a result with any application intent
until its effects have been inspected. Saved records retain historical evidence.

The completion callback receives `{ run_id, accepted, commits, branch, failed, text }` even when
the Run stops early. `failed` is ordered and includes each Bead ID, safe outcome,
  observation IDs, comment status, any returned broker operation ID and exact
  comment text.
`summary.text` is a host handoff: after a VM Run, fetch the named branch and use
the accepted Bead to commit mapping and failed comment text to update the host's
Beads copy by hand. It contains no worker output.
If the broker cannot confirm a failure comment, the controller stops and marks
that entry unconfirmed in the summary. Inspect the VM broker operation and
tracker before manually copying it; the controller never repeats the mutation
under a new identity.

All continuations run once on Neovim's main loop. Off-list Beads are rejected
before effects, and mismatched child receipts are refused before launch. The
broker independently restricts grants to its durable approval. The finite
workflow has one work and one verification stage per Bead. It shares the existing
Run owner and ledger; traversing this fixed list creates no generated-work charge.
Capture currently anchors a Run to its first initialized Session. The broker
retains each child's durable binding, and the Lua Run owns all live Sessions.

During a worker prompt and again when it finishes, the controller reads the
authenticated Provider hold under the exact Run approval. Polls are one second
apart with at most one outstanding poll, and stop on completion or disposal.
An exhausted or expired hold returns a `held`
controller and `summary.provider_hold`, Parks the local Run and stops progression.
It is a Run condition, not a failed Bead, and is never inferred from an Agent's
HTTP error text. The broker independently refuses every new child authorization,
including verifiers, while that hold remains active. Hold publication and child
authorization share the Provider ledger lock. A missing or mismatched status
response stops the controller without granting another child.

The operator buffer displays the hold and retains Sessions until `q` or editor
exit disposes them. No request is retried, budget extended or Session resumed
automatically; the existing explicit operator extension rules remain in force.
Terminal launcher receipts remain the proof of confined cleanup.

Workers use a private headless Session API with OpenAI Provider attribution and
native Skills. Their ACP permission policy automatically allows requests inside
the installed containment boundary. A request without a compatible approval
option is cancelled without opening a human prompt. The launcher registry selects
their command and environment; ordinary configured Agents retain their selected policy. No
Run ledger token or Provider credential is passed to a worker environment.

Worker and verifier startup has a 60-second deadline, including the installed
supervisor and ACP initialization. The supervisor allows 40 seconds for broker
authorization: it must retain source inputs and build the native Beads replica
before replying. Other launcher broker and Session lifecycle callbacks retain
their 30-second bound; the Beads mutation relay has 75 seconds to allow mutation
and replica refresh. Signing and platform operations keep their separate
five-second bounds. These waits do not extend the approved Run expiry or
Provider budget, and ordinary Sessions retain their existing startup timeout.

`dispose()` cancels active turns and closes each launcher's controller input.
The existing supervisor then owns its authenticated Park, loss settlement and
disposal sequence. A successful Lua return confirms local disposal; only a
terminal launcher receipt proves confined descendants were cleaned up. The
supervisor must stay alive to finish that proof. Late startup, authorization,
prompt or verification callbacks cannot start another worker after disposal.

The controller stages the selected inputs, Parks and exports each successful
worker, launches a distinct verifier Session, runs the broker's fixed plan and
reads durable status before the continuation. The broker checks the plan against
the Run envelope and spends verifier authority once. Each export or verifier-run
request gets a deadline capped at one hour from its construction and at the
approved Run expiry. A long Run does not widen the native operation budget;
elapsed preparation time does not consume a later operation's fresh budget,
and no issued request is renewed or retried. Promotion when configured,
failure recording and Run summary are part of this controller. This does not claim Verified
posture or complete the first live Run.

## Real VM operator entrypoint

First use the installed
[`workspace launch-inputs prepare`](../skills-core/README.md#inactive-run-preparation)
command to measure the explicitly selected source/cache and project instructions
against the current Supply and trusted registry. It writes an inactive proposal
and canonical manifest; it does not approve a Run, launch a Session or call a
Provider. Inspect the saved proposal with its exact manifest digest before
building the operator selection below. Copy `proposal.run_id` into
`envelope.run_id`, its proposed profile ID/revision into `envelope.envelope_id`
and `envelope.envelope_revision`, and the parsed `manifest.json` into `manifest`.
Use its `base_commit` for the dedicated worktree's initial `head`.
Add the reviewed ordered Beads, finite limits and fixed verification plan to the
inactive operator envelope, with every expiry set to zero. Select an execution
duration or an explicit stop-by cutoff; preparation grants neither.
The selected immutable cache must be privately readable by the operator and broker;
preparation does not change registry, group membership or access permissions.

Before each Run, recheck Bead readiness and have the operator freshly review the
exact ordered selection, fixed verification plan and finite limits, including the
Provider request cap, Session count and execution duration or stop-by cutoff. At
Start, current source, selected task readiness, runtime/policy, services, boot
and required matching rehearsal must be rechecked. Earlier selections, plans, limits
and approvals are historical records; they do not set defaults or authorize a
new Run. Obtain separate explicit operator approval for the reviewed Run before
invoking the caller. Provisioning code changes must be rebuilt into a reviewed
signed release before installation; a development binary or fixture pass cannot
stand in for that deployment.

### Required rehearsal before paid dispatch

Before **every** paid Run, require a passing rehearsal for the exact installed
signed release digest, current boot, source snapshot, ordered selection and
canonical tracker. A disposable-VM gate, an earlier boot or another release
cannot satisfy this requirement.

In the real guest, the operator runs the installed command:

```sh
/usr/local/lib/louiselm/current/bin/louiselm-control run rehearse \
  --selection /private/selection.json --plugin /approved/louiselm --json
```

The selection must be an operator-owned mode-0600 file, at most 64KiB. The
explicit plugin path is the trusted LouiseLM installation used by the operator,
never Lua from an untrusted candidate repository. The installed binary embeds
the coordinator. It uses the installed verifier Neovim, production launcher,
Control broker and capture CLI, and the selected stock runtime. It stages the
exact current clean Run HEAD and cache without changing the paid Run's inputs.

Rehearsal creates one independent UUID Run per worker, with the same identity
in the broker and capture ledger. Before granting these Runs any authority, the
broker durably denies all their Provider attempts before reservation or upstream
dispatch, and refuses its canonical Beads mutations. This denial survives restart.
No prompt is sent, no production TLS
trust override exists, and the paid Run is neither reserved nor activated.
Each selected worker starts through the real tracker-replica/workspace/ACP path,
attaches to a separate real capture record, then is disposed. The broker checks
each exact Session's reservation ledger and re-verifies its signed terminal
chain; process absence is not cleanup proof. Read-only input staging is reusable
by the subsequent paid Run.

Exit 0 and stdout `louiselm.run-rehearsal/1` with `status: passed` means all
selected workers initialized and attached, all consumed slots ended with verified
terminal chains, and every Session has zero Provider attempts. The broker keeps
the result privately. It binds the signed release, kernel boot, complete ordered
selection, exact staged source/cache/Agent/skills manifest, trusted plugin Lua
and canonical native tracker export/executable. A new attempt invalidates the
old pass before preparation. `failed` or `refused` is never a pass; normalized
phase/code is retained without raw stderr. Ordinary handled startup failures
still fail rehearsal but retain signed failed-launch terminal evidence. Missing,
unverified or nonterminal evidence reports `recovery_required`; no recovery or
retry is automatic. The separately reviewed exact saved-evidence delivery
operation may finish delivery without making new cleanup claims.

Before opening the paid draft and again on Start, the shipped first-Run script
remeasures the inputs and asks the broker to require the latest passing result,
including re-verification of every terminal chain and the zero-attempt ledger.
A changed boot, release, source, selection, plugin or tracker refuses dispatch.
A fixture result or handwritten JSON cannot supply that prerequisite. An older
release that lacks the command also refuses. Item 7 of `louiselm-a4r1d` remains
open until the maintainer runs this command in the actual guest; disposable
gates do not certify that guest or authorize a paid request.

To reopen one explicitly phone-published selection in a local waiting view,
choose its registration ID and current revision from the computer's
`louiselm-capture phone-control list-prepared` output:

```sh
nvim --noplugin -u NONE --cmd 'set runtimepath^=/approved/louiselm' \
  --cmd 'let g:louiselm_bead_run_registration="REGISTRATION_HEX"' \
  --cmd 'let g:louiselm_bead_run_revision=1' \
  -c 'luafile /approved/louiselm/scripts/bead-run-waiting.lua'
```

The authenticated local owner must be available and skills integration must
be installed. This opens the exact published revision without starting,
authorizing, allocating Sessions, or consuming a limit; closing the view
disposes the controller. The waiting view is currently local only: it does not
register a phone controller or offer Start. A later Start must independently
perform the normal readiness, rehearsal and broker checks. The controller
retains the exact registration revision and selection digest; if an authorized
caller later starts it, owner-bound Run accounting is admitted after broker
authorization and before the first Session attachment. Admission refuses a
changed or unpublished registration and never replaces those native checks.

`scripts/bead-run.lua` is the concrete caller for a reviewed first Run of three
to five Beads. In an interactive VM Neovim using the approved LouiseLM checkout,
invoke it with a private selection file:

```sh
nvim --noplugin -u NONE --cmd 'set runtimepath^=/approved/louiselm' \
  --cmd 'let g:louiselm_bead_run_selection="/private/selection.json"' \
  -c 'luafile /approved/louiselm/scripts/bead-run.lua'
```

The file must be operator-owned mode 0600 and at most 64KiB. Review it before
Start: it rechecks the exact rehearsal and current readiness, then activates the
reviewed limits at dispatch. A missing pass leaves the saved draft inactive.
No API key belongs in it. The closed `louiselm.operator.bead-run/2` object
contains:

- `envelope`: complete `RunEnvelope`, with a fresh UUID Run ID, exact sorted
  Bead scope, two Sessions per Bead, one failure comment per Bead, fixed plan
  digest and explicit finite shared Provider request cap. Set the envelope and
  all capability `expires_at_ms` fields to `0`, with revision `1`; these are
  placeholders, not authority. This
  first-Run caller permits only `gpt-5.6-luna`, with an at-most-high reasoning cap;
  the registered runtime must select the approved low-effort default.
- `execution`: optional `{kind: "duration", duration_ms: ...}`, defaulting to
  `1800000` milliseconds after checks at dispatch, or an explicitly reviewed
  `{kind: "stop_by", expires_at_ms: ...}` absolute Unix millisecond cutoff.
  Only one form is allowed. Changed reviewed terms require fresh review;
  inspecting, reopening or rehearsing a draft never activates or refreshes them.
- `beads`: exactly the same IDs in reviewed execution order, as `{id, prompt}`
  records containing individually reviewed worker instructions. This order need
  not be alphabetical; the envelope's permission set must remain sorted.
- `manifest`: complete resolved `SessionInputManifest` from trusted provisioning,
  naming the measured installed Agent, current Generation/view, policy and
  explicit input categories. Do not invent placeholder identities or treat a
  parsed record as provenance. Only source snapshot/base identities are refreshed;
  retain the selected Agent/supply/instruction configuration for the entire Run.
- `worktree`: `{path, journal_parent, head}` naming the dedicated clean
  `run/<run-id>` checkout, private journal directory and exact initial commit.
- `plan`: broker-readable fixed verification-plan file; its bytes must match the
  envelope's approved digest. `cache`: broker-readable immutable cache directory
  whose digest matches the manifest. No candidate installer is invoked.
- `snapshot_parent` and `input_group`: existing operator-owned mode-0750
  snapshot directory and its numeric trusted sharing GID. Trusted VM provisioning
  must make **only** the operator and broker members, never Session identities,
  and make all ancestors traversable by those accounts. This is not permission to
  share keys, Agent config or credential stores. The caller changes group/read
  permissions only on each newly created frozen source snapshot; immutable
  broker copies remain broker-owned mode 0700.

The installed signed launcher/broker/runtime, canonical broker Beads tracker,
trusted verifier tools, current supply and compatible `louiselm-capture` CLI are
prerequisites. The caller installs none of them and reads no credential store.
Skills-off preparation also requires a valid `louiselm-discovery.json` registered
in the runtime measurement, so the launcher can mask its native Skill roots.
Missing or invalid inventories refuse preparation before a proposal is published;
the canonical empty view alone does not establish native-source masking.
`scripts/fetch-stock-codex-chain` produces the pinned Codex inventory alongside
its four public runtime files. Register `louiselm-discovery.json` as a measured
adapter too; merely copying it into an already registered runtime is insufficient.
Its `louiselm.discovery.skill-roots/1` scope lists actual Skill roots and instruction
files, not fictitious filesystem slots for embedded schemas or disabled plugins.
It cannot establish full native-supply verification. The Codex integration always
sets `CODEX_HOME` to the private home: runtime state stays writable beside the
read-only `skills` mask. Nested roots such as `skills/.system` remain in signed
mask evidence but are hidden by the outer empty mount, not a second child mount.
This changes the measured runtime configuration; prepare fresh inputs and obtain
fresh authorization after installing the matching release. Do not retry consumed
Run authorizations or relabel their old manifests.
The Agent runtime must live outside the tool-visible system mounts (`/usr`,
`/bin`, `/sbin`, `/lib`, `/lib64`, `/etc/alternatives`), for example under
`/var/lib/louiselm-runtimes`. A runtime under `/usr/local/lib` is inside `/usr`
and cannot satisfy Agent/tool isolation.
The broker retains its credential and validates the full envelope and grants.
Grants use unattended conformance without a waiver and do not require cold
recovery. Brokered workers require an explicitly selected `enforced` installation
and valid current certification; `pre_cutover` produces Unevaluated evidence and
cannot admit Brokered startup. Preparation checks the installed policy before
staging snapshots or creating child grants. This check is not certification:
the launcher still validates the actual runtime, host and certificate. The
caller never changes policy, enables Verified posture or weakens a refusal.
Network-denied offline fixtures may retain `pre_cutover`; their success alone
does not establish that the deployed Brokered configuration can launch.

Each preparation captures the current accepted HEAD through the existing workspace
CLI, refuses unaccepted working-copy changes, shares that new snapshot privately,
and uses authenticated broker staging. Fresh worker and verifier identities are
derived from the fresh Run ID and Bead index. A previously confirmed identical
snapshot binding within this same Run may be reused after rejection; an uncertain
staging result is never resent. Construction owns no process; disposal kills
pending preparation and ignores late callbacks. Promotion remains the real
manual UI. `q` stops the Run; its buffer presents the payload-free host handoff.
Copy it before closing, fetch the named branch, and update the host tracker by
hand only after checking the accepted commits and any unconfirmed failure.

## Refresh a saved result

Changed destination HEAD, verification rules or qualified tools require an explicit
refresh, followed by a new preview and a separate Accept. Time alone does not.
Use `require("louiselm.workflow.saved_result").refresh(options, callback)` or set
`g:louiselm_saved_refresh_selection` to an operator-owned mode-0600 JSON file and
run `:luafile /path/to/trusted/plugin/scripts/bead-result-refresh.lua`.
The script asks before starting its one unpaid verifier and opens the new review;
closing that review leaves the result pending and never starts a successor.

`options.selection` is the closed Rust `RefreshSelection`: saved `result_id`,
current `request.verification_digest` as `revision_digest`, fresh `input_id`,
new frozen `snapshot`/`snapshot_digest`/`expected_head`, and current
`plan`/`plan_digest`. Supply current resolved `manifest`, immutable `cache`, a fresh
`session_id`, and an explicitly approved fresh `envelope` with one Session,
that exact plan, finite expiry and null Provider/Beads/command/skill capabilities.
Freeze the clean destination `run/<original-run-id>` using `workspace prepare`;
share only those inputs with the broker, never operator keys or journals.

Refresh applies exact changed paths in a separate copy, not a textual merge.
Any conflicting path refuses; no Agent repairs it. Failed/cancelled verification
keeps the original pending. Successful verification appends immutable private
revisions while preserving the original bytes and old/new verification/base
lineage. It does not extend the original Run or refill its budgets. Any prior
Accept application, including an uncertain one, must be inspected instead.
Installed case 14 covers expired-original refresh, verifier failure, changed
rules, destination movement, stale approvals and one explicit idempotent Accept.

## Acceptance gates

The normal Lua suite covers list and grant refusal, sequencing, permission
policy, ledger admission, cancellation and fast-event callbacks. The installed
gate is required in the `sender-guard-vm` CI job:

```sh
for bead_case in 1 2 3 4 5 6 7 8 9 14; do
  bead_timeout=$((bead_case == 14 ? 300 : 120))
  ./scripts/launcher-vm exec sudo -n env \
    LOUISELM_REQUIRE_BEAD_EXECUTOR=1 LOUISELM_BEAD_EXECUTOR_CASE="$bead_case" \
    LOUISELM_TEST_LUA_ROOT=/home/vm \
    LOUISELM_TEST_BEADS_INSTALLER=/home/vm/scripts/install-broker-beads.py \
    LOUISELM_TEST_CAPTURE=/path/to/current/louiselm-capture \
    LOUISELM_TEST_NVIM=/var/tmp/louiselm-test-nvim/bin/nvim \
    timeout "$bead_timeout" unshare --mount --propagation private -- \
    /bin/bash -c 'umask 022; exec "$1" \
      launch_supervisor::system::installed_tests::daemon::bead_executor::privileged_installed_lua_bead_executor \
      --exact --nocapture' bash /path/to/current/louiselm_skills-test-executable
done
./scripts/launcher-vm exec sudo -n env \
  LOUISELM_REQUIRE_BEAD_EXECUTOR=1 LOUISELM_BEAD_EXECUTOR_CASE=1 \
  LOUISELM_TEST_LUA_ROOT=/home/vm \
  LOUISELM_TEST_BEADS_INSTALLER=/home/vm/scripts/install-broker-beads.py \
  LOUISELM_TEST_NVIM=/var/tmp/louiselm-test-nvim/bin/nvim \
  timeout 120 unshare --mount --propagation private -- \
  /bin/bash -c 'umask 022; exec "$1" \
    launch_supervisor::system::installed_tests::daemon::bead_executor::privileged_installed_lua_bead_executor_failure_cleanup \
    --exact --nocapture' bash /path/to/current/louiselm_skills-test-executable

# Required combined stock-runtime gate; see provisioning below.
for stock_case in 11 12 13 10; do
  ./scripts/launcher-vm exec sudo -n env \
    LOUISELM_REQUIRE_BEAD_EXECUTOR=1 LOUISELM_BEAD_EXECUTOR_CASE="$stock_case" \
    LOUISELM_STOCK_CODEX_DIR=/var/tmp/louiselm-stock-chain \
    LOUISELM_TEST_BR=/var/tmp/louiselm-native-br \
    LOUISELM_TEST_BEADS_JSONL=/var/tmp/louiselm-controller-source/.beads/issues.jsonl \
    LOUISELM_TEST_WORKSPACE_SNAPSHOT=/var/tmp/louiselm-controller-snapshot \
    LOUISELM_TEST_LUA_ROOT=/home/vm \
    LOUISELM_TEST_BEADS_INSTALLER=/home/vm/scripts/install-broker-beads.py \
    LOUISELM_TEST_CAPTURE=/path/to/current/louiselm-capture \
    LOUISELM_TEST_NVIM=/var/tmp/louiselm-test-nvim/bin/nvim \
    timeout 360 unshare --mount --net --propagation private -- \
    /bin/bash -ec 'umask 022; /usr/sbin/ip link set lo up; exec "$1" \
      launch_supervisor::system::installed_tests::daemon::bead_executor::privileged_installed_lua_bead_executor \
      --exact --nocapture' bash /path/to/current/louiselm_skills-test-executable
done
```

The guest must contain the current Rust build, `lua/`, `scripts/bead-run.lua`,
`scripts/bead-rehearse.lua`,
`tests/workflow/installed_beads.lua`, `tests/workflow/installed_operator.lua`,
the real capture CLI, stable Neovim and SQLite. The VM provisioner
includes SQLite; CI copies its stable Neovim distribution into the guest. The
distro Neovim is too old for the plugin's supported APIs. This gate uses the real
operator CLI, installed launcher, headless Lua Sessions and durable terminal
cleanup receipts. Operator inspection is live-only, so it cannot prove cleanup
after its worker disappears. The measured ACP peer and tracker are offline
fixtures in cases 1–9; those cases run a passing plan, a failing plan, an unapprovable
worker and accepted promotion into a dedicated Run worktree without Provider requests. Capture
admission/attachment is doubled in cases 1–4. Case 5 runs the actual operator
caller with the real capture CLI, a nonalphabetical reviewed execution order
against the exact sorted permission set, three separately promoted commits,
next-HEAD snapshots and six durable terminal cleanup receipts. Its ACP peer and verification
plan remain offline fixtures, not live acceptance. Live Codex/Provider acceptance remains the
persistent-VM Run's responsibility.
The separate failure-cleanup case interrupts case 1 during its tracker import,
then checks that the original failure survives, the broker is reaped, the
tracker exits and the fixture account is removed. It also forces a secondary
account-removal error during unwinding and checks that the original panic survives.
Cases 6–7 inject a captured durable Provider hold after worker completion and
after a clear status check, respectively. They use the real operator exchange,
child authorization, Lua controller and terminal cleanup, proving that neither
a successor worker nor verifier receives authority. The hold is an explicit
offline spending double; these cases make no Provider calls.
Case 9 removes only the private fixture's staged source before startup. It
requires the real launcher's matching `workspace_prepare/unavailable` diagnostic
in the stopped Lua Run, one consumed authorization and no successor, verifier
or task-failure comment. The original supervisor must persist an authenticated
terminal failed-launch receipt without successful preparation or Agent
measurements, then release its local lease. The wrapper still exits with failure.
For source-sized stock-runtime diagnosis, the installed workspace fixture accepts
`LOUISELM_TEST_WORKSPACE_SNAPSHOT` naming a native frozen snapshot; it copies and
validates every included file and binds the seven project instruction documents.
Unset retains the ordinary empty fixture. This diagnostic variant does not by
itself establish the complete operator/capture/native-tracker composition.
The guarded stock-runtime fixture also accepts `LOUISELM_TEST_BR` naming the
qualified native tracker. It then provisions the canonical tracker in private
mounts and exercises native replica export/import before launch, with the
production launcher's forty-second authorization deadline. Optionally,
`LOUISELM_TEST_BEADS_JSONL` imports a LouiseLM tracker export through native `br`,
and `LOUISELM_TEST_AGENT_GID_START` selects a separate Agent GID range. These
diagnostic inputs do not change the default fixture or authorize a paid Run.
Case 5 also accepts the source snapshot: its checkout must have exactly the
selected source-base digest before the operator starts. CI retains the empty
case and additionally runs both stock startup and the real operator/capture
case against a snapshot of the tracked repository. Capture uses the installed
`/usr/local/lib/louiselm-tools` layout, not the release's `bin` directory.
The source-sized operator case has a bounded three-minute aggregate wait and
five-minute fixture approval, based on measured complete three-task cycles;
all production per-operation deadlines and spending caps remain unchanged.
Cases 10–13 require the combined stock/native/operator configuration; missing
inputs fail rather than substitute a double. Provision the four artifacts and
discovery inventory with `scripts/fetch-stock-codex-chain`, then verify them
offline with `scripts/fetch-stock-codex-chain --check DIRECTORY`. Their hashes
and immutable modes must match Node 24.16.0, Codex/Code Mode 0.156.1 and
codex-acp 1.10.0. CI builds native `br` at
`5830c1e49ae0f38b67fc50382c8481f1ff46a7cf` and imports the tracked full export
into private broker custody. These cases require a full source snapshot, Skills
off, Brokered networking and an Enforced installation with a passing real
certificate, never a waiver or PreCutover evidence.

Case 11 first runs the production rehearsal CLI for all three selected workers:
three real capture attachments, three verified terminal chains, zero Provider
reservations, same-input pass readback and changed-selection refusal. It then
runs the unchanged production daemon through a fourth stock ACP initialization,
Running receipt and real capture attachment, disposing before a prompt.
Cases 12 and 13 inject replica-import and pre-ACP workspace failures through a
fixture-only wrapper around native Beads. They characterize the Nov 16 and
Nov 23 failure classes, not the lost historical cause: the first refuses before
consumption, the second retains the original `workspace_prepare/unavailable`
failure with a verified terminal failed-launch chain. Both refuse paid dispatch.
Case 10 compiles the actual daemon dispatcher into the library test;
only this test process supplies the private TLS CA through the existing test-root
setter. No installed CLI flag, production trust override or real Provider
credential is added. A pristine production startup establishes broker state identity; the
dummy credential is provisioned between daemons, before the serving daemon loads
custody. The private network namespace permits only loopback. A local TLS peer
elicits three real Code Mode file edits and exactly six requests, with one unused
unit of fixture budget so exhaustion does not deliberately block the last
verifier. All three promotions use the actual operator approval callback and
current-HEAD snapshots. The gate cryptographically verifies all six terminal
chains, Certified conformance, runtime/source bindings and the durable spent
count, then checks guard cleanup. Its aggregate Lua wait is 270 seconds, inside
the five-minute fixture approval; production deadlines are unchanged. These
offline checks do not identify the lost live failure or close paid-Run acceptance.
Cases 4 and 8 remove both disposed Session trees with the real retention
collector, reopen by saved ID, refuse changed rule bytes before application and
reuse the unchanged evidence after restoration. Case 8 waits beyond the original
Run's real deadline before explicit Accept. Both create one reviewed commit;
double Accept returns that commit with one application journal. Case 8 starts no
successor and reports that remaining work needs fresh approval. These are
offline delayed-acceptance checks, not recovery of the lost original live refusal
or maintainer acceptance.
