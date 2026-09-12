# Private source snapshots

The local `louiselm-skills workspace` commands freeze selected source bytes,
create an independent writable Git repository, and export/apply byte bundles.
They do not launch a Session or
establish Verified posture. Launcher consumption and lifecycle retention are
tracked in `louiselm-d6fv.5.5`; independent verification/promotion is
`louiselm-d6fv.5.4`.

```sh
louiselm-skills workspace prepare \
  --repository /path/to/checkout \
  --output /private/snapshots/proposal \
  --include src/edited.rs --include new-file.txt --robot-json

# Inspect the preview and proposal/snapshot.json, then use its snapshot_digest:
louiselm-skills workspace materialize \
  --snapshot /private/snapshots/proposal \
  --digest sha256:REVIEWED_DIGEST \
  --output /private/workspaces/session --robot-json
```

Both output directories must be new, with an existing parent outside the input
tree. Keep snapshot storage and output parents under the operator's control and
inaccessible to Session writers. The future launcher must establish that boundary;
ordinary user-owned files alone cannot isolate processes sharing the same UID.
Neither command alters the source checkout, its index, or its Git configuration.

`prepare` reads the exact HEAD commit once, then copies the working-copy bytes of
each explicitly selected relative file. A selected tracked deletion removes that
file. Staged additions and staged modifications have no special authority: the
working copy is used only for selected paths, and all other tracked files retain
their committed bytes. Untracked and ignored files require exact selection;
directory selections and glob patterns are unsupported. There is no filename
heuristic that can prove a selected file contains no secrets: inspect selections.

The preview lists included/excluded modifications, deletions, untracked files and
ignored paths. Ignored directories are summarized with a trailing `/`; explicitly
selected files inside them are listed separately, while the remaining contents
stay excluded. The frozen `snapshot.json` contains the complete inventory and
selection record; `files/` holds read-only copies. A later checkout, index or HEAD
change cannot alter the captured bytes.

`materialize` requires the exact preview digest and rechecks every inventory file
before publishing. It copies only listed files, so extra files in the snapshot
directory have no effect. Files have private `0600`/`0700` modes; snapshots use
`0400`/`0500`. New Git metadata contains a single baseline commit, raw blobs and a
fresh index, with no source history, remotes, alternates, shared hardlinks, hooks
or filter configuration. Candidate `.gitattributes` cannot transform the baseline
bytes. Later user-invoked Git commands can interpret editable attributes normally.

The initial contract accepts printable ASCII relative paths using the existing
canonical path rules, with an additional ban on backslashes and `.git` components
at any depth. Case collisions, file/directory collisions, symlinks, submodules,
special files and multiply linked selected files are refused. Binary content is
copied unchanged. Executable intent is retained; other permission bits are dropped.
An unselected unsafe replacement of a tracked file is reported as modified while
the snapshot retains its regular committed version.

Limits are 10,000 files/working-copy paths, 16 MiB per file, 128 MiB of source
content and 8 MiB of canonical snapshot metadata. Git output is bounded and each
local plumbing operation has a ten-second deadline. No command runs hooks,
filters, signing, maintenance, candidate programs, or remote fetches.

The versioned JSON preview (`louiselm.workspace.preview/1`) has `snapshot_digest`,
`base_digest`, `base_commit`, file/byte counts and normalized `changes`. The
snapshot digest binds the canonical `louiselm.workspace.snapshot/1` record;
the base digest binds its ordered path/mode/size/content-hash inventory. Neither
digest grants approval. Human output uses the same preview. Payloads and absolute
source paths are absent from default output; errors do not echo raw Git stderr.

Exit `0` means the local operation completed; exit `1` means refusal or failure.
Publication uses a private temporary directory and atomic no-replace rename.
Failures before rename publish nothing and remove staging. A directory-sync
failure after rename can leave the complete output present: inspect it before
retrying, and never overwrite it implicitly.

The public blocking Rust APIs are `workspace::prepare` and
`workspace::materialize`. Call them outside editor/event-loop callbacks. The
regressions run through the shipped CLI in `skills-core/tests/workspace_cli.rs`;
deterministic mutation and failed-publication checks live beside filesystem I/O.

## Export and apply byte bundles

After editing the private workspace, stop its writers before export:

