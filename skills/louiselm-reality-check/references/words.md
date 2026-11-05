# Words

Use these exact words so runs can be compared. Most examples come from public
runs of Jeffrey Emanuel's `reality-check-for-project` skill.

## Status of a promise

| Status | Meaning | Example |
| --- | --- | --- |
| `WORKING` | Reachable from the shipped entry point, and a gate, test, or this session's run would catch a break. | The installed binary synthesized audio end to end during the check. |
| `PARTIAL` | Real, reachable code that delivers part of the promise. | A parser that accepts only a bounded subset of the promised language. |
| `UNWIRED` | Real code that no production path reaches: built but not wired. | A finished renderer that the shipped CLI rejects; a tokenizer whose asset is never embedded. |
| `STUB` | Placeholder, mock, scaffold, ignored option, or deliberate refusal, actually observed. | One-line task modules; a `--robot` flag that is parsed and never read. |
| `UNPROVEN` | Code may exist, but the claim goes beyond the evidence. | An iOS pipeline shipped, but its speed was never measured on a device. |
| `NOT_STARTED` | Nothing exists. | No decode loop file at all. |
| `REGRESSED` | Once worked or was closed with evidence; later evidence invalidates it. | A `consumer:` reference whose call site a refactor removed. |
| `WRONG_APPROACH` | Built in a way that cannot reach the promise; needs a decision first. | Three incompatible file-format specs, each claiming to be final. |

`UNWIRED` is a local addition: his reports file it under `PARTIAL` or a
"wiring" gap. It is the gap a LouiseLM `inert:` close leaves open.

When what ships goes beyond what the docs promise ("code has outrun the
README"), mark the promise `WORKING` and add a docs gap.

Never upgrade a status because a Bead is closed. "Done" without a production
consumer is `UNWIRED`; "tests pass" without a test of this promise is `UNPROVEN`.

## Not evidence

- A Bead closed, a commit message, or an agent's summary.
- A suite that passes with zero tests, or tests that only check a symbol exists.
- A gate that checks files the swarm itself regenerates (self-certifying).
- Red, skipped, or disabled CI, and artifacts older than the code they describe.
- A mock or fixture standing in for the real input on the path being judged.

## Type of a gap

- **Integration**: parts work alone but not together, including finished code no
  production path calls. Usually the cheapest gap with the most value.
- **Implementation**: stub or incomplete code.
- **Proof**: it may work, but nothing would catch a break. Close it with a test
  or gate on the promise itself, not with paperwork.
- **Vision**: a promise with no code and no live Bead.
- **Docs**: the docs promise more, or less, than ships. Never close it by quietly
  weakening the promise: lowering a promise is a maintainer decision.

The "Bead completion illusion": almost every Bead closed, yet the core value does
not flow end to end. It usually hides in integration and proof gaps.

## Coverage of a gap

- `COVERED`: list the live (open, in-progress, or blocked) Bead IDs whose
  completion, as written, closes the gap. A closed Bead never covers a gap.
- `THIN`: live Beads exist, but finishing them as written would not close the
  gap (missing acceptance, wrong boundary, a joint no Bead owns).
- `NO_BEAD`: nothing live covers it. The swarm will never close it.
- `DECISION`: only the maintainer can move it: a choice, an asset, hardware, or
  a human check such as listening or visual review. Name the exact decision.

Coverage answers "is it planned?", not "is it next?". A covered gap on the
critical path can still be the top move.

## Priority of a bridge Bead

`0` data loss, security, or a critical regression; `1` on the critical path to
the first end-to-end result; `2` other gaps; `3` docs; `4` an accepted limit kept
for the record.
