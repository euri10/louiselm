# Evidence export

`:LouiselmForensicsExport` creates a new private JSON artifact from explicit
selections in one Forensics record. It works without an open chat. Nothing is
uploaded or attached automatically; the record and its source files stay unchanged.

First inspect the record with `:LouiselmForensicsView /path/to/record.json`.
Choose observation fields or inclusive line ranges from its `evidence_sources`
array (indices start at 1), then supply a new output path:

```vim
:LouiselmForensicsExport /path/to/record.json /path/to/evidence.json observation:capabilities source:1:12:18
```

Escape spaces in command paths with a backslash. The destination directory must
already exist, be operator-owned, and exclude group/world writes. Keep it and
its ancestor paths under your control throughout export. A symlink as the output
directory is refused. Existing files, including the source record, are never overwritten.
The artifact is published through a private temporary file with mode `0600`.
Inspect its item states before attaching it to an external issue.

Selectors are `observation:FIELD` and `source:INDEX:FIRST:LAST`. Observation fields
are `agent`, `agent_version`, `cwd`, `model`, `options`, `capabilities`,
`neovim_version`, `louiselm_version`, `git_commit`, `git_branch`, and `dirty_files`.
Source ranges support indexed `acp_log` and `agent_transcript` regular JSONL files.
Collection may have no log pointer: export reports that selection as missing;
it does not discover or load another Session's logs. Use observation selections
for recorded Git state; export does not execute Git or follow source symlinks.

The versioned artifact uses `kind: evidence_export`, `schema_version: 1`, and
`redaction: structure-only-v1`. Each item carries its selection and a state:
`exported`, `missing`, `unreadable`, `unsupported`, `invalid_json`, `changed`, or
`limit_exceeded`. A range is exported only if every selected line was read and
decoded. Other states contain no partial line payloads. A successful command can
therefore produce an artifact containing only unavailable selections.

Redaction is a conservative structural projection. It retains fixed protocol
field names, a small allowlist of protocol enums, booleans, array order, and
selected line numbers. All other scalar values become `[redacted]`; unknown
object keys are replaced with a count. This removes free-form messages, prompts,
tool arguments, secrets, numeric identifiers, host paths, and Session identities
without relying on secret-pattern matching. Version/model/branch strings are
also redacted. It is useful for event shape and ordering, not verbatim content
or request-ID correlation. Protocol-shaped fields remain untrusted observations,
not claims that a valid ACP exchange occurred.

Bounds are fixed: 16 selections, 200 selected lines total, line numbers at most
10,000, source indices at most 100, 64 KiB per selected line, and a 1 MiB scan
prefix per range. Records and final artifacts are each limited to 256 KiB.
Projection replaces subtrees beyond depth 8 or width 128 with `[redacted:limit]`;
exceeding 2,048 visited nodes replaces the entire value. If the complete artifact exceeds its bound,
export fails without publishing; select less evidence. Repeated ranges each read
their own bounded snapshot. Detected in-place changes during a read refuse that
selection; export does not freeze source writers or guarantee a cross-file instant.

The internal asynchronous entrypoint is
`require('louiselm.forensics.export').write(record_path, output_path, selectors, callback)`.
It returns a cancellation function, or `nil, error` for invalid arguments. The
callback receives `path, error` once on the editor loop. A returned path with an
error means publication succeeded but temporary cleanup failed; do not describe
the artifact as rolled back. Cancellation prevents publication until that operation
is admitted and cannot undo a completed write. Re-registering the UI commands
cancels their pending export and suppresses late notifications.
