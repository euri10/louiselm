# Foreground no-Provider conformance QA

This is the bounded desktop acceptance workflow for `louiselm-d6fv.12.5.4`,
not the first Bead Run, production supply selection or Verified cutover.
Use the maintainer's private foreground terminal for existing-key signing and
secret/hardware interaction. An Agent may prepare unsigned artifacts and run
the disposable-VM gates; it must not perform the private ceremony.

## Review before privileged work

Finish the affected suites and installed composition gates, commit the selected
source, then build **two fresh clean releases** with the existing `release build`
owner. Review their exact manifests, source commit, component hashes and locked
toolchain. Both must include `bin/louiselm-tool-test-agent`; an older five-component
bundle cannot provide this workflow. Do not sign a dirty tree or reuse the old
October 9 preparation. Keep both releases, their manifests and exact restore
bytes private. Signing/installing either is a separate reviewed action.

The runtime proposal producer can consume an unsigned bundle without claiming
it is signed or installed:

```sh
louiselm-skills workspace qa-runtime prepare \
  --bundle /private/reviewed-bundle-one --output /private/new-runtime-proposal \
  --robot-json
```

Review `proposal.json` and its exact file SHA256. It binds the actual Agent bytes,
the fixed empty-skill discovery inventory and one release-derived runtime ID.
It does not register an Agent, trust a key, authorize a Run or launch anything.
The only root metadata publication command is separate and explicit:

```sh
sudo python3 scripts/install-qa-runtime.py /private/new-runtime-proposal \
  --reviewed sha256:EXACT_PROPOSAL_FILE_HASH
```

Replace all placeholders with reviewed absolute paths and hashes. The installed
operator must invoke this command; it accepts no registry/runtime path overrides.
All source ancestors must be root/operator-owned and non-writable by other users.
It remeasures the proposal, executable and inventory, preserves existing entries,
and appends only its fixed QA runtime, network-denied envelope and Agent. The Agent
is published last. Existing conflicting entries or changed files refuse. Retain
partial QA-only registration after failure; do not delete/repair it automatically.
Serialize this operation with all other registry maintenance. Publication is
**registered, not authorized**; the installed launch owner must still bind the
runtime's executable to a trusted release component.

## Real inputs and inactive selection

Use `workspace prepare` for an actual committed source snapshot. Select an explicit
empty private cache directory and measure it through the real cache-base owner:

```sh
louiselm-skills workspace qa-runtime measure-cache \
  --cache /private/new-empty-qa-cache --robot-json
```

