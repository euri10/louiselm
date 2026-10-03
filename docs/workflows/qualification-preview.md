# Curated qualification preview

`scripts/qualification-preview.py` validates a selected, versioned JSON manifest
and prints what a proposed baseline/candidate comparison would use. It makes no
Model request, runs no acceptance command, and writes no result file. It does
not launch a trial or approve a production route.

The committed synthetic example covers bulk reading with source/citation checks,
a mechanical change with a Python unit test command, and reasoning with an
explicit human review rubric:

```sh
python3 scripts/qualification-preview.py validate docs/workflows/qualification-fixtures/manifest.json
python3 scripts/qualification-preview.py preview docs/workflows/qualification-fixtures/manifest.json
python3 scripts/test-qualification-preview.py
```

The v1 manifest contains two declared direct route tuples (configured Agent,
resolved Provider, and advertised Model/options), positive whole-run limits for
Model requests, elapsed seconds, selected input bytes and output bytes, an
explicit Provider disclosure list, and required isolation controls. The sample
names and numbers are synthetic examples, not recommended Models or caps.
The preview cannot verify that an Agent actually advertises the options or that
the declared Provider is resolved correctly; these remain launch blockers.
Classifier, worker and combined routes need their own component tuples in a
future schema and are refused by this direct-pair version.
Request, time and byte caps are local trial controls, not exact remote billing
or token guarantees. Prices and quota stay `unknown` unless measured through
supported runtime reporting; this command never reads credentials or asks a
Provider for them.

Each fixture explicitly names one task input, source and instruction snapshots,
their SHA-256 hashes, provenance, and acceptance requirements. The one pinned
fixture definition is shared by both arms. Paths are relative to the manifest's
directory; absolute paths, traversal, symlinks, missing files, hash drift and
non-UTF-8 files are refused. No other file is discovered or copied. To include
historical conversation content, first curate it into a selected file in the
bundle and mark that fixture `provenance.kind` as `selected_history` with a
reference. An unselected history file is ignored. Review the preview's
`disclosure.possible_files_by_provider` before allowing any future execution.
The command does not print selected file contents.

`acceptance.reference_checks` pin source-backed answer text and citations;
`acceptance.commands` hold argv arrays for tests that the explicit isolated runner
may execute; `acceptance.human_review` records the rubric needed before a
reasoning case can count as quality evidence. Preview never evaluates any of
them. `launch: blocked` lists the unproven containment and runtime properties.
The [explicit paired runner](https://github.com/euri10/louiselm/blob/main/docs/workflows/qualification-run.md)
describes those requirements, upfront Run approval and verifier preflight.
A preview establishes none of those properties and does not authorize execution.
