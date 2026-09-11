# Private source snapshots

The local `louiselm-skills workspace` commands freeze selected source bytes and
create an independent writable Git repository. They do not launch a Session or
establish Verified posture. Launcher consumption and lifecycle retention are
tracked in `louiselm-d6fv.5.5`; export and independent verification/promotion are
`louiselm-d6fv.5.2` and `.4`.

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
