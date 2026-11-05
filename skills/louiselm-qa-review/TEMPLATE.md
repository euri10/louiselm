# Working file template

Write this to the project root (or wherever the user reads it easily) as
`QA_REVIEW.md`. It replaces re-explaining steps in chat: they open it, you
edit it. Keep it untracked — it is scratch, live only for the duration of the
round. Per the skill's closing step: if the project tracks work in beads, the
file's final content becomes a round bead's description and the file is then
deleted, never committed; otherwise, decide explicitly with the user whether
to commit or delete it when the round ends.

---

````md
# Manual QA review — <subject>

Working file for the current round. You run the steps, I edit this file as we
learn. Untracked on purpose; tell me to commit or delete it when we are done.

**Build under test:** <commits>. **Re-run Setup after any fix.**

Origin: <the bug or change that started this, one paragraph>

## Status

| # | Case | State |
|---|------|-------|
| 1 | <behavior> | not run / **passed** — <evidence> / **blocked** — see F1 |

## Setup

<how to get the build under test actually running: rebuild, reinstall, restart,
and a command that proves the running copy matches the source>

<if the subject is driven by an agent/service process with its own
approval or privilege mode (e.g. a permission-prompt gate), state which mode
the session must run in and how to confirm it — a debug/skip-approval launch
flag makes the gated cases silently untestable, with no signal in the UI that
anything is wrong. Say which cases require it.>

<fixture creation, reset between runs>

## Ground rules learned the hard way

<Append every time a case misses for an environmental reason rather than a code
reason. This section is the most valuable part of the file by the end of a
round — it stops the same misfire recurring.>

## Cases

### 1. <behavior>

Exercises `<commit>` / `<file:line>`.

Steps:

> <exact prompt or keystrokes>

Expect: <observable result, including where focus/state ends up>

Failure signature: <what the bug looks like, so a pass is falsifiable>

## Evidence check

<one command that prints a verdict, plus what the pass condition is and what the
command cannot prove>

## Cleanup when the round closes

<fixtures to delete, state to revert, rules to revoke>

## Reporting back

Send me the run id, which case, what you saw, and anything surprising. I pull
the evidence myself. If something is wrong I will fix it, and you will need to
redo Setup before retesting.

## Findings this round

**F1 — <one line>** (`<ticket>`, fixed in `<commit>` / not fixed).
<what broke, the root cause with file:line, and how it was found.>
````

---

## Notes on using it

- Status values worth distinguishing: `not run`, `passed`, `blocked on F<n>`,
  `retest` (fixed since last attempt).
- Put the pass evidence in the table cell, not just "passed" — it is the record.
- When a case is blocked by a finding, say so in the table so the user does not
  burn a run on it.
- Keep failure signatures even after a case passes; they are the regression
  description if it ever comes back.
