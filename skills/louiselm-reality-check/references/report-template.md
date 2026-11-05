# Reality check report template

Use this structure for the chat assessment. It does not require a file. Bridge
findings and plans belong in self-contained Beads. Only when the user requests a
saved report or project instructions require one, keep one file per run and
revise it in place. Cite evidence instead of narrating it; keep the report short.

```md
# Reality check: <project>, <YYYY-MM-DD>

Mode: <quick | full | bridge | delta> (+ focus). Audit boundary: `<full sha>` on
`<branch>`; dirty paths left alone: <paths | none>; Beads at <time>: <open> open,
<in progress> in progress, <closed> closed. Code audit by: <independent read-only
agents | this session>. Previous audit: <chat/Bead/report reference | none>.
Phase 1 made no code or tracker changes.

## Bottom line

<One honest paragraph: does the project deliver its vision today, what truly
works, the central problem, and the single most useful next move.>

## Where we really are

1. What specifically IS working right now? <...>
2. What is NOT working or not yet implemented? <...>
3. What is blocking us from getting there? <...>
4. If we implemented all open and in-progress Beads, would we close the gap
   completely? <Yes | No>, because <...>.
5. What goals from the vision are NOT covered by ANY live Bead? <gap ids | none>

## What actually runs

<The traced shipped path, entry point -> ... -> result, with `file:line`; say
plainly whether it is real or mocked. Commands run this session and what they
returned. What was not run, and why.>

## Vision checklist

| # | Promise | Source | Status | Evidence | Beads |
|---|---------|--------|--------|----------|-------|
| 1 | <testable promise> | `README.md:12` | WORKING | `<test, gate, or command>` | closed `<id>` |
| 2 | <testable promise> | plan §3 | UNWIRED | `<module>` has no production caller | `<live id>` |

## Gaps (by vision impact)

### G1. <plain title> (<type>, <critical | major | minor>)

<What is missing, the evidence (`file:line`, command output), and why it matters
to the vision.> Coverage: <COVERED ids | THIN: why | NO_BEAD | DECISION: what>.

## Tracker drift

<Closed Beads whose behavior is absent; open Beads that already ship;
duplicates. Or: none found.>

## Next moves

1. <Highest leverage first; mark each "agent" or "maintainer decision".>

## Bead delta (proposed | applied)

- Create: <title> (<type>, P<n>) for G<n>; blocks <id>
- Update: <id>: <change>
- Reopen: <id>: <acceptance text it fails>; <`file:line` evidence>
- Close: <id>: <verdict and evidence>

Or: "None. Live Beads already cover the vision; what is missing is <...>."

## Bridge plan (Phase 2 only)

### G1. <title>: <STATUS> -> WORKING

- Now: <current state>
- Target: <the promise, at the boundary the user sees>
- Success: <unit and end-to-end checks with useful logs; one negative check
  that fails against a stub or mock>
- Plan: <the change>
- Depends on: <ids or gaps> · Size: <S | M | L | XL>
- Live Beads would close it: <yes: ids | no: why>

Critical path to the first end-to-end result: <milestones, each with its check>.
Early value: <a slice that ships before the whole path, if any>.
Parallel lanes: <lane: Beads in order; files the lane owns>.
Non-goals: <...>.
Ambition rounds: <round n: what changed; what was rejected and why>.
Refinement rounds: <round n: what changed; the round that found nothing>.

## Not checked

<Everything this run did not verify, so the next run starts there.>
```

## Bridge Bead description

Route creation through `louiselm-to-beads`. Each bridge Bead must contain its
audit boundary, evidence and plan without depending on chat or a report file:

```md
## Why

Reality check <YYYY-MM-DD> at `<full sha>`: <the gap in one sentence>.
Evidence: <`file:line`, command and result>.

## Contract

1. <the behavior that must exist at the boundary the user sees>
2. <constraints: memory, safety, protocol, compatibility>

## Proof

<What closes it: the production consumer or gate it creates. An `inert:` close
moves this gap; it does not close it.>
Negative check: <a test that fails if the behavior is stubbed or mocked>.

## Tests

- Unit: <...>
- End to end: <through the shipped entry point, with logs that explain a failure>

## Scope

- In: <...>
- Out: <...>
```
