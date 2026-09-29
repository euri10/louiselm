# Sequential contained Bead workers

`require("louiselm.workflow").new_bead_executor(options)` constructs the headless
controller for an exact ordered Bead list. `controller:start(callback)` approves
the operator-selected Run envelope through `louiselm-control run authorize --json`,
admits its capture-service ledger, and starts one Contained Session at a time.
It requires an installed launcher and broker, their registered Codex runtime,
and previously staged source/cache inputs. It does not install those components.

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
  on_worker = verify_then_preview,
}))
assert(controller:start(function(ok, err)
  -- Present completion or the explicit failure to the operator.
end))
-- The owner calls controller:dispose() when releasing the Run.
```

The two callbacks are controller responsibilities:

- `prepare(bead_id, done)` starts asynchronous snapshot preparation and returns
  `true`, or `false, error`. It calls `done({grant = grant, prompt = instruction})`
  when the inputs are staged, or `done(nil, error)`. `grant` is the complete
  closed `GrantRequest` in `skills-core/src/broker/authorization.rs`. Its launch
  must name this Run, revision, envelope and Agent, with fresh Session,
  authorization and request IDs. Its Beads scope must use the worker role and
  exactly this one Bead. The broker validates the remaining authority fields.
- `on_worker(result, continue)` receives the retained Session, assigned Bead,
  exact launch request, separate Run-envelope and launch-request digests, and
  any worker error. Verification and promotion consume this handoff. Only
  `continue(true)` prepares the next snapshot, so it can include the previously
  accepted change. `continue(false, error)` stops execution. Worker failures are
  not retried; the verifier/failure controller decides whether to continue.

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

This API ends at verification handoff. The fixed-plan verifier
(`louiselm-u06e2`), promotion, failure recording and Run summary are separate
work. It does not claim Verified posture or complete the first live Run.

## Acceptance

The normal Lua suite covers list and grant refusal, sequencing, permission
policy, ledger admission, cancellation and fast-event callbacks. The installed
gate is required in the `sender-guard-vm` CI job:

```sh
./scripts/launcher-vm exec sudo -n env \
  LOUISELM_REQUIRE_BEAD_EXECUTOR=1 LOUISELM_TEST_LUA_ROOT=/home/vm \
  LOUISELM_TEST_BEADS_INSTALLER=/home/vm/scripts/install-broker-beads.py \
  LOUISELM_TEST_NVIM=/var/tmp/louiselm-test-nvim/bin/nvim \
  timeout 120 unshare --mount --propagation private -- \
  /bin/bash -c 'umask 022; exec "$1" \
    launch_supervisor::system::installed_tests::daemon::bead_executor::privileged_installed_lua_bead_executor \
    --exact --nocapture' bash /path/to/current/louiselm_skills-test-executable
```

The guest must contain the current Rust build, `lua/`,
`tests/workflow/installed_beads.lua`, stable Neovim and SQLite. The VM provisioner
includes SQLite; CI copies its stable Neovim distribution into the guest. The
distro Neovim is too old for the plugin's supported APIs. This gate uses the real
operator CLI, installed launcher, headless Lua Sessions and durable terminal
cleanup receipts. Operator inspection is live-only, so it cannot prove cleanup
after its worker disappears. The measured ACP peer and tracker are offline
fixtures; no Provider requests or tracker mutations are issued. Capture
admission/attachment is doubled, with separate real capture-service integration
coverage in the normal suite. Live Codex/Provider acceptance remains the
persistent-VM Run's responsibility.
