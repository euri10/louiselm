---
name: louiselm-to-beads-bug
description: File a defect as a beads bug with the evidence that proves it, then close it with the evidence that fixes it. Use when a bug is found — during a qa-review round, while coding, from a log or a user report — or when the user says to file, track, or record a bug. Also use for a defect found but deliberately not fixed now, so the deferral survives the conversation.
---

# LouiseLM To Beads Bug

This skill records a defect in beads while the proof is still in front of you. A
bug bead is forensic: it must let someone else — or you in a month — reproduce
the finding without this conversation.

Use the `br` CLI, not legacy `bd`. Always pass `--json` for agent-readable
output, resolve the actor at runtime with `ACTOR="${BR_ACTOR:-assistant}"`, and
sync explicitly with `br sync --flush-only` after creating or updating beads.
Git is your job; `br` never commits.

## Process

1. **File before you fix.** Pre-fix artifacts vanish the moment you fix them:
   the failing log line, the raw payload, the process still hung, the error text.
   Copy them into the bead now, not from memory afterwards.

2. **Verify the claim first.** Read the artifact yourself rather than repeating a
   report. If the cause is still a hypothesis, write it as one — never state a
   guessed mechanism as fact.

3. **Check for a duplicate:** `br search "<distinctive keywords>" --json`.

4. **Pick a priority** by user impact, not by how interesting the bug is:
   `0` data loss, hang, or security; `1` core path broken with no workaround;
   `2` degraded but workable; `3` cosmetic.

5. **Create it** with the template below:

   ```bash
   ACTOR="${BR_ACTOR:-assistant}"
   br create --actor "$ACTOR" "Symptom-first title" --type bug --priority 1 \
     --labels <area> --description "$DESCRIPTION" --json
   br sync --flush-only
   ```

   Title the symptom, not the patch: "Parallel permission requests hang the
   session", not "Add a request queue".

6. **If fixing now:** claim it (`br update --actor "$ACTOR" <id> --status
   in_progress`), write the failing test first, run the project's gates, then
   close with evidence:

   ```bash
   br close --actor "$ACTOR" <id> --reason "Fixed in <commit>. <root cause in one line>. Tests: <names>. Gates: <suite/lint/typecheck results>. <deviations from the filed acceptance criteria>"
   br sync --flush-only
   ```

   Never close on "should be fixed". If you diverged from the acceptance
   criteria you filed, say so in the close reason.

7. **If not fixing now:** leave it open and say plainly in the bead why it is
   deferred and what is unverified. Tell the user the id. A deferred bug that
   only exists in a conversation is a bug you have thrown away.

8. Report the id and title. Commit `.beads/` with your own changes.

## Bug Template

```md
<One paragraph: what breaks, for whom, how often.>

## Observed

<Where it was seen: session/run id, date, environment.>

<The verifiable artifact — log path with line numbers, payload, command output,
process state. Include identifiers so someone else can find the same evidence.
Quote it; do not paraphrase.>

## Symptoms

- What the user sees, in their terms
- What the system reports (status, error text)

## Root cause

<Mechanism with `file:line`. Exact paths and line numbers belong here — unlike a
feature bead, this record is meant to be forensic, not durable-abstract.>

<If unknown: say "unknown", list the hypotheses and what would discriminate.>

## Why it was not caught

<The test gap: what the suite asserts instead, or which fixture was invented
rather than captured. This is the part that prevents the next one.>

## Fix direction

<Where the narrowest correct fix goes, and options rejected with the reason.>

## Acceptance

- A regression test that fails before the fix and passes after
- <observable behavior that proves it, at the boundary the user sees>

## Not covered

<Related breakage found but deliberately out of scope, unverified assumptions,
environments not tested.>
```

## Beads Notes

- File paths and line numbers are the value here. `louiselm-to-beads-feature` tells you to
  keep descriptions durable and path-free; a bug record is the opposite, because
  its job is to point at the defect.
- One bug per defect. Two symptoms from one cause is one bead; one symptom from
  two causes is two.
- A finding that turns out to be a bad reproduction or a wrong expectation is not
  a bug — fix the recipe or the doc and say so, rather than filing noise.
- Bugs found while fixing another bug get their own bead immediately, including
  regressions you just introduced. Own those in the record.
- Link with `br dep add <child> <parent>` when one bug blocks another; keep
  `br dep cycles` empty.
- Never run bare interactive TUI tools while using this skill.
