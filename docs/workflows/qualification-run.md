# Explicit paired qualification trials

`scripts/qualification-run.py plan` prepares the fixed verification plan.
`run` explicitly approves one selected Run and starts separate workers and
fixed verifiers under that envelope. Workers retain Brokered access; verifiers
have denied network and no Provider authority. Both verifiers must initialize
before either worker starts. Unsupported offline initialization refuses the trial
without a worker prompt. This implements the confirmed design in louiselm-70c20.

The installed offline fixture exercises this composition with a measured
synthetic ACP peer, not a vendor Agent. No paid comparison or production route
is approved by these tests.

First curate and preview a `louiselm.qualification-manifest/v1` bundle. The
committed example is synthetic: its placeholder Agents/Providers cannot run.
The runner targets one installed OpenAI Agent advertising select
options named `model` and `reasoning_effort`, with distinct baseline/candidate
pairs and effort at most `high`. Other Providers, option shapes, Auto/classifier
routes and missing installed authority are explicit refusals.

```sh
python3 scripts/qualification-preview.py preview /absolute/bundle/manifest.json
python3 scripts/qualification-run.py plan /absolute/bundle/manifest.json
```

`plan` validates the bundle and prints the fixed verification plan and its exact
digest. It runs no Model or command. Review both the selected-file disclosure
and those argv arrays before preparing the Run authority; fixture code is not
executed during preparation. Preparing a plan is not authorization to run it.

## Operator selection

The JSON selection must be an operator-owned regular file, mode `0600`, at most
64 KiB, with exactly these fields:

| Field | Required value |
| --- | --- |
| `schema` | `louiselm.operator.qualification-run/1` |
| `manifest` | Absolute path to the reviewed curated bundle manifest |
| `policy_revision` | Exact comparison policy revision |
| `workload` | `{"kind":"main","id":"<chosen workload>"}` |
| `retention_days` | `7`, acknowledging the installed workspace retention |
| `envelope` | Complete resolved `louiselm.broker.run-envelope/1` record |
| `input_manifest` | Complete resolved `louiselm.session.input-manifest/1` record |
| `cache` | Absolute path to the trusted immutable cache |
| `snapshot_parent` | Absolute operator-owned directory, mode `0750`, owned by `input_group` |
| `input_group` | Sharing GID restricted to the operator and broker |

Resolve runtime, Generation/view, policy and cache identities through the
installed workflow; do not invent digests or reuse an unmeasured runtime.
Preparation replaces only the source snapshot/base identities after hashing
the selected bytes. No reusable Provider credential belongs in this selection.

The envelope names a fresh Run, the exact generated verification-plan digest,
exactly four fresh Sessions, both selected Models, the applicable effort ceiling,
and the manifest's whole-Run request cap. Its Provider expiry equals the Run
expiry, which must fit within `limits.elapsed_seconds`. Command authority is
absent. The current Run schema also requires a valid Beads scope; the trial
never passes it to a child, requests a tracker mutation, or discloses a tracker.
Worker children receive only their own selected Model/effort under the shared
Run cap. Verifiers explicitly receive the `fixed_verifier` role and no Provider,
tracker, dependency, Skill-request, arbitrary-command or cold-recovery grant.
Their role is persisted by the broker and signed into launch evidence. Missing
Provider authority alone never turns an ordinary Agent into a verifier.

The installed launcher/broker, current conformance and supported measured Agent
integration must admit the launch. The runner never installs privileged software
or converts a refusal into an ordinary Session.

After reviewing the selection, the operator authorizes and executes it with:

```sh
python3 scripts/qualification-run.py run /absolute/selection.json --output /private/new-result
```

This approval binds the exact plan digest, four-launch ceiling, shared request
cap and expiry upfront; it is not a prompt for each verifier command.

## Execution and artifacts

Preparation revalidates selected hashes and freezes only selected input,
instruction and source files. Raw Git plumbing avoids developer configuration,
hooks, signing and attribute filters. Each arm starts from the same immutable
source/cache in a separate private Session; arm changes never reach the bundle,
operator checkout, tracker or the other arm. The selected prompts run in manifest
order. Every prompt requires the acknowledged exact Model/options pair.

Both verifier Sessions are preflighted once under denied network, with no Model
prompt or configuration requests. After an arm, the worker is Parked and its
result is frozen. Its already-running, distinct verifier executes only the
preselected command plan against that export. A passing command
requires the matching request/job and proven verifier cleanup. Lost replies read
the original durable status, never repeat command execution.

The broker counts actual upstream attempts across both workers. Prompt count is
not that budget. Selected input bytes, observed output text bytes and elapsed
time are additional local ceilings, not token, billing or remote spend guarantees.
Prices, quota and actual request totals remain unknown when not available from
supported runtime evidence. The runner never reads another tool's credentials
or queries a Provider for Account limits.

The new `0700` result directory receives `0600` `report.json` and
`observations.json` before launching. They contain check results, declared and
confirmed option tuples, snapshot identity, Session/turn/verification IDs and
measurement coverage, not prompts, Model answers or command output. A refusal,
timeout or crash leaves pending/unknown results; it never invents a success.
Human-review checks remain pending. Feed this report to the explicit comparison
decision workflow only after the required human judgment; execution itself is
not production approval.

Local cancellation closes owned Sessions and pending Control processes. Inspect
the named Sessions' terminal launcher receipts before treating remote cleanup
as proved. Private preparation files are removed after exit; installed worker and
verifier artifacts containing selected source remain under the acknowledged
seven-day retention policy. Choose a disclosure scope that permits that retention.

## Gates and acceptance boundary

```sh
python3 scripts/test-qualification-preview.py
python3 scripts/test-qualification-run.py
nvim --headless --noplugin -u ./tests/minimal_init.lua \
  -c "lua MiniTest.run_file('tests/routing/trial_spec.lua')" -c 'qa!'
```

CI requires both installed gates in a disposable VM with private mount/network
namespaces. `privileged_activated_brokered_guard_start` initializes two denied-
network verifiers and two guarded workers; its peer advertises no Model options,
so no prompt runs and quality checks remain pending.
`privileged_installed_lua_qualification` uses explicitly selected synthetic ACP
options, checks six turn identities, runs both frozen jobs, probes source-snapshot
and ambient-environment isolation, and verifies four distinct identities and
their signed terminal chains. Human judgment and unavailable usage remain pending.
The installed exact-job gate additionally checks tampering, one-use execution,
cancellation and independent cleanup; the Run-budget gate proves shared actual
upstream counting. None certifies vendor offline initialization, a paid comparison
or the maintainer's desktop deployment.
