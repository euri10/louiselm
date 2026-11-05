---
name: louiselm-qa-review
description: Turn recent code changes into a manual QA plan a human runs at the keyboard, then iterate that plan as findings come in. Use when the user asks to manually QA or verify recent work ("QA the last 5 commits", "what should I test from today's changes", "give me a test recipe", "I'm the tester, you write the steps"), or reports the result of a manual test round. Not for writing automated tests — that is ordinary TDD.
---

# LouiseLM QA Review

You write the plan and read the evidence; a human drives the app. They see what
no test asserts: focus, modality, timing, what an editor looks like after a
cancel. Your job is to aim them at the code that actually changed and to be
rigorous about proof.

## Quick start

1. Scope: `git log --oneline <range>` and `git diff --stat <range>`. Ask which
   range if unclear ("last 5 commits" / "since yesterday" / a branch / a ticket).
2. Write a working file (see [TEMPLATE.md](TEMPLATE.md)) with a status table and
   one case per behavior. Tell the user its path.
3. They run a case and report. You pull the objective evidence yourself, confirm
   or refute, update the file, and hand back the next case.

Edit the working file every round. Never make the user scroll the conversation
to find the current steps.

## What belongs in a manual case

Only what automated tests cannot reach. For each change ask: *what would still
pass every test and be broken?*

- Real payloads from the real peer — tests use invented fixtures, so a field the
  peer actually sends may be unhandled everywhere.
- Where focus, cursor and windows end up.
- Modal interactions: two prompts at once, a dialog open while a command runs.
- Cancel, teardown, disposal, reconnect.
- Anything whose fix you cannot verify from inside the process.

Skip what a unit test already covers. Say so explicitly rather than padding.

## Write cases that actually reach the code

State for each case: goal, which commit or file it exercises, exact steps, the
expected result, the **failure signature**, and the evidence command.

The failure signature matters most. "It worked" is unfalsifiable; "the picker
vanished on its own and input reported *Queued for next turn*" is a test.

When the app is driven through another agent or service, the prompt is part of
the apparatus and usually the reason a case misses:

- Name the exact tool or code path you want, or the model picks another one.
- Give absolute paths; a bare filename triggers a search first.
- Check what is auto-approved or short-circuited before your code sees it.
- Fresh session per case: a prior denial makes models hedge and detour.
- Reset fixtures between runs; a file already in the target state produces no work.

If you predict a rough edge from reading the code, write it into the case as a
prediction to confirm or refute. A confirmed prediction is a finding; a refuted
one corrects your model of the system.

## Evidence

Name one artifact that settles the case without the user's interpretation: a
protocol log, a state file, a written file, process state. Give a command that
prints the verdict.

**Validate the check against a known-good run before trusting it.** A check that
false-alarms burns a whole round on a non-bug. Confirm each field means what you
think it means — a terminal state may be reported by a different event than you
expect.

Beware artifacts the current session also writes: pin logs by session or run id,
never by "most recent file".

Say plainly which part of a case the evidence proves and which part rests only
on what the user saw.

## Running the loop

When the user reports a result:

1. Pull the evidence before theorizing. Do not accept or dispute their reading
   until you have looked.
2. Classify: **recipe bug** (your steps never reached the code — fix the case and
   apologize once, briefly) or **product bug** (record it, then fix it with a
   failing test first, run the project's gates, commit).
   Record it **before fixing**, while the failing artifact still exists: invoke
   `louiselm-to-beads-bug` if the project tracks work in beads (`.beads/` exists), else
   the project's tracker. A round can surface several findings across hours —
   what is not filed is lost when the conversation ends.
3. If you committed a fix, the build under test changed: tell them how to reload
   or rebuild, and which cases to rerun. Batch fixes when you can; every fix
   costs them a rebuild.
4. Update the status table and the findings list. Number findings (F1, F2…) and
   record the ticket id and the commit that fixed each, so the working file and
   the tracker point at each other.

Own your own regressions in the findings list. A round that finds a bug you
introduced is the round working.

## Closing the round

Summarize what passed, what each finding was, and — most useful — *why the test
suite missed each one*. That reasoning is what improves the next round. List
leftovers: untracked fixtures, unreverted state, bugs found but deliberately not
fixed.

A round routinely turns up defects outside its scope — a second agent's payload,
an adjacent code path. Do not fix those; file each one (`louiselm-to-beads-bug`) with the
evidence and say it is unverified. Filing is what makes not-fixing safe, and an
out-of-scope finding is usually the seed of the next round.

**This step is required, not a manual call left for later.** A round that ends
without it leaves an untracked working file with no decided fate — that has
already happened twice.

If the project tracks work in beads (`.beads/` exists):

1. Create one round bead: `br create -t chore --labels qa-round
   --description-file <working-file> --external-ref <session-id>`. The working
   file's content becomes the bead's description — this bead is now the
   round's sole durable record, not a second copy of it.
2. Parent every finding filed this round under it — new ones via `--parent
   <round-id>` on `br create`, already-filed ones via `br update <finding-id>
   --parent <round-id>`. Never bead the cases themselves; they aren't
   independently actionable and the plan/results already live in the round
   bead's description.
3. Delete the working file. It was scratch. Do not commit it — a committed
   copy sitting next to the round bead is a second source of truth that will
   drift.

If the project does not track work in beads, closing is still required in
spirit: decide explicitly with the user, in the moment the round ends, whether
the working file is committed or deleted. Do not leave it untracked
indefinitely for someone to puzzle over later.
