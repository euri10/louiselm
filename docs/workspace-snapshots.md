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
establishes Verified posture. Confined execution and evidence-gated promotion
remain `louiselm-d6fv.5.4.2` and `louiselm-d6fv.5.4.3`.

The closed plan document contains only a schema and ordered commands:

```json
{"schema":"louiselm.workspace.verification-plan/1","commands":[{"argv":["cargo","test","--locked"],"cwd":".","timeout_ms":300000}]}
```

Select the SHA-256 of the exact plan file bytes, including whitespace and any
trailing newline. Every command is required; each has an argument array, a
relative working directory (`.` or a directory represented in the source
inventory), and a positive millisecond timeout. Plans are at most 64 KiB, with
1–32 commands, at most 128 arguments per command, at most 4096 bytes per argument,
and a total timeout budget of one hour. NUL arguments, empty executables, unsafe
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