```sh
louiselm-skills workspace export \
  --snapshot /private/snapshots/proposal --digest sha256:REVIEWED_SNAPSHOT_DIGEST \
  --workspace /private/workspaces/session \
  --output /private/bundles/proposal --robot-json

# Inspect proposal/bundle.json and its payload; use the returned bundle_digest:
louiselm-skills workspace apply \
  --snapshot /private/snapshots/proposal --digest sha256:REVIEWED_SNAPSHOT_DIGEST \
  --bundle /private/bundles/proposal --bundle-digest sha256:REVIEWED_BUNDLE_DIGEST \
  --output /private/integration/proposal --robot-json
```

Export compares the complete final filesystem inventory with the validated
snapshot baseline. Agent commits, index/config/hooks, Git status/diff, ignore
rules and Agent-provided path lists have no authority. Only the exact root `.git`
entry is excluded without reading or following it; nested `.git` components are
refused. All other regular files, including ignored and newly created files,
participate. Inspect the bundle for sensitive content before sharing it.

The canonical `louiselm.workspace.bundle/1` record binds the normalized base
digest and complete final inventory. Its digest therefore binds additions,
deletions, binary bytes and executable-bit changes. `files/` contains only added
or modified files; unchanged files come from the exact snapshot during apply.
Empty directories and permission bits other than executable intent are omitted.
Case and file/directory collisions, links, special files, noncanonical names and
multiple hardlinks are refused. Root Git metadata is never copied.

Capture pins objects through Linux descriptors before reading them. It checks
file metadata around reads and compares two complete metadata inventories,
including directory identities, to reject observed concurrent mutation or
replacement. It opens no device/FIFO payload and requires Linux procfs for
reopening pinned regular files. This is race detection, not a freeze primitive:
the caller must stop workspace writers and protect the workspace parent, input
stores and output parents. Future launcher integration owns that lifecycle.

The snapshot's file/content/record limits also apply to bundles. Traversal
additionally permits at most 20,000 total files/directories and 64 directory
levels, including empty directories but excluding root Git metadata. File
content stays raw; path encoding remains printable ASCII.

Apply requires both exact digests and the matching normalized baseline, rechecks
all needed payload bytes and modes, and publishes a fresh source tree with
private `0600`/`0700` files. It never executes Git, hooks, filters, candidate code
or verification commands, and creates no Git metadata. Unlisted bundle-store
files have no effect. Existing destinations and outputs inside either input
are refused. The same private staging, no-replace publication and failure rules
described above apply; a failed operation cannot overwrite an existing tree.

Both commands return `louiselm.workspace.bundle-preview/1`, with `bundle_digest`,
`base_digest`, `result_digest`, final file/byte counts, and sorted `added`,
`modified`, `deleted` paths. Human output derives from that record. Neither the
bundle nor the result digest grants authority: cross-Session bundles stay
untrusted until independent confined verification and exact-digest promotion.

The blocking Rust entrypoints are `workspace::bundle::export` and
`workspace::bundle::apply`. Their CLI regressions cover hostile Git metadata,
determinism, byte/mode reconstruction, malformed records, digest substitution,
unsafe trees and output preservation. Tree-capture unit tests deterministically
inject mutation, replacement, addition and deletion between scans.

## Prepare verification inputs

`workspace verification prepare` binds an exact snapshot, exported bundle and
operator-selected plan into a fresh private job. `inspect` remeasures that job.
Neither command executes the plan, creates a Session, authorizes promotion, or
establishes Verified posture. The broker-driven execution and explicit
existing-checkout promotion APIs are described below.

The closed plan document contains only a schema and ordered commands:

```json
{"schema":"louiselm.workspace.verification-plan/1","commands":[{"argv":["cargo","test","--locked"],"cwd":".","timeout_ms":300000}]}
```

Select the SHA-256 of the exact plan file bytes, including whitespace and any
trailing newline. Every command is required; each has an argument array, a
relative working directory (`.` or a directory represented in the source
inventory), and a positive millisecond timeout. Plans are at most 64 KiB, with
1–32 commands, at most 128 arguments per command, at most 4096 bytes per argument,
and a total timeout budget of one hour. NUL arguments, empty or option-like executables, unsafe
paths and unknown fields are refused. No environment overrides or implicit
shell interpretation are provided. The plan file may contain sensitive arguments:
keep it private. Preparing it is not permission to run it.

```sh
louiselm-skills workspace verification prepare \
  --snapshot /private/snapshots/proposal --digest sha256:REVIEWED_SNAPSHOT_DIGEST \
  --bundle /private/bundles/proposal --bundle-digest sha256:REVIEWED_BUNDLE_DIGEST \
  --plan /private/plan.json --plan-digest sha256:REVIEWED_PLAN_DIGEST \
  --output /private/verification/proposal --robot-json

louiselm-skills workspace verification inspect \
  --job /private/verification/proposal --digest sha256:REVIEWED_JOB_DIGEST \
  --robot-json
```