Bind the returned `cache_digest`, not a manually constructed cache manifest. Follow
[inactive input preparation](../skills-core/README.md#inactive-run-preparation)
with the proposal's actual `agent_id` and `envelope_id`, **`--skills off`**, an
existing empty-capable store and the root registry. Keep the complete project
instruction-path array; skills-off must not erase source instructions. This
does not enroll skill supply or promote a permanently untrusted signing store.
Arrange only the input tree's required operator/broker readability; do not make
private signing keys or unrelated home directories readable.

After the two releases and input tree are reviewed, prepare shortly before the
window, as the one-hour absolute expiry starts at preparation:

```sh
python3 scripts/conformance-qa.py prepare \
  --input /private/owner-produced-inputs --digest sha256:EXACT_MANIFEST \
  --releases sha256:FRESH_RELEASE_ONE sha256:FRESH_RELEASE_TWO \
  --output /private/new-qa-round
```

The owner rechecks the input tree. `selection.json` and `plan.json` are private
inactive records, not authority. Review the printed exact selection digest,
fresh Run ID, all twelve Session/request/authorization IDs and expiry. Its closed
Run envelope denies Provider and Beads families; each child also denies
dependency/skill/Provider/Beads permissions. The only command is exactly
`printf QA_OK`, two uses per Session, five seconds, no delegation. No arbitrary
shell, Agent prompt, dependency or write command is accepted.

## Explicit exclusive window and foreground control

No other Contained Sessions may be active or Parked. Review exact privileged
commands and current installation/policy identities before either release is
installed or policy is changed. Preserve ordinary Agent YOLO/approval choices.
The existing install refresh preserves `pre_cutover`; it is **not** an enforcement
switch. The coordinator never changes policy, installs/signs a release or
restores policy. Inspect the installed public owner and review its exact
`release_id`, `operator_uid`, `conformance` and canonical `policy_digest`:

```sh
louiselm-skills workspace qa-runtime inspect-installation --robot-json
sudo /usr/local/lib/louiselm/current/bin/louiselm-skills launcher qa-policy \
  --reviewed-policy sha256:EXACT_CURRENT_POLICY --to enforced \
  --exclusive-no-contained-sessions --robot-json
```

Only the installed operator's explicit private sudo command can change policy.
It holds the installation lock and **every** idle identity lock through the
conformance-only private/public publication; occupied or poisoned identities,
stale/foreign/no-op reviews and mismatched authority refuse. A preconstructed
launcher rechecks governing policy after acquiring its lease, closing the
empty-check/new-launch race. It changes no approval choices, keys, pool identity
or certificate/history. A partial write is an error: retain both files and inspect
the mismatch; never infer success or automatically repeat/restore. Serialize
this window operation with other administrator maintenance.

An enforced policy needs a new current-boot fixed certification, not the old
pre-cutover certificate. Run the already-reviewed fixed `louiselm-launch certify`
operation in the private foreground terminal while observing responsiveness.

Once that separately reviewed window is established:

```sh
python3 scripts/conformance-qa.py run /private/new-qa-round/selection.json \
  --reviewed sha256:EXACT_SELECTION --exclusive-enforced-window
```

Foreground verbs are `start`, `status`, `command`, `park`, `resume`, `dispose`,
`quit`, one per line. `start` checks the public authority with the existing
installed owner (`workspace qa-runtime inspect-installation --robot-json`),
authorizes the exact parent once, stages the immutable inputs once, authorizes
one child and starts the installed launcher. It consumes one of twelve planned
authorizations **before** effects. At most one active/Parked identity is retained.
`status` uses authenticated operator reads, checks report bytes against immutable
admission and saves bounded private typed evidence. It does not renew anything.
`resume` explicitly reconnects the deterministic peer only after authenticated
Resume succeeds; it cannot replenish command uses or expiry.

Dispose before starting another Session. A signed exact terminal Disposal receipt
and process exit must be confirmed before the coordinator releases the slot.
EOF, timeout, transport loss, checker refusal or a lost reply retains the journal
and spending; there is no automatic replay, renewal, refund or cleanup claim.
After inspection, explicit foreground recovery is:

```sh
python3 scripts/conformance-qa.py resolve-disposal \
  /private/new-qa-round/selection.json --reviewed sha256:EXACT_SELECTION \
  --exclusive-enforced-window
```

This repeats only an exact retained Disposal request, or requests a new explicit
Disposal from authenticated settled state. It never repeats commands, Start,
Park or Resume. It requires a bound terminal receipt and settled terminal status
before clearing the active slot. Unavailable ownership/cleanup remains blocked.
Keep the original selection/journal; deleting them is not recovery.

## Human cases and truthful limits

### Explicit retired-QA-release fault and restoration

This operation is separate from the coordinator. After admitting a Session on
the first **new QA release**, install/refresh the reviewed second release through
the existing owners, keeping it default. Do not use the old desktop release as
the fault target. In the private foreground terminal, review a read-only plan:

```sh
sudo /usr/local/lib/louiselm/current/bin/louiselm-skills launcher qa-fault plan \
  --baseline sha256:ORIGINAL_DESKTOP_RELEASE --retired sha256:FIRST_NEW_QA_RELEASE \
  --run EXACT_RUN_ID --session EXACT_ACTIVE_SESSION_ID --exclusive-qa-window --robot-json
```

The plan names the root-registered Session/Run pin, current default, baseline,
exact retained launcher hash, size, inode/device and original mode. It requires
two distinct newer clean-source releases containing the test Agent, the actual
consumed offline QA grant and unexpired fixed deny-family Run approval, and the
registered deterministic Agent/network-denied envelope. It refuses default,
baseline, unknown, unbound, renewed, symlinked, hardlinked or changed targets.
It does not approve a Run, sign, install, suspend or resume anything.

Review the complete plan and its printed `reviewed` digest. Apply is a second,
explicit command with the **same** baseline/retired/Run/Session arguments:

```sh
sudo /usr/local/lib/louiselm/current/bin/louiselm-skills launcher qa-fault apply \
  --baseline sha256:ORIGINAL_DESKTOP_RELEASE --retired sha256:FIRST_NEW_QA_RELEASE \
  --run EXACT_RUN_ID --session EXACT_ACTIVE_SESSION_ID \
  --reviewed sha256:EXACT_FAULT_PLAN --exclusive-qa-window --robot-json
```

It rechecks the real owners under the installation lock, creates/fsyncs a
root-private recovery record **before** changing only that launcher's mode from
0555 to 0444, and never alters bytes, pin/history, keys, kernel controls or the
default. Serialize all administrator release/registry maintenance during this
operation. Duplicate/uncertain apply refuses; retain the record and exact bytes.
Observe the actual validity suspension and disabled effects at the keyboard.

Restoration is also explicit and requires the original plan digest:

```sh
sudo /usr/local/lib/louiselm/current/bin/louiselm-skills launcher qa-fault restore \
  --reviewed sha256:EXACT_FAULT_PLAN --exclusive-qa-window --robot-json
```

It consumes only its protected recovery record, requires the same retained bytes,
inode and unchanged default/binding, restores exactly 0555, remeasures the pin and
retains the record/history. It may restore after expiry or disposal, but cannot
renew Session authority. An uncertain already-restored 0555 target is checked
idempotently; a substituted target refuses. Restore and subsequent fixed
recertification do **not** thaw a Session: use explicit authenticated Resume or
truthful refusal plus terminal Disposal. Never erase the journal or bypass a
failed restoration. No automatic fault or restore is performed.

Follow the active `QA_REVIEW.md` round at the keyboard. Capture actual process
suspension, disabled effects, original last-success time and current cause,
recertification without implicit thaw, explicit Resume/Dispose and responsiveness.
Typed status/certificate success alone is not a human observation. Foreign-ID,
hostile and race probes belong to the disposable VM, not the desktop.

Release continuity uses only the two new reviewed releases. The newer remains
default; only the new retired QA release may be the reversible invalidation
target. Preserve its exact bytes and retained history; never mutate old desktop
releases, system controls, keys, kernel settings or unrelated records. If an exact
safe fault/restore operation has not been provided and reviewed, leave that case
unaccepted. This coordinator provides no hidden fault-injection operation.

After **every** QA Session is terminal, inspect/review the current policy again
and explicitly restore it:

```sh
sudo /usr/local/lib/louiselm/current/bin/louiselm-skills launcher qa-policy \
  --reviewed-policy sha256:EXACT_ENFORCED_POLICY --to pre_cutover \
  --exclusive-no-contained-sessions --robot-json
louiselm-skills workspace qa-runtime inspect-installation --robot-json
```

Compare the restored governing configuration with the recorded baseline for the
installed fresh default, allowing no change except conformance. Interrupted
foreground control promises no unattended restoration. Keep the round open until
the maintainer confirms the exact reproductions and restoration. Completed code
gates do not close desktop acceptance.

## Gates

`scripts/test-conformance-qa.py` checks closed bounds, durable spending/uncertainty,
expiry/count retention and exact lifecycle settlement; publisher tests check
measured-source refusals, linked-file rejection and preservation/conflicts.
The mandatory VM/CI gate is
`launch_supervisor::system::installed_tests::daemon::qa_entrypoint::privileged_installed_no_provider_qa_entrypoint`:
actual runtime/input owners, root QA metadata publication, reviewed exclusive
policy transition/restoration (occupied/stale/cached-launch refusals and old
certificate invalidation), authenticated approval, installed launcher, command,
Park/Resume, two Sessions and terminal disposal.
It runs with `LOUISELM_REQUIRE_QA_ENTRYPOINT=1`, `LOUISELM_TEST_LUA_ROOT=/home/vm`,
private mount/network namespaces and root **only inside the disposable guest**.
It installs two real software-key fixture releases through the release and
launcher owners, replaces the default while retaining the first Session's pin,
then consumes the reviewed fault/restore commands. It checks suspension,
immutable admission/history/bytes, root-private recovery retention, targeting and
symlink/stale/duplicate refusals, no implicit thaw after restore/recertification,
explicit Resume with unchanged spending, and terminal cleanup. The toy source
identity and disposable software keys are explicit fixtures, not fresh clean
builds, hardware signing or desktop acceptance.
