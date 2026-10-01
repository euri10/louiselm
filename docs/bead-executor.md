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
- `on_worker(result, continue)` receives the retained Session, assigned Bead,
  exact launch request, separate Run-envelope and launch-request digests, and
  any worker error. A finished worker also supplies durable `verification`
  status, ordered command observations and `verification_passed`.
  `continue(true)` sends passing work to the promotion preview when a Run
  worktree is configured. `continue(false)` rejects it.
  Keep the producer Session retained until the promotion decision completes;
  its authenticated broker worker serves the exact byte transfer.
  Known failed or rejected Beads receive one broker-mediated comment containing
  the Run ID, outcome category and worker turn/verification request IDs when
  available (falling back to broker Session IDs), then the next
  snapshot is prepared. `continue(nil, error)` stops the Run. Unknown or
  quarantined verification outcomes stop the Run regardless of the continuation;
  no outcome is retried automatically.

When `worktree` is supplied, the controller requires the snapshot's `base_commit`
to match the current committed HEAD; the broker also validates that field in
the digest-selected snapshot before staging. The dedicated checkout must be clean and on
`run/<run-id>`; its journal parent is private and outside the checkout. After a
passing verification and `on_worker` continuation, the operator CLI rechecks the
broker record, destination identity, clean tree and branch. `on_promotion`
receives the exact changed paths and approval digest; the supplied UI helper
shows them to the maintainer. Rejection makes no checkout change. Acceptance
reopens the same selection, rechecks the digest, applies through the existing
broker promotion protocol, and creates one LouiseLM Git commit with
`Refs <bead-id>`. A changed or uncertain operation stops the Run for inspection.
Only the new commit becomes the `expected_head` of the next Bead.

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

Workers use a private headless Session API with OpenAI Provider attribution and
native Skills. Their ACP permission policy automatically allows requests inside
the installed containment boundary. A request without a compatible approval
option is cancelled without opening a human prompt. The launcher registry selects
their command and environment; ordinary configured Agents retain their selected policy. No
Run ledger token or Provider credential is passed to a worker environment.

`dispose()` cancels active turns and closes each launcher's controller input.
The existing supervisor then owns its authenticated Park, loss settlement and
disposal sequence. A successful Lua return confirms local disposal; only a
terminal launcher receipt proves confined descendants were cleaned up. The
supervisor must stay alive to finish that proof. Late startup, authorization,
prompt or verification callbacks cannot start another worker after disposal.

The controller stages the selected inputs, Parks and exports each successful
worker, launches a distinct verifier Session, runs the broker's fixed plan and
reads durable status before the continuation. The broker checks the plan against
the Run envelope and spends verifier authority once. Promotion when configured,
failure recording and Run summary are part of this controller. This does not claim Verified
posture or complete the first live Run.

## Real VM operator entrypoint

`scripts/bead-run.lua` is the concrete caller for a reviewed first Run of three
to five Beads. In an interactive VM Neovim using the approved LouiseLM checkout,
invoke it with a private selection file:

```sh
nvim --noplugin -u NONE --cmd 'set runtimepath^=/approved/louiselm' \
  --cmd 'let g:louiselm_bead_run_selection="/private/selection.json"' \
  -c 'luafile /approved/louiselm/scripts/bead-run.lua'
```

The file must be operator-owned mode 0600 and at most 64KiB. Review it before
invocation: starting it approves the exact envelope and admits the real capture
ledger. No API key belongs in it. The closed `louiselm.operator.bead-run/1` object
contains:

- `envelope`: complete `RunEnvelope`, with a fresh UUID Run ID, exact ordered
  Bead scope, two Sessions per Bead, one failure comment per Bead, fixed plan
  digest, finite expiry and explicit finite shared Provider request cap. This
  first-Run caller permits only `gpt-5.6-luna`, with an at-most-high reasoning cap;
  the registered runtime must select the approved low-effort default.
- `beads`: exactly the same ordered IDs, as `{id, prompt}` records containing
  individually reviewed worker instructions.
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
The broker retains its credential and validates the full envelope and grants.
No Verified cutover or recovery exception is enabled: grants explicitly use
unattended conformance with no waiver and do not require cold recovery for this
first Contained Run. Deployment must retain its selected pre-cutover posture;
an enforced installation can refuse this caller without being weakened.

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

## Acceptance gates

The normal Lua suite covers list and grant refusal, sequencing, permission
policy, ledger admission, cancellation and fast-event callbacks. The installed
gate is required in the `sender-guard-vm` CI job:

```sh
for bead_case in 1 2 3 4 5; do
  ./scripts/launcher-vm exec sudo -n env \
    LOUISELM_REQUIRE_BEAD_EXECUTOR=1 LOUISELM_BEAD_EXECUTOR_CASE="$bead_case" \
    LOUISELM_TEST_LUA_ROOT=/home/vm \
    LOUISELM_TEST_BEADS_INSTALLER=/home/vm/scripts/install-broker-beads.py \
    LOUISELM_TEST_CAPTURE=/path/to/current/louiselm-capture \
    LOUISELM_TEST_NVIM=/var/tmp/louiselm-test-nvim/bin/nvim \
    timeout 120 unshare --mount --propagation private -- \
    /bin/bash -c 'umask 022; exec "$1" \
      launch_supervisor::system::installed_tests::daemon::bead_executor::privileged_installed_lua_bead_executor \
      --exact --nocapture' bash /path/to/current/louiselm_skills-test-executable
done
```

The guest must contain the current Rust build, `lua/`,
`tests/workflow/installed_beads.lua`, `tests/workflow/installed_operator.lua`,
the real capture CLI, stable Neovim and SQLite. The VM provisioner
includes SQLite; CI copies its stable Neovim distribution into the guest. The
distro Neovim is too old for the plugin's supported APIs. This gate uses the real
operator CLI, installed launcher, headless Lua Sessions and durable terminal
cleanup receipts. Operator inspection is live-only, so it cannot prove cleanup
after its worker disappears. The measured ACP peer and tracker are offline
fixtures; the gate runs a passing plan, a failing plan, an unapprovable
worker and accepted promotion into a dedicated Run worktree. No Provider requests are issued. Capture
admission/attachment is doubled in cases 1–4. Case 5 runs the actual operator
caller with the real capture CLI, three separately promoted commits, next-HEAD
snapshots and six durable terminal cleanup receipts. Its ACP peer and verification
plan remain offline fixtures, not live acceptance. Live Codex/Provider acceptance remains the
persistent-VM Run's responsibility.
