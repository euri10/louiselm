# LouiseLM skills core

`louiselm-skills` is the trusted tool that turns an untrusted Skill candidate
into an immutable, content-addressed package, inspects every byte of it
deterministically, and renders the canonical Dossier a reviewer approves. It is
the only writer of immutable packages.

It decides nothing about whether a skill is safe. It decides what a reviewer is
shown, and it guarantees that the bytes shown are the bytes that were captured.

## Building the Sender guard

Building this crate requires Linux x86_64, Clang with the BPF backend, and
Linux UAPI development headers (`clang` and `linux-libc-dev` on Debian/Ubuntu).
`build.rs` compiles `src/launch_supervisor/sender_guard/lifecycle.bpf.c` and its
shared binding source into a little-endian BPF object. Cargo tracks both the
source directory and the fixed UAPI include trees. Compiler failures fail the
build; an earlier object cannot satisfy a failed rebuild.

`launch_supervisor::SENDER_GUARD_OBJECT` contains the bytes. The launcher embeds
them and its internal, unprivileged `__sender-guard-object` inspection verb
writes only that object to stdout, without accepting a path or loading it.
The signed launcher measurement therefore covers the guard too. No compiled
object is committed or installed separately.

`python3 scripts/test-sender-guard-build.py` (from the repository root) checks
rebuilds and compiler errors. The artifact gate is
`python3 scripts/test-sender-guard.py skills-core/target/debug/louiselm-launch`;
it checks the actual embedded object, including the six program/seven map
inventory and BTF data. The privileged
[VM gate](../docs/launcher-vm.md#embedded-sender-guard) consumes those same bytes.
The production loader and activation remain separate tasks; `Brokered` still
refuses, and these component checks do not confer Verified posture.

## Commands

```text
package <candidate-dir> [--captured-at MS]
verify <digest>
inspect <digest>
dossier <digest> [--against DIGEST] [--review-depth DEPTH]
                 [--assessment-model M --assessment-prompt P]
list
policy [--digest]

preflight --request FILE [--manifest FILE]
          [--previous-request FILE --previous-manifest FILE]
          [--store DIR] [--registry DIR] [--robot-json]
preflight --direct [--robot-json]

workspace prepare --repository DIR --output NEW_DIR [--include FILE ...] [--robot-json]
workspace materialize --snapshot DIR --digest SHA256 --output NEW_DIR [--robot-json]
workspace export --snapshot DIR --digest SHA256 --workspace DIR --output NEW_DIR [--robot-json]
workspace apply --snapshot DIR --digest SHA256 --bundle DIR --bundle-digest SHA256 --output NEW_DIR [--robot-json]

trust bootstrap --primary PUBLIC_KEY --release PUBLIC_KEY [--require-hardware]
trust show
trust reset --confirm  # development stores only

recovery setup --store PATH --primary PRIVATE_KEY --release PRIVATE_KEY
recovery status --store PATH
recovery change --store PATH --via primary|release|paper|passkey
                [--authorizer PRIVATE_KEY] [--primary NEW_PRIVATE_KEY]
                [--release NEW_PRIVATE_KEY] [--paper replace] [--passkey replace]
recovery reset --store PATH

generation admit --member DIGEST[:DEPTH][=AGENT,...] ... --key PRIVKEY
                 [--all-agents --registry DIR]
generation witness DIGEST --remote URL [--branch B]
generation activate DIGEST
generation status | list

view materialize --registry DIR
view empty

quarantine exclude DIGEST... --reason TEXT
quarantine all --reason TEXT
quarantine show
```

Except for `preflight`, `workspace` and local-only `recovery`, commands accept `--store DIR`,
`--policy FILE --policy-digest D`, and `--robot-json`. Recovery requires an
explicit store; `status` returns public JSON, while setup/change/reset require
the trusted local foreground terminal and refuse robot mode.

The [private source snapshot commands](../docs/workspace-snapshots.md) freeze
HEAD plus explicitly selected working-copy files and materialize independent
writable source and Git metadata. Export compares actual workspace bytes with
that baseline; apply validates exact bundle/base digests and publishes a fresh
integration tree without executing candidate code. They provide local artifacts; launcher
integration, confinement and verified promotion remain separate work.

Exit status is part of the contract, so an unattended caller never has to parse
prose:

| status | meaning |
| ------ | ------- |
| `0` | succeeded; the subject is admissible |
| `1` | failed; no success claimed; persistence errors may follow atomic publication |
| `2` | succeeded; the subject is **not** admissible — verification failed, or Inspection produced a fatal finding |

```sh
digest=$(louiselm-skills package ~/candidates/my-skill --robot-json | jq -r .digest)
louiselm-skills dossier "$digest" --review-depth read
```

## What is guaranteed

**A digest names one sequence of bytes.** The package digest is the SHA-256 of
the canonical manifest, and the manifest is the sorted list of every file's
path, executable bit, size, and content hash. Modification times, ownership,
permission bits beyond `executable`, empty directories, symlinks, and the
location the tree was captured from never enter it — so the same candidate
captured anywhere produces the same digest, and no local fact can change one.

**Capture fails closed.** A tree it cannot describe unambiguously produces no
package at all: device nodes, sockets, and FIFOs; files with more than one hard
link; directory cycles through symlinks; paths that collide once a filesystem
normalizes them; non-ASCII paths, unless a pinned policy admits them; files that
changed while being read; anything over a policy limit.

Path collision keys use Unicode compatibility decomposition (NFKD), then
ASCII case folding. This rejects canonical and compatibility-equivalent names
even under a policy admitting non-ASCII paths, while preserving original path
bytes in manifests. It does not implement full Unicode case folding.

**Symlinks are resolved, not preserved.** A link's content is copied in as a
regular read-only file, so a later edit to the link or its target cannot change
bytes that were already reviewed. Where the link pointed is a local fact with no
portable meaning, so it lives in Supply lineage, outside the package, and the
Dossier flags every origin that reached outside the candidate root.

**Nothing recorded is trusted.** `verify` re-reads and re-hashes every file
against the manifest, and building a Dossier always verifies, re-inspects, and
re-derives the diff first. A recorded digest is a claim to be checked. An
Agent-produced digest, preview, or analysis is evidence only.

**The rules are content-addressed.** The Inspection policy and Unicode profile
are compiled into the binary, and every finding reports the exact policy digest
that produced it. A replacement policy is accepted only when the caller states
the digest it expects (`--policy` requires `--policy-digest`), which is what
stops a synced dotfile or an Agent-written file from silently widening or
weakening Inspection.

**Hostile bytes cannot act.** Every value that reaches a reviewer or an Agent is
escaped where it is produced, not where it is printed, so the human render and
the robot view carry the same escaped facts. Homoglyphs and hidden characters
are escaped too: showing a zero-width space as itself would hide the finding
inside the report of it.

## Fatal versus findings

Inspection produces two kinds of output.

_Fatal_ means the package cannot be reviewed at all. The class is deliberately
tiny: no `SKILL.md` at the root, `SKILL.md` without usable frontmatter, or a
file that is neither binary nor valid UTF-8. Growing this class moves judgement
from the reviewer to a scanner that cannot read prose.

Everything else is a _mandatory Dossier finding_ — a fact the reviewer must be
shown, never a verdict: hidden and bidirectional code points, ASCII homoglyphs,
terminal control sequences, URLs, credential references, encoded payloads and
decoders, network and process reach, SVG that acts rather than draws, declared
and undeclared binaries, executables, images, content contradicting its
extension, and any file the scan budget could not cover in full.

## Assessment

Assessment is a Model's advisory opinion and has no authority. It runs under an
empty capability envelope — an assessor offered any capability is refused before
it is called — and it is keyed to the exact package digest, Model, and prompt
version. An opinion about anything else is treated as absent, not as stale,
because showing a reviewer an opinion about different bytes is worse than
showing none.

## Skill Admission

A package that verifies is not a package anyone approved. Skill Admission is
the local ceremony that turns reviewed packages into a **Skill Generation**: one
signed record binding the complete admitted set, the Dossier each member was
approved from, the claimed review depth, the governing policy, the Provider view
roots, and its place in a chain — sequence and predecessor. The set is admitted
as a whole, with one touch, so addition, deletion, replacement, policy change,
and rollback are all visible as changes to a signed record.

One physical YubiKey holds distinct **Primary** and **Release** credentials.
Primary signs Admissions; Release authorizes trusted builds. A backed-up passkey
and a written paper phrase independently authorize recovery changes, never
ordinary Admissions or releases. There is no separate SSH Recovery role.

Follow the [one-token setup and recovery ceremony](../docs/recovery-ceremony.md)
for provisional first-release signing, atomic installed setup, method/key
replacement, readiness and last-resort reset. Production setup requires both
recovery methods and strict hardware presence/verification on both signing
roles. Development stores cannot be promoted to production authority.

Interactive signing displays `ssh-keygen` touch prompts and diagnostics directly
on the private operator terminal while the helper is running. They are not copied
into captured errors or Agent logs. Noninteractive calls retain escaped failure
diagnostics. Signing deadlines, process cleanup and foreground restoration still
apply; a signing refusal does not trigger an automatic retry.

Normal Admission and `release sign` record exact approved payload digests under
the trust mutation lock. Retired keys verify only that recorded history, never
new or backdated approvals. Signing outside `release sign` does not register a
release for verification after its signing key is retired. Losing all usable
authority requires explicit `recovery reset`, fresh setup and re-Admission.

A signed Generation governs nothing until it is **witnessed**. Its exact bytes
are published to a protected Git branch and read back from the remote before it
can be activated, so a Generation only takes effect once it exists somewhere the
operator does not solely control. The witness ledger is append-only per
Generation: a digest already published with different bytes is refused, never
overwritten. During a witness outage nothing changes, and the previous
Generation stays in force. Activation refuses an older sequence or a different
Generation at the current sequence, so intentional rollback requires a newly
admitted higher sequence. Each lineage pin records activation time. Retrying the
exact current digest confirms activation without appending another pin or
changing its original timestamp.

Activation serializes its Generation records and Supply lineage under the trust
lock. It writes and syncs `activation.pending.json` before replacing any of them;
ordinary failures restore the previous state. An interrupted activation rolls back
before the next `generation status`, `list`, or other Admission operation reads
the store. These APIs report a busy store instead of exposing an in-flight
transaction; raw files are not a committed-state inspection API.

If rollback cannot finish, repair the reported filesystem problem and retry the
operation; retain the journal, which contains the recovery evidence. Once every
new file is durable, journal removal commits the activation. Failure to sync that
removal reports **uncertain commit durability**: retry activation of the same
digest to settle the outcome without creating another pin. A crash in this final
window can recover the complete old or complete new state. Process-crash tests
cover publication boundaries; they do not emulate physical storage failure.

The commit to the witness branch is an ordinary commit. Branch protection on the
remote is the control; a second hardware signature there would cost another
touch and prove nothing the first one did not.

**Emergency quarantine** narrows authority immediately and needs no token:
excluded packages drop out of the current Generation the moment the file is
written. It only ever narrows. Giving authority back requires a newly admitted
Generation, so `quarantine clear` refuses by design rather than becoming a way
to re-enable quarantined supply without a touch.

### What v1 trusts

The ceremony trusts the kernel, the root-owned `louiselm-skills` binary, and the
local TTY. Hardware attestation bytes may be recorded alongside an enrolled key,
but nothing here validates a manufacturer certificate chain, so the record says
`validated: false` and the bytes are evidence only — never proof that a key is
genuine hardware.

Root-owned release installation and protected trust have passed the disposable-VM
acceptance recorded in `louiselm-lm70`. A development store remains writable by
its operator and permanently untrusted; it does not acquire that boundary by
running these commands. Desktop deployment and the complete Verified Session
cutover remain `louiselm-d6fv.9`.

### The manual ceremony

Automated tests cover the chain, the state machine, the witness protocol, and
signature verification, using software keys. They cannot cover a physical touch.
Complete the linked one-token ceremony first, then use the installed tool and
protected production store for Admission. Replace digest/remote placeholders:

```sh
skills=/usr/local/lib/louiselm/current/bin/louiselm-skills
production_store=/var/lib/louiselm/skills
sudo "$skills" generation admit --store "$production_store" \
  --member 'sha256:<pkg>:read=claude,codex' --key "$HOME/.ssh/id_louiselm_primary"
sudo "$skills" generation witness --store "$production_store" \
  'sha256:<generation>' --remote git@your.host:infra/skill-witness.git
sudo "$skills" generation activate --store "$production_store" 'sha256:<generation>'
sudo "$skills" generation status --store "$production_store"
```

Every `--member` names the Agents whose Instruction view the package enters, and
that scope is part of the signed bytes. `--all-agents --registry DIR` expands to
the literal Agent names in that registry at signing time, so the record never
means something different because a later Agent was registered; scoping a
package differently is a new Generation, not an edit. Views are keyed by Agent,
never by Provider (louiselm-5qzq).

After activation, `view materialize --store DIR --registry DIR --robot-json`
publishes an immutable Instruction view for each registered Agent. It verifies
the current Generation's signature, witness state, policy and complete package
set before selecting the signed members. Missing, altered or quarantined supply
refuses the operation; no member is silently dropped. Repeat the command to
verify existing artifacts. A registry entry with no members receives the empty
view; membership naming an unregistered Agent creates nothing on that host.

Each result names a digest, `root` and `skills_root`. The store layout is
`views/sha256-<view-digest>/view.json` plus
`skills/sha256-<package-digest>/SKILL.md` and the package's supporting files.
The digest covers a canonical description of Generation, Agent and package
identities, independent of host paths. Publication never overwrites an existing
destination; files and directories are read-only and every reuse checks their
contents, modes and complete inventory. The store owner must still protect its
root from untrusted writers.

`view empty --store DIR --robot-json` produces the canonical empty mask for
`skills=off`, without requiring a Generation or trust enrollment. Its
`skills_root` exists and contains nothing. A failure produces no usable view.
Both commands perform blocking local I/O and return `managed_supply` errors.
Materialization is the artifact step of Admission; it does not mount a view or
enable Verified launch. Native-source masking and the read-only mount remain
under `louiselm-d6fv.3.3` and `louiselm-d6fv.9`.

Check physical presence and PIN/user verification yourself. The verifier
requires the signature assertion flags; key-generation options alone do not
set its policy. Recovery changes inherit that policy, and a replaced Primary
must be refused for a new Admission. Verification/status need no token touch.

### Session input binding

`session_manifest::SessionInputs::resolve` reads the registered Agent,
remeasures its runtime and materializes its current witnessed Instruction view.
The view carries its Generation identity from the same locked resolution,
including when that Agent has no admitted members. This blocking API refuses
runtime drift, missing supply and altered views; it never falls back.

The caller supplies the per-Session project-instruction, tool-schema and
plugin-schema snapshots, measured cache-base digest, isolation evidence reference
and exact capability envelope revision. The [private cache APIs](../docs/session-caches.md)
capture immutable warm bytes and seed independent Session overlays; even an
empty cache requires its explicit digest. `MeasuredInput::from_bytes` binds captured bytes by path,
size, executable bit and SHA-256. `None` refuses binding; `Some([])` explicitly
records an empty snapshot. Verified v1 requires `Some([])` for the ACP MCP list.
Project instructions remain measured Session inputs, never admitted packages.
Workspace files stay editable; automatic loading must use the frozen Session
snapshot. Later edits take effect in a new Session, not through rediscovery.

`SessionInputManifest::build` creates `louiselm.session.input-manifest/1`.
Its canonical bytes bind all inputs, the governing policy, and disclosure for
every reachable Provider. File sets and runtime baselines are sorted; Agent
argument and routing order is preserved. `parse` checks bounded, closed,
canonical JSON, including nested records. The digest uses the spelling already
accepted by `LaunchRequest::validate`.

This is an input record, not launch authority. Parsing cannot prove provenance,
complete discovery-source control, immutable mounts or continued currency.
Resolve immediately before binding, protect captured snapshots, and require
the adapter/confinement proofs before reporting Verified posture. Do not log
manifest bytes: registered arguments and environment can contain secrets.

### Discovery evidence and supply status

`discovery::Inventory` declares every source category for a versioned adapter.
Its canonical `louiselm-discovery.json` must be registered among the measured
runtime adapter files. `AuthenticatedInputs::verify` binds the manifest and
source observations to the existing signed sequence-zero launcher receipt;
`DiscoveryProof::verify` remeasures the runtime and checks exactly one matching
snapshot or mask for every declared source. A proof is specific to one complete
launch request, including its Session and Run. Native MCP is always masked.

These are evidence-consumer APIs, not installed-adapter conformance. Generic
Bubblewrap evidence has no source observations and fails discovery verification.
The production launcher must establish lifetime controls before signing source
observations: fixed executable, no self-update, no mutable instruction reload,
snapshot routing and complete native masks. Actual adapter inventories, hostile
discovery probes, snapshot storage/mounting and launch refusal wiring remain
`louiselm-d6fv.9`; component tests do not enable a user-visible Verified launch.

`supply_posture::derive` independently produces the four supply/disclosure
`DimensionInput` values. It rechecks the current Agent view and registered
runtime, and accepts native/disclosure evidence only through authenticated
launch inputs and request-bound discovery proofs. Missing or mismatched
artifacts fail their dimension with a fixed code and next action. A runtime
control known to be unsafe fails even if the executable's bytes still match.

The existing `Posture::evaluate`, human renderer, robot serializer and Lua
presentation validator share `louiselm.verified-posture/1`. They include fixed
cloud-plaintext and embedded-executable disclosures, never input contents or
configured Provider names. The isolation/network owners must supply their own
evidence; all four supply dimensions passing does not establish a Verified
launch. Installed status/launch consumption remains `louiselm-d6fv.9`.

The broker's canonical `louiselm.launch.session-status/6` includes a
display-only six-dimension `PostureStatus`, derived from retained trusted facts.
Callers cannot supply its verdict. The initial runtime producer consumes the
authenticated launch/start chain and preserves its original proof-validation
time; other dimensions remain explicitly unverified until their evidence
producers are connected. Status reads run no evidence probes and grant no
authority. The same response includes broker-owned cold-recovery readiness,
with typed missing/pending evidence reasons and the original retained-point
expiry. Readiness grants no admission and promises no future load success.
The required `conformance_admission` field separately reports immutable launch
history: `unevaluated`, `certified` with a report digest, or `waived` with the
exact condition and optional report digest. Historical certification or waiver
never supplies current isolation proof, renews approval or grants authority.
See [canonical status composition](../docs/broker-lifecycle.md#canonical-status-composition)
for the launch-based freshness meaning and remaining integration scope.

### Broker-mediated dependency downloads

`DependencyFetch` is an authenticated Agent operation, not a shell command or
general HTTP proxy. It proposes a `DependencyRequest` with a stable request ID,
exact typed `Candidate`, and bounded byte reservation. The broker validates the
Session, Run, revision, current posture, lifecycle and immutable starting inputs
before any external disclosure. Tools do not inherit this Agent authority.

The trusted controller stages the existing `SessionInputManifest`, then supplies
`GrantRequest.dependencies: Some(ApprovedDependencies)` before launch. The grant
names the manifest digest, selected `Cargo.lock` path, exact lockfile digest,
registry archive templates, pinned IP addresses, explicit preapproved exceptions,
attempt/byte budgets and expiry. Omission denies every dependency fetch; no
existing configuration is automatically opted in. Cargo lockfile versions 3/4
are supported. The broker parses staged bytes, never an Agent-edited lockfile,
and automatically admits only exact starting registry coordinates **and** their
SHA-256 integrity. It does not run Cargo to resolve names.

New coordinates and exceptional sources remain local typed candidates.
Interactive operators inspect and approve exact batches (up to 32 at a time):

```sh
louiselm-control dependencies inspect SESSION --json
louiselm-control dependencies approve SESSION CANDIDATE_ID... --json
```

The dedicated operator socket authenticates the operator before lookup. Approval
persists the complete candidate identity, starts no download and cannot extend
expiry or budgets. The Agent retries its original request after approval. A
`has_more` response indicates another pending page. Unattended Runs never create
pending prompts: every Session needing fetch authority must have its grant
recorded before the Run's first launch is consumed. Already-recorded grants remain
usable; a later Session with no dependency capability is still allowed.

HTTPS uses fixed approved origins/IP destinations, TLS certificate validation,
no DNS lookup, no proxy, no redirects, no content decompression and a deadline
bounded by both the grant and 30 seconds. Arbitrary HTTPS URLs and missing
integrity require explicit approval; URL origins must also have preconfigured IP
destinations. Git/non-registry proposals always need explicit approval, but this
adapter deliberately does not run Git or unknown download helpers: propose and
approve an exact HTTPS archive instead. These approvals grant neither Provider
egress nor ambient Session networking.

Downloaded archives remain opaque bytes. The unprivileged broker delivers bounded
chunks on the existing authenticated supervisor channel. The supervisor verifies
the digest and publishes only `artifact-<sha256>` in its pinned Session cache,
mode 0600, without overwriting existing files or following Agent links. Final
publication shares the local revocation lock. Archive paths, executable bits,
install scripts and post-install scripts are never interpreted or executed.
Missing integrity is reported as unverified, never upgraded by a successful fetch.

Durable intent consumes the attempt and byte reservation before I/O. Unknown
outcomes are not automatically retried or refunded. A success confirms that
publication, not permanent cache integrity: the owning Session can subsequently
modify its cache. A fresh relay retry of a previously published request returns
`Unknown`, never a new integrity claim or another fetch. Local fake-HTTP,
authenticated-relay and actual-filesystem tests
are not a claim of an installed distinct-UID or live-registry acceptance run.

### Broker-mediated Beads mutations

The authenticated Agent command relay accepts typed comments, atomic claims,
nonterminal status updates, individual label additions/removals, typed dependency
edits and closure with one typed verdict. A trusted caller configures `BrokerService` with
`configure_beads_tracker(workspace, program, expected_digest)` and supplies an
`ApprovedBeadsMutations` launch permission with the canonical project digest,
exact issue IDs, sorted exact `effects`, a trusted `role`, a non-refundable
`max_mutations` budget and an expiry. Without configuration and a grant,
mutations are refused. The broker derives the actor from its retained
Agent/Session binding and checks the current
Session, Run, envelope revision, lifecycle state and quarantine status.

Status and label grants name exact values; statuses use lowercase canonical
spelling, never claim aliases. Dependency additions and removals name an exact
edge type and require both issue IDs. Workers cannot close issues, add blocker/parent
edges, remove dependency edges or change the `integration_verified` gate label,
even if those effects appear in a supplied grant. Coordinators still need the
exact effect permission. Claims always use `br update --claim` with the
derived actor; status updates cannot substitute for claiming, closing or deletion.
Close requests carry one `consumer`, `gate`, `live`, `inert` or `none` verdict;
`inert` names its follow-up issue. The broker never supplies `--force` or a policy
bypass. Upstream `br` remains responsible for graph and workflow validation.
Parallel-worker dispatch, integration gates and publication remain `louiselm-qbr.6`.

The runner uses the configured `.beads/beads.db`, checks the executable digest,
clears its environment and bounds execution with process-group cleanup. Durable
receipts retain a request digest, not the comment body or subprocess output.
Identical retries return the same operation. A crash or process-observation
failure can leave `Unknown`: the effect may already have occurred, so the broker
never automatically invokes `br` again for that request ID. A nonzero exit is
also not proof of no write; `Failed` preserves that process result only.

Requests explicitly set `required`. Optional denials return `CapabilityDenied`
without escalation. A denied required action from a live authenticated Session
returns a durable escalation naming the exact project, effect, issue IDs and
minimum role for one attempt. Repeated requests for that same missing scope in
the same Session/envelope revision share one operation UUID and Permission
Required Attention item, even when their retry IDs or comment bodies differ.
The record contains no comment/reason text. Escalation never grants authority;
expiry, exhausted budgets, quarantine and project changes still refuse writes.

The authenticated operator endpoint exposes `operator::beads_mutation` and the
`louiselm.operator-beads/1` request: `operation_id` plus an optional `decision`.
Inspection works after Session loss and returns original request/project digests,
the unchanged outcome, or the exact escalation. To reconcile `Unknown` or `Failed`,
first inspect canonical Beads state and independently retained request/evidence.
If that establishes what happened, submit `reconcile` with `applied` or
`not_applied` and the canonical SHA-256 `evidence_digest`. The broker adds an
immutable operator-UID/timestamp attestation; it does not claim to verify the
evidence, overwrite the original outcome, rerun `br`, or refund the attempt.
Conflicting attestations and reconciliation of `Completed` refuse. If the evidence
is inconclusive, leave the outcome unresolved; absence alone is not proof.
Only after establishing non-application may a separately authorized new request
be considered. The same request ID always retains its original outcome.

For an escalation, `dismiss` clears its Attention item without approving anything;
retry or restart cannot reopen that condition. New authority remains an explicit
trusted-controller decision. Agent requests cannot inspect or settle operator
records. CLI/UI presentation remains separate work.

Installed startup reads `/etc/louiselm-broker-beads.json`, selected explicitly by
the administrator. Absence leaves ordinary broker startup usable and all tracker
mutations disabled; invalid configuration refuses startup. One canonical project
is configured per machine-wide daemon. The operator's per-launch mutation grant
must name its project digest; switching the configured project cannot transfer
old grants, even where issue IDs coincide. Agent requests contain no project
path, actor, executable, or routing override.

Prepare an **existing** canonical project outside Session-writable storage and a
root-owned, non-writable copy of the reviewed `br` executable. The project and its
ancestors must belong to root or the installed operator, without group/other
write permission. `.beads` and all its contents may belong to root, the operator,
or the broker; only the broker's dedicated group may have group write access.
The broker needs traversal, database read/write and directory write access.
An operator-owned `.beads` directory with the broker GID and mode `2770`, and
database files with that group and mode `0660`, supports shared administration;
ensure subsequent operator writes preserve these permissions. Symlinks, hard
links to files, special files and extended ACLs are refused. The pinned executable
and every ancestor must be root-owned without group/other write permission.

After reviewing those paths and the binary's exact SHA-256, provision with:

```sh
sudo python3 scripts/install-broker-beads.py \
  --workspace /srv/louiselm/project --br /usr/local/lib/louiselm-tools/br \
  --sha256 REVIEWED_64_HEX_DIGEST
```

Run from the repository root. The command changes only the protected configuration;
it never initializes, moves, or changes ownership of existing tracker data. An
identical rerun is harmless; a differing existing configuration is refused.
Its JSON result gives `project_digest` (SHA-256 of the canonical absolute path's
filesystem bytes) for `GrantRequest.beads_mutations`, alongside sorted exact issue
IDs, exact `effects`, `role`, `max_mutations` (1–64), and `expires_at_ms`. Provisioning grants no mutations
by itself. Restart the updated installed broker to load it; the trusted controller
must still pass the explicit grant through `InstalledBroker::authorize`.

Project-bound launches receive disposable [Session Beads replicas](../docs/beads-replicas.md).
Native `br`/`bvr` reads use `BEADS_DIR`; completed canonical mutations refresh
the local copy before returning their receipt. Provisioning denies Session
identities direct canonical database/JSONL access. Local replica edits never
flow back into canonical storage. Unconfigured or ungranted launches have no
replica; broker restart and Disposal preserve the documented publication and
cleanup boundaries.

The regular suite covers the authenticated service and relay. To exercise an
existing upstream `br` against a disposable project, from `skills-core/` run:

```sh
LOUISELM_TEST_BR=/absolute/path/to/br cargo test --all-features --locked \
  --test broker real_br_ -- --ignored --nocapture
```

The installed acceptance is
`launch_supervisor::system::installed_tests::daemon::beads::privileged_installed_tracker_routes_only_approved_mutations`.
Build the binaries and library test, then run that exact test in the
[disposable VM](../docs/launcher-vm.md), under root and a private mount namespace,
with `LOUISELM_REQUIRE_BROKER_BEADS=1` and `LOUISELM_TEST_BEADS_INSTALLER` naming the provisioning script
(keep its sibling `install-broker-attention.py` beside it). It exercises unset
startup, protected configuration and digest refusal, distinct-UID denial of direct
writes, and durable at-most-once outcomes through the measured Agent relay.
CI runs this composition with `/usr/bin/true` as the process stand-in. Set
`LOUISELM_TEST_BR` to an existing upstream binary to additionally prove the real
claim, exact label and single correctly attributed comment despite Agent retries. This fixture proves
installed composition, not activation of the maintainer's desktop.

### Prospective artifact preflight

`preflight --request request.json --manifest inputs.json --robot-json` reads
exact canonical `louiselm.launch.request/2` and
`louiselm.session.input-manifest/1` bytes. These are proposed inputs, not an
approval or proof that a Session ran. Do not log the input files: they can
contain runtime arguments and environment. The safe output contains only
opaque identities, digests, closed codes and fixed notices.

The versioned `louiselm.launch.preflight/1` record separates `proposed` identities
from the existing six-dimension `posture`. It checks request/manifest digest,
Agent, Generation and envelope bindings before using proposed bytes to constrain
independent view materialization and runtime measurement. Missing or contradictory
bindings cannot verify either artifact dimension. Artifact failures stay
dimension-specific. It uses the embedded supply policy, existing supply store
(`--store` or the normal store location), and `Registry::open_trusted`
(`--registry` or `/var/lib/louiselm/registry` on Linux). Missing/untrusted readers
provide no trusted evidence. Inspection may create the existing immutable view
cache, but does not change permissions, start an Agent or record approval.

Native discovery controls, isolation, network enforcement and bound Provider
disclosure remain unproven in this prospective mode. A proposed isolation
reference is not a verified mount. A proposed disclosure digest is not recorded
disclosure evidence. Network scope is explicitly unresolved: these input records
do not contain network rules bound to the requested envelope revision, and
today's registry must not be used to reconstruct a prior revision. The isolation
contract is also unresolved: these proposed artifacts do not identify the
contract enforced by a launcher. Actual launch receipts bind that contract.
Output therefore exits **2**, never launch-ready success; malformed commands or
input files exit **1** with fixed diagnostics. Human output omits no robot
identities or diff results. `preflight --direct` separately reports that a direct
vendor command, including a wrapper, has no LouiseLM Verified posture.

Add both `--previous-request prior.json --previous-manifest prior-inputs.json`
to compare explicitly selected inputs. `comparison.state` distinguishes
`not_requested`, `different_agent`, `inputs_unavailable` and `compared`.
Comparison requires matching manifests for the same Agent, not the same Run;
it does not assert shared workspace or previous execution. `changes` contains
typed fields and exact before/after identities. `unresolved` lists unknown
fields instead of treating them as unchanged. No automatic history lookup or
new authority store is involved.

After Neovim setup, `:LouiselmPreflight request.json inputs.json` reads the same
robot record asynchronously and opens `:checkhealth louiselm`. Two additional
files select the prior request/manifest. Health labels its retained result as a
selected snapshot, never live status. A new selection clears the old result;
setup/reset cancels pending work and clears it. For a repository-built executable
or explicit store/registry, use the headless UI adapter:

```lua
local started, err = require("louiselm.health").preview({
  command = "/absolute/path/to/louiselm/skills-core/target/debug/louiselm-skills",
  request = "/private/request.json",
  manifest = "/private/inputs.json",
}, function()
  vim.api.nvim_cmd({ cmd = "checkhealth", args = { "louiselm" } }, {})
end)
assert(started, err)
```

`require("louiselm.preflight").read` exposes a disposable async reader without
health/UI ownership. It bounds stdout to 64 KiB, discards stderr, imposes a
30-second process timeout, schedules completion onto the main loop and ignores
completion after disposal. JSON decoding validates fixed presentation fields;
it never creates authority from a received status.

Refresh whenever inputs change. This command does not offer an approve/launch
action. The future launcher consumer (`louiselm-d6fv.9`) must recheck artifacts
and bind the exact displayed `request_digest`, or require a refreshed preview;
it must never preview A and launch B. Authorization and canonical live Session
status remain broker-owned (`louiselm-qbr.5.1.2`). This component does not enable
a user-visible Verified launch.

## Broker Session ownership

`BrokerService::serve_launch` returns an owned `BrokerSession` only after both
exact launch receipts are durably acknowledged. It retains the consumed
authorization, the initial sequence-1 receipt head, and the original
credential-authenticated supervisor channel. The continuing broker worker owns
packet ordering and correlation; `launch_head()` is a launch snapshot, not live
Session status.

Installed receipt verification uses a root-owned genesis binding below
`launcher/receipt-bindings/`, indexed by the digest of the Session ID. The
signer records the exact genesis payload digest and original Session, Run,
release and key before returning its first signature. Registration shares the
installer/rotation lock, so a stale signer cannot admit a new chain after its
key or release changes. Exact retries preserve the original binding; failed
registration returns no signature or durability claim.

Routine key rotation permits already registered Sessions to continue with their
original key. Installed verification rereads the public keyring and checks the
registered binding, so both old and new chains verify without restarting the
broker. A release upgrade preserves the root binding and canonical broker
receipt bytes; historical verification does not authorize running an old
release or automatically resume a Session. Unknown/unregistered histories fail
closed rather than acquiring trust from their own signatures or timestamps.
Unverifiable histories are quarantined per Session.

### Provider credential custody

`InstalledBroker` loads reusable Provider credentials under its dedicated
non-root UID/GID before accepting connections. After verifying the existing
state identity marker it provisions `provider-credentials/` inside the private
broker state directory, with exact mode `0700`. An empty directory configures
no Providers and leaves ordinary launches available.

Provision each credential through a trusted operator channel as a broker-owned
mode-`0600` regular file named for its configured Provider id (1–64 lowercase
ASCII letters, digits or hyphens). The contents are nonempty UTF-8, at most
16 KiB, with surrounding whitespace removed. Do not put credential bytes in
command arguments, environment variables, Session files or logs. Restart the
broker to reload changed files; custody never imports other tools' stores.

Startup refuses foreign ownership, wider or special mode bits, symlinks,
multiply linked files, special files and malformed contents with the existing
`CredentialUnavailable` protocol error. Opened inodes are pinned before reading;
secret buffers are zeroized on drop and never serialized or included in Debug.
`provider_credential(id)` returns a `CredentialHandle` containing only the id.
It grants no request authority by itself. An API key needs no refresh; rotate it
by replacing the file and restarting the broker.

### Brokered Provider requests

A launch grant may carry `provider_requests` (`ApprovedProviderRequests`):
the configured Provider id, the exact HTTPS `upstream` URL
(`…/v1/responses`), controller-selected `addresses` (no DNS lookup), a
non-refundable `max_run_requests` shared by every Session of the Run, the
sorted `models` a request may name, the `max_effort` ceiling for
`reasoning.effort` (`none` < `minimal` < `low` < `medium` < `high` < `xhigh`
< `max`), and an expiry. Absence denies every request. A request naming another
Model, stating a higher or unknown effort, or stating none is refused with
`CapabilityDenied` before any unit is spent.

`provider_endpoint::serve_provider_connection` accepts only the Responses
request shape stock Codex was observed to send (reviewed headers and top-level
fields, `Host` equal to the endpoint's own authority, no `Authorization`,
compression or chunked bodies; 32 MiB body bound). Each complete request goes to
`BrokerService::serve_provider_request` on the Session's worker, which checks
the grant, credential and live supervisor status, durably spends one Run unit
under `provider-requests/`, rechecks expiry and channel after that write, and
only then sends the request upstream with the broker-held key as the bearer.
Upstream `401`/`403` is a typed `CredentialUnavailable` refusal; nothing is
retried and a spent unit is never refunded. The response streams back as it
arrives.

Not yet enabled: Park/Attention on exhaustion and expiry is
`louiselm-qbr.5.1.3.2.3`, and placing
the listener in a Session's network namespace behind the kernel sender guard
`louiselm-qbr.5.1.3.2.4`. Until then the sandbox refuses `Brokered` network
and nothing in production serves the endpoint. The Verified-launch gate
remains `louiselm-qbr.5.1.3.3`.

The existing closed receipt/audit schemas exclude credential fields. The
required installed-custody VM gate checks actual distinct-UID denial, both
process environments and argument lists, Session files, authorization records,
receipts and audit records. See [broker launch acceptance](../docs/broker-launch-acceptance.md).

### Retired launcher private keys

Newly installed keys carry an exhaustive `signing_sessions` ledger in the
root-owned keyring. Admission records a live reference before returning the
genesis signature. Park, a signed terminal receipt, broker restart, or loss of
the supervisor does not release it. The owning supervisor completes the
reference only after process/identity cleanup, relay quiescence and durable
terminal receipt acknowledgement, with its event receiver disconnected.
Completed Sessions cannot sign again, even when another Session retains the key.

Rotation removes an unused retired private key; final Session completion removes
it when the last reference completes. Both share the admission/signing/rotation
lock. Cleanup durably closes signing before unlinking exactly `key` in that
key's private directory, then fsyncs the directory. It preserves public keys,
historical genesis bindings, key directories and exact receipt bytes. Installed
validation accepts the deliberately absent historical private file.

For a failed cleanup, use the current verified release from a trusted
administrator terminal, with the exact retired key ID from `launcher status`:

```sh
sudo /usr/local/lib/louiselm/current/bin/louiselm-skills launcher cleanup-key \
  --expected-key-id "$retired_key_id" --robot-json
sudo /usr/local/lib/louiselm/current/bin/louiselm-skills launcher status --robot-json
```

Retry the same key after lock contention, unlink failure or failed fsync, even
if the file already appears absent. `private_key_cleanup_authorized` records
irreversible signing shutdown; it alone does not claim successful unlink/fsync.
Status reports pending removal and missing reference authority with next actions.
Supervisor completion failures return `KeyCleanupUnavailable` with that maintenance
action; they do not report an already acknowledged receipt as unstored.
Active keys, revoked keys and outstanding references refuse routine cleanup.
Revocation remains a separate compromise decision and is never undone here.

Missing reference authority stays unknown, including keys created without the
ledger. No directory scan or release upgrade backfills it. A crash or incomplete
launch may leave a stale live reference: retain that key and inspect trusted
supervisor/lifecycle evidence; this command has no force option. Do not edit
references, infer completion from age, Agent claims or process absence, or restore a key whose
signing lifetime has closed. Uncertain cleanup preserves required material.
The ledger shares the installed JSON size bound; an oversized update refuses
before replacing existing authority. It does not discard historical references.

Disposable installed tests cover two real Sessions, final disposal, broker
restart, release upgrade and unchanged historical verification; separate signer
tests cover Park/resume and signing races, and fault tests cover unlink/fsync
retry. These gates do not perform production maintenance or prove recovery
after root compromise.

### Compromised launcher keys

From a trusted administrator terminal, set `compromised_key_id` to the exact
installed key ID being revoked and use the current verified release:

```sh
sudo /usr/local/lib/louiselm/current/bin/louiselm-skills launcher revoke-key \
  --expected-key-id "$compromised_key_id" --robot-json
sudo /usr/local/lib/louiselm/current/bin/louiselm-skills launcher status --robot-json
```

Revocation persists `revoked_at_ms` independently of routine retirement. It
invalidates every receipt under that key, including receipts created before
the decision. The timestamp is administrative metadata, never a trust cutoff.
The operation shares signing/rotation serialization; a busy operation refuses
and may be retried with the same key. Success proves durable revocation only.
Retry, reinstall and rotation cannot undo it. Revoking the active key does not
automatically create a replacement key or launch a replacement Session.

Installed supervisors recheck authority on a fixed 250 ms host interval, with
their existing bounded operation timeout. Authority-read failure also withdraws
continuation. They revoke the capability enforcer before attempting whole-tree
freeze, retain successfully frozen identities, and use existing fail-closed
cleanup if narrowing fails. Late signatures, receipts and reconnects cannot
reenable a withdrawn Session. A controller disconnect still disposes its tree.
New launch signing and pre-enable checks also require current authority.

`launcher status` reports revoked keys and affected root-registered Sessions.
`InstalledBroker::key_revocation` exposes the same per-Session inspection to
trusted broker consumers. `observation: null` means containment is unconfirmed;
`frozen` or `failed` records a local supervisor observation below
`launcher/key-containment/`. These root-owned records are unsigned control
observations, not Launcher receipts or continuing liveness guarantees. A
failure reading them must not become a successful containment claim. Inspect
controller/supervisor health and keep affected Sessions disabled. Original
receipt bytes remain untouched and untrusted; no automatic recovery, re-trust,
replacement launch or host-compromise repair is provided. Unaffected keys remain
usable. Neither Agent actions nor broker projections can change key trust.

Closing the rendezvous listener does not close returned Sessions. Explicitly
closing or dropping a `BrokerSession` closes its channel, including clones.
This initiates existing broker-loss handling; closing a socket is not proof of
completed grant revocation or process cleanup.

The pending authorization also holds the optional exact `ApprovedCommands`
record. `serve_launch` derives Agent attribution from the verified Start receipt
and binds that approval before acknowledging sequence 1. Receipt schema `/4`
requires the actual Agent PID/assigned identity and tool-isolation evidence
digest; a signed foreign identity is refused. An absent command approval grants
no effects. Within an explicit command approval, omitted (or `null`) `uses`
means no invocation quota; `uses: 1` through `uses: 64` opts into a finite,
non-refundable limit. Finite parents cannot delegate uncapped authority.
This applies to ordinary and unattended Runs without changing exact command
scope, generated-work budgets, expiry or operator-selected permission policy.
Approval expiry includes time spent awaiting the supervisor and
completing startup.

`inspect` separates the durable receipt state/head and signed prerequisites from
launch acknowledgement. `inspect_active` adds the owning worker's final ACK send
and local channel state. A stored Running receipt alone is not launch success or
current Agent liveness; inspection without the owner deliberately makes neither
claim.

`InstalledBroker::bind` composes this service using the installer's root-owned
`launcher/public-config.json` and public keyring. It requires the dedicated
installed UID/GID, no supplementary groups, and pre-provisioned private broker
state and rendezvous directories with mode `0700` below root-owned ancestors.
The public configuration contains measured paths/digests and numeric identities,
not keys or credentials. The private configuration and signing keys remain
root-only. Signature checks use the measured absolute OpenSSH executable, empty
environment and a bounded process deadline. `InstalledBroker::step` processes
commands and signed lifecycle outcomes on the retained connection.

The [broker launch gate](../docs/broker-launch-acceptance.md) exercises this
composition with measured fixtures in the disposable VM. It does not install a
desktop service, enable vendor Agents, or establish fully Verified posture.
Recovery remains `louiselm-qbr.5.1.2`; user-visible cutover remains `louiselm-d6fv.9`.

## Sandbox startup

Sandbox startup uses the release's existing `louiselm-launch` executable as a
single-threaded bootstrap. It blocks while the parent admits its PID to the
Session cgroup, then receives only the workload stdin and Bubblewrap's two gate
descriptors over a private socket and replaces itself with Bubblewrap. Descriptor
inheritance is configured only in that fresh process; concurrent parent spawns
cannot inherit the gates. Host identity is still checked before the workload is
released. The crate denies unsafe Rust across its binaries and tests. The sole
reviewed exception temporarily masks SIGTTOU while returning foreground terminal
ownership after bounded interactive signing (`launcher_install/foreground.rs`).

The privileged launcher pins its bootstrap to the validated release directory.
Development callers of `BubblewrapBackend` can select the Cargo-built launcher
explicitly with `with_bootstrap`; that executable must be trusted and traversable
by the assigned Session identity. This does not make a development build a
verified release.

Without a usable cgroup, NamespaceOnly preparation pins Bubblewrap's observed
PID-namespace leader with a pidfd before releasing the startup gate. Disposal
kills that leader first and lets the monitor reap it; success requires both
processes reaped, including when the workload never started. A failed proof
returns `CleanupUnproven` and retains the Session's handles for retry. Complete
membership is unavailable (`processes()` returns `NoCgroup`), and the disposal
report's initial count covers only the monitor and leader. This does not supply
cgroup freeze/interrupt control or verified Lifecycle evidence.

`without_cgroup()` selects this development path explicitly for conformance on
hosts that also offer delegated cgroups; HostIdentity plans remain refused.

## The trusted release

Everything above assumes the binary enforcing it is not one the Agent can
rewrite. Running from the development checkout makes that assumption false: the
code being confined can edit the code that decides whether confinement worked.

A **release** breaks the circle. `release build` refuses anything but a clean
commit — untracked files count as dirty, because a file that is not in the
commit cannot be reviewed by reading the commit and can still be compiled in —
and binds the commit, the locked dependencies, the toolchain, the policy, the
schema set, and every resulting byte into one manifest. Its digest is the
release identity. The **Release** credential signs it in its own namespace,
distinct from Primary even on one token. Neither recovery method can sign it.

The standard bundle includes `louiselm-skills`, `louiselm-launch`, and
`louiselm-control`. If any declared executable is absent after the locked Cargo
build, assembly fails without publishing a release manifest.

Installing is content-addressed and atomic. Each release lands in its own
immutable directory and the prefix's `current` symlink is replaced by a rename,
the one operation that cannot be observed half-done:

```text
<prefix>/releases/<release-id>/{bin,policy,schemas,manifest.json,manifest.sig}
<prefix>/current -> releases/<release-id>
<prefix>/state.json
```

A failure anywhere before the rename leaves the previous release exactly as it
was, and the previous release's files stay on disk and usable. Downgrades are
refused: the safe path away from a bad release is building a newer one.

The install request supplies a bundle and a prefix and nothing else. Component
names come from a fixed allowlist, the layout is decided by the tool, and modes
are set by the tool — an install cannot introduce a new command, a different
policy path, or a different owner.

**Ownership is reported, not asserted.** This process cannot make a file
root-owned without being root, so `release status` says what the bytes actually
are — uid, world-writability, and whether any installed component's hash has
changed since install — and names the next action. It never claims a trust
boundary the filesystem does not show.

**A development build says so.** `release identity` reports `unverified` with
the code `development_build` for anything not running from inside an installed
release. A store created by a development build is marked untrusted at creation
and stays that way, and a development build may not activate a Skill Generation
in a trusted store. Promotion is not offered: an Agent that can write the store
must not also be able to decide the store is trustworthy.

### Manual release acceptance

Automated tests assemble bundles from fake component files and install into
temporary prefixes, so they cover identity, signing, tampering, atomicity, and
downgrade without a nested build or root. `louiselm-lm70` records completed
disposable-VM acceptance of genuine YubiKey signatures, root-owned installation
and upgrade, downgrade/tamper refusal, and exact restoration. This does not
certify desktop deployment or complete recovery readiness; unfinished installed
Android/YubiKey recovery acceptance remains `louiselm-d6fv.11.5`.

Use the [one-token release/recovery runbook](../docs/recovery-ceremony.md), not
older two-token rotation recipes. It distinguishes the permanently untrusted
first-release store from fresh installed production setup. Beads records the
exact clean source, two unsigned bundles and transfer checks; do not reuse
pre-recovery artifacts. Signing/install, upgrade/downgrade and destructive
tamper checks are maintainer-operated, inside the disposable acceptance VM.

### Manual launcher-authority acceptance

This procedure changes root trust data, subordinate-ID reservations, and
`sudoers`. Run it only in a disposable VM, using a dedicated operator account,
and take a snapshot before the destructive digest checks.

For the rootless host-side QEMU/KVM setup and disposable reset procedure, see
[the launcher acceptance VM runbook](../docs/launcher-vm.md).
Before installed-authority acceptance, run the separate
[hostile conformance gate](../docs/launcher-conformance.md). It uses deterministic
Agent/service doubles, requires actual kernel denials and cannot grant Verified
posture or substitute for the genuine signing ceremony below.

#### Installer authority (louiselm-d6fv.4.2)

The installer, status, rotation, and lease primitives in this slice are covered
by `cargo test --test launcher_install`. The standard release build includes
the real `louiselm-launch` executable; installation refuses a release that does
not contain those measured bytes.

Once that component exists, install its signed release at the fixed prefix as
described above. Run the following as the VM maintainer; choose unused ranges
if the example ranges collide on the host:

```sh
operator=louiselm-operator
uid_start=2000000
gid_start=3000000
slots=4
broker_uid=1500
broker_gid=1500
skills=/usr/local/lib/louiselm/current/bin/louiselm-skills
launcher=/usr/local/lib/louiselm/current/bin/louiselm-launch

test "$(id -u "$operator")" -ne 0
test -x "$skills"
test -x "$launcher"
sudo "$skills" release identity --robot-json | tee /tmp/release-identity.json
release_id=$(jq -er 'select(.verified == true) | .release_id' \
  /tmp/release-identity.json)
launcher_digest="sha256:$(sha256sum "$launcher" | awk '{ print $1 }')"
sudo "$skills" release status --robot-json | jq -e '.trusted == true'

sudo "$skills" launcher install \
  --operator "$operator" \
  --broker-uid "$broker_uid" \
  --broker-gid "$broker_gid" \
  --uid-start "$uid_start" \
  --gid-start "$gid_start" \
  --slots "$slots" \
  --robot-json | tee /tmp/launcher-install.json
jq -e --arg operator "$operator" \
  --arg release_id "$release_id" \
  --arg launcher_digest "$launcher_digest" \
  --argjson uid_start "$uid_start" \
  --argjson gid_start "$gid_start" \
  --argjson slots "$slots" '
    .schema == "louiselm.launch.install.status/1" and
    .trusted == true and (.failures | length) == 0 and
    .config.operator == $operator and
    .config.release_id == $release_id and
    .config.launcher_digest == $launcher_digest and
    .config.pool == {
      uid_start: $uid_start,
      gid_start: $gid_start,
      slots: $slots
    } and
    (.active_key_id | type) == "string" and
    .retained_key_ids == [] and .occupied_slots == []
  ' /tmp/launcher-install.json
! grep -q 'OPENSSH PRIVATE KEY' /tmp/launcher-install.json
```

Check the installed ownership and modes. Every displayed owner must be `0:0`:

```sh
sudo stat -c '%u:%g %a %n' \
  /usr/local/lib/louiselm/launcher \
  /usr/local/lib/louiselm/launcher/config.json \
  /usr/local/lib/louiselm/launcher/keyring.json \
  /usr/local/lib/louiselm/launcher/private \
  /usr/local/lib/louiselm/launcher/private/keys \
  /usr/local/lib/louiselm/launcher/private/scratch \
  /usr/local/lib/louiselm/launcher/locks \
  /etc/sudoers.d/louiselm-launch
sudo find /usr/local/lib/louiselm/launcher/private/keys \
  -mindepth 2 -maxdepth 2 -name key -exec stat -c '%u:%g %a %n' {} +
```

The expected modes are `0711` for `launcher`, `0444` for `keyring.json`,
`0440` for the sudoers fragment, `0600` for `config.json` and private keys,
and `0700` for all private and lock directories. The installer also records
exactly one non-overlapping reservation under numeric owner `0` in each
subordinate-ID ledger:

```sh
test "$(sudo awk -F: -v s="$uid_start" -v n="$slots" \
  '$1 == "0" && $2 == s && $3 == n { c++ } END { print c + 0 }' \
  /etc/subuid)" = 1
test "$(sudo awk -F: -v s="$gid_start" -v n="$slots" \
  '$1 == "0" && $2 == s && $3 == n { c++ } END { print c + 0 }' \
  /etc/subgid)" = 1

sudo awk -F: -v s="$uid_start" -v n="$slots" '
  !($1 == "0" && $2 == s && $3 == n) && $2 < s + n && s < $2 + $3 { bad = 1 }
  END { exit bad }
' /etc/subuid
sudo awk -F: -v s="$gid_start" -v n="$slots" '
  !($1 == "0" && $2 == s && $3 == n) && $2 < s + n && s < $2 + $3 { bad = 1 }
  END { exit bad }
' /etc/subgid

for uid in $(seq "$uid_start" "$((uid_start + slots - 1))"); do
  ! /usr/bin/getent passwd "$uid" >/dev/null
done
for gid in $(seq "$gid_start" "$((gid_start + slots - 1))"); do
  ! /usr/bin/getent group "$gid" >/dev/null
done
! awk -F: -v s="$gid_start" -v n="$slots" \
  '$4 >= s && $4 < s + n { found = 1 } END { exit !found }' /etc/passwd
```

Validate the exact sudo boundary independently. The operator is pinned by
numeric UID, the component by SHA-256, and the argument vector by the fixed
literal `run`, `prepare` or `certify` argument:

```sh
operator_uid=$(id -u "$operator")
launcher_sha256=${launcher_digest#sha256:}
sudo /usr/sbin/visudo -cf /etc/sudoers.d/louiselm-launch
sudo grep -Fx "Defaults!$launcher fdexec=digest_only" \
  /etc/sudoers.d/louiselm-launch
sudo grep -Fx \
  "#$operator_uid ALL=(root:root) NOPASSWD: NOSETENV: sha256:$launcher_sha256 $launcher run, sha256:$launcher_sha256 $launcher certify, sha256:$launcher_sha256 $launcher prepare" \
  /etc/sudoers.d/louiselm-launch
```

Prove reinstall and rotation are idempotent, retaining public history and only
private keys needed by live Sessions, without exposing private bytes:

```sh
old_key=$(jq -r .active_key_id /tmp/launcher-install.json)
sudo "$skills" launcher install \
  --operator "$operator" --broker-uid "$broker_uid" --broker-gid "$broker_gid" \
  --uid-start "$uid_start" --gid-start "$gid_start" \
  --slots "$slots" --robot-json > /tmp/launcher-reinstall.json
test "$(jq -r .active_key_id /tmp/launcher-reinstall.json)" = "$old_key"

rotation_id=vm-acceptance-1
sudo "$skills" launcher rotate-key \
  --rotation-id "$rotation_id" --expected-key-id "$old_key" \
  --robot-json | tee /tmp/launcher-rotation.json
new_key=$(jq -r .active_key_id /tmp/launcher-rotation.json)
test "$new_key" != "$old_key"
jq -e --arg new "$new_key" '.created == true and .key_id == $new' \
  /tmp/launcher-rotation.json

sudo "$skills" launcher rotate-key \
  --rotation-id "$rotation_id" --expected-key-id "$old_key" \
  --robot-json | jq -e --arg new "$new_key" \
  '.created == false and .active_key_id == $new'
sudo "$skills" launcher status --robot-json | tee /tmp/launcher-status.json
jq -e --arg old "$old_key" --arg new "$new_key" '
  .trusted == true and .active_key_id == $new and
  (.retained_key_ids | index($old)) != null
' /tmp/launcher-status.json
sudo -u "$operator" test -r /usr/local/lib/louiselm/launcher/keyring.json
! sudo -u "$operator" test -r /usr/local/lib/louiselm/launcher/private
# This fresh-install fixture has admitted no Sessions: the old private key is gone.
old_directory="sha256-${old_key#sha256:}"
! sudo test -e "/usr/local/lib/louiselm/launcher/private/keys/$old_directory/key"
```

#### Runtime acceptance

The Rust relay boundary takes `RelayStdio`, not arbitrary blocking `Read`/`Write`
implementations. It owns descriptor-backed controller I/O and preserves bytes
prefetched while reading the launch frame. Callers give it exclusive use of the
open file descriptions, including duplicates, until cleanup restores their
original flags. The CLI duplicates stdin/stdout with close-on-exec enabled.

`SystemRunningAgent` owns one cancellable, nonblocking relay worker. Successful
quiescence joins it and closes controller/child I/O even when the controller stays
open or stops reading. Disposal requires both relay and process-tree cleanup
before the identity lease can be released. Event callbacks must return promptly;
`false` requests retry under backpressure. This does not make uninterruptible
kernel/filesystem faults cancellable, or prove the installed authority ceremony.
The spawning coordinator remains alive through lifecycle cleanup: handing a
sandbox to another Rust thread does not transfer its kernel parent-death binding.
Parent-death protection stays enabled.

An ACP relay failure closes capabilities and proves Disposal before signing a
terminal receipt with the closed `relay_failed` cause. It is not a fabricated
process-exit classification. Earlier lifecycle receipts remain ordered, and
failed signing/storage retains audit intent or exact signed bytes for broker
reconciliation. The owner returns `RelayFailed` only after the terminal receipt
is durably acknowledged; unproven cleanup fails without a successful Disposal
receipt or identity release. This uses the existing receipt schema, with no raw
I/O errors or controller payloads in the new cause.

`cargo test --test launch_supervisor` covers the complete launch transaction
against a deterministic fake Control broker. A live ceremony additionally
requires the real broker from louiselm-qbr.5.1.1 at the installed rendezvous.
Once it is installed, submit one canonical request line as the operator, then
continue ACP on the same stdin. This is the Session-launch invocation; the
separate fixed `certify` maintenance invocation is documented in
[host certification](../docs/launcher-conformance.md#installed-certification-debian-13-x86_64).
Neither accepts a release-ID argument:

```sh
sudo -u "$operator" sudo -n \
  /usr/local/lib/louiselm/current/bin/louiselm-launch run \
  < /tmp/launch-request.json
```

Confirm that `run extra`, `prepare extra`, `certify extra`, internal worker verbs, a copied launcher at a different path, the exact path
after changing one byte, and a rule containing a different valid SHA-256 are all
rejected by `sudo -n`. Roll the VM back after these destructive checks; do not
repair an immutable release in place.

The [Agent process identity boundary](../docs/agent-process-identity.md) pins the
actual workload, not Bubblewrap's reaper. The [tool boundary](../docs/tool-isolation.md)
supports the exact release-bound deterministic test integration, selected with
Agent registration `tool_integration: "louiselm.test-tool-integration/1"`.
Unset or unsupported integrations refuse Verified launch; whole-Session
containment is not Agent/tool-isolation proof.

Capture both receipts from one launch: sequence zero records `Starting`, its
durable acknowledgement permits startup, and sequence one records `Running`.
Rotate the launcher key once and capture another pair. Verify both linked
receipts from each launch. The public keyring must retain both the active and
retired public keys while no private key is readable by the operator. Verify
each canonical payload against the public key selected by its `signing_key_id`
and the fixed namespace. This raw OpenSSH check establishes the signature only;
the installed broker additionally requires the protected genesis binding and
validates the complete receipt chain:

```sh
key_id=$(jq -r .signing_key_id /tmp/receipt.payload)
public_key=$(jq -r --arg key_id "$key_id" \
  '.keys[] | select(.key_id == $key_id) | .public_key' \
  /usr/local/lib/louiselm/launcher/keyring.json)
test -n "$public_key"
printf 'louiselm-launch %s\n' "$public_key" > /tmp/allowed-signers
/usr/bin/ssh-keygen -Y verify -f /tmp/allowed-signers -I louiselm-launch \
  -n louiselm.launch.receipt/2 -s /tmp/receipt.sig \
  < /tmp/receipt.payload
```

Finally, hold identity slot _N_ through one Session. A second acquisition of
slot _N_ must fail busy while an adjacent slot succeeds; after disposing the
first Session, slot _N_ must be acquirable again. This proves the persistent
`locks/<slot>.lock` inode coordinates live leases rather than merely recording
them.

## Gates

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
../scripts/test-skills-core
node --test --test-timeout=5000 tests/recovery_browser.test.cjs
```

The recovery browser gate uses Node.js 22+ built-ins, without npm packages or a
personal browser. It runs the shipped client with deterministic time and async
browser doubles; it does not replace installed Android/YubiKey acceptance.

## Scope

This crate owns packaging, Inspection, the Dossier, Skill Admission, and the
trusted release. It
requires `ssh-keygen` for signatures and `git` for witnessing; both are part of
the trusted base rather than vendored, and this crate implements no
cryptography of its own.

Agent-scoped Instruction views (louiselm-d6fv.3), Session launch and containment
(louiselm-d6fv.4), and portable Endorsements (louiselm-d6fv.8) build on the
canonical contract, the Generation chain, and the release identity defined here.
`louiselm-launch` is built into the signed bundle. The control-service binary
is still owned by louiselm-qbr.5.1; the bundle format already has a slot for it,
and a release that declares a component it cannot produce is refused rather
than shipped short.