Preparation reuses the trusted byte-only bundle loader. `source/` contains
read-only `0400`/`0500` source files without Git metadata; `plan.json` retains
the exact plan bytes; canonical `job.json` binds snapshot, baseline, bundle,
plan and the complete result inventory. Publication uses the same private
staging and no-replace rename as snapshots. Existing destinations remain intact.

Inspection checks the selected job digest, canonical record, complete source
inventory and plan bytes. Added, missing or changed source files, executable-bit
changes, links, special files and root Git metadata refuse inspection. Unlisted
files outside `source/` are not inputs. A changed plan, even whitespace-only,
requires a different job digest. Read-only file modes are not a security boundary
against the owning user: keep the entire job and its parent inaccessible to
Session writers, and exclude concurrent writers while inspecting. Later execution
must use a separate confined copy, not mutate this retained input artifact.

Both commands emit `louiselm.workspace.verification-preview/1` with state
`prepared`, job/snapshot/bundle/base/result/plan digests and command count. Human
output uses the same identities. Neither output includes raw commands, arguments,
source payloads or paths. Exit `0` means preparation or integrity inspection
completed, never that verification passed; exit `1` means refusal or failure.
Digests establish byte identity, not trusted origin or approval.

The public blocking APIs are `workspace::verification::prepare` and `inspect`;
keep them outside editor/event-loop callbacks. The actual-CLI regression suite
is `skills-core/tests/workspace_cli/verification.rs`.

## Execute an exact job through the Control broker

`InstalledBroker::stage_verification`, `export_verification`, `run_verification`
and `verification_status` compose with the existing launch and lifecycle APIs.
They are blocking broker-worker entrypoints, not editor callbacks or a new daemon.
The controller supplies its authenticated `LifecycleCaller::Operator` identity;
an Agent-provided role or self-reported result never authorizes verification.
This supports operator-selected automatic policy without requiring a human
prompt for each command. Ordinary Agent/helper command approvals are unchanged.

The trusted controller first stages exact baseline-snapshot and plan bytes in
private broker storage, then explicitly Parks the producing Session. Its existing
Launch supervisor captures its own frozen workspace, exports the bundle and
prepares the job under root-owned storage. The export binds the actual producer
launch, Park receipt, Generation, runtime/isolation receipt chain and job digests.
An older CLI-prepared job has byte identity only: it cannot be retroactively
assigned a producing Session. Use this observed export to establish provenance.
General initial-workspace launch/retention integration remains `louiselm-d6fv.5.5`.

After selecting that exact job and export digest, the controller launches a
distinct configured verifier Agent in the same Run and Generation, with a
different host identity, no ordinary command grants and no cold-recovery
requirement. Verification requires its original Running sequence-1 receipt.
Neither another Session's head nor an already-used verifier is accepted. The
broker durably spends one exact job intent before sending it over the original
authenticated supervisor connection. The supervisor remeasures the protected
job and creates a separate writable copy and tool home; the verifier Agent's own
namespace cannot write that copy or the retained job.

The supervisor runs the ordered required commands through the existing confined
tool backend. Exact argv/cwd values are shell-quoted as literals at that boundary;
shell interpretation requires an explicitly approved `sh -c` command. Environment
is fixed to the tool PATH and private HOME; network, Agent runtime/home, capability
sockets and operator files are not granted. Candidate hooks/build scripts/proc
macros can run only inside this confinement, never through the host preparation
or export APIs. Missing installed isolation support refuses execution.

Each actual command exit/timeout is durably recorded before the next command;
execution stops on the first failure or unknown outcome. Cancellation, lost
verifier lifetime and expiry prevent success. Cleanup failures stay sticky and
prevent identity release. The broker retains normalized command observations,
then requires the verifier's signed terminal Disposal receipt before publishing
a complete record. Neither stdout/stderr nor raw commands, environment, source
payloads or host paths enter this evidence. The original job is never mounted
writable and is remeasured again before the supervisor publishes its result.

`VerificationStatus` distinguishes NotRequested, Unknown, Quarantined and
Completed. Completed means observations and whole-Session cleanup are recorded,
not that commands passed: inspect `record.execution.commands_passed()`. Absent or
uncertain outcomes remain Unknown across restart, and spent launches are never
automatically replayed. Quarantine of either producer or verifier invalidates
applicability on subsequent reads. Evidence binds exact job/plan/bundle identities;
it grants no promotion, installed approval or Verified cutover.

A correlated supervisor refusal still permits an explicit Disposal on that
authenticated connection; the broker obtains cleanup before closing it while
leaving missing command observations Unknown. If the transport itself is lost,
the existing supervisor broker-loss policy freezes/reconciles the Session;
disconnection alone is not cleanup evidence or permission to reuse its identity.

The required privileged CI/disposable-VM consumer is
`launch_supervisor::system::installed_tests::verification::privileged_installed_exact_job_verification`.
It uses actual configured measured fixture Agents, the dedicated unprivileged
broker, installed signing authority and real confined commands. Ordinary host
test runs skip this privileged fixture; a skipped run is not confinement evidence.

## Promote into the operator checkout

Promotion has two explicit phases: preview the exact changes, then commit that
preview. The operator controller connects its trusted applicator directly to the
dedicated broker. `InstalledBroker::serve_promotion` runs on the original producing
Session's broker worker; `workspace::promotion::PromotionClient::prepare` runs as
the non-root operator. These are blocking APIs, not editor callbacks or a new
daemon. The controller supplies the installed broker UID from trusted configuration.
Both endpoints check kernel peer credentials. Agent/helper roles cannot approve a
promotion, and deserializing a request or verification record grants no authority.

`PromotionRequest` binds the canonical verification-record digest, verifier
Session, complete job/snapshot/base/bundle/result/plan identities, destination
device/inode/owner, unique request ID and an absolute expiry of at most five
minutes. Identify the selected checkout with `DestinationIdentity::inspect`;
preparation reopens and checks that exact object. Broker admission requires all
verification commands to have passed, proven verifier disposal, a distinct
producer, current unquarantined evidence and reverified installed receipt chains.
The producer must still be at its exported Park receipt. A later producer
lifecycle head requires a fresh export/verification selection.

The launcher accepts only a retained export ID and exact digests on its existing
authenticated broker connection. It publishes validated snapshot/job bytes below
its fixed Session directory, root-owned and read-only to the installed broker
group. It never receives an operator checkout pathname. The broker remeasures the
transfer and sends bounded normalized inventories and raw bytes to the operator;
the operator independently validates their sizes, hashes and executable intent.
Transfer artifacts follow Session storage retention; general retention and expiry
remain `louiselm-d6fv.5.5`.

`prepare` pins and locks the operator-owned destination and returns an additions,
modifications and deletions preview without changing checkout bytes. `commit`
consumes that handle. The source tree must exactly match the approved baseline,
apart from root `.git`: dirty changes, untracked additions, symlinks, special files
and mode changes refuse rather than being merged. Git metadata is preserved;
neither endpoint invokes Git, hooks, filters or candidate programs. A newly
combined source result needs fresh confined verification.

The caller must stop editor/build writers for the entire preview/commit operation.
The advisory lock serializes cooperating promotion clients; it cannot freeze
arbitrary same-UID processes. Descriptor-relative operations and repeated source
checks reject observed races, but are not a substitute for writer exclusion.
Checkouts must be operator-owned and not group/world writable. The local journal
parent must be private (`0700`), operator-owned and outside the checkout.

Each file effect has a durable broker permit before the write and an acknowledged
observation after file/directory synchronization and readback. Permit creation is
serialized with quarantine: revocation denies subsequent effects, while an
already admitted effect may complete. Only the operator process mutates checkout
bytes. A changed destination, refusal, expiry or I/O failure can therefore leave
an explicitly partial result; completed writes are never described as rolled back.

The operator journal retains numbered `intent` and `done` observations plus
`complete.json` only after complete readback/synchronization. Broker
`promotion_status` reports `NotRequested`, `Unknown` with granted/completed counts,
or `Completed`. A missing acknowledgement remains uncertain across restart.
Reusing the exact request returns only its recorded historical status; changed
request bytes refuse, and neither endpoint automatically repeats writes. Inspect
both journals after an interruption and preserve the partial checkout before
preparing a newly verified result. A later quarantine does not rewrite history
or claim to undo an already completed promotion.

The existing privileged CI fixture above covers actual operator-owned promotion,
preview without effects, changed selections and destination refusal, replay,
quarantine, failed/timed-out verification and preservation of a host-hook sentinel.
This does not activate the user-visible installed Verified cutover, which remains
`louiselm-d6fv.9`.
