---
name: louiselm-reality-check
description: Step back from closing Beads and check whether the project actually delivers what its README and plan promise. Lists testable promises, checks each against code and a real run, asks whether finishing every open Bead would deliver the vision, then bridges the gaps with Beads. Use when asked "where are we", "reality check", "what's missing", "are we on track", "gap analysis", "does this actually work", or when work feels busy but stalled.
---

# LouiseLM reality check

After days of agents closing Beads, motion is easy to mistake for progress. Answer
one question honestly: does the sum of the work deliver the stated vision? If not,
what is missing, does a live Bead cover it, and who must act? Code and running
software are the ground truth; the README and plan are the measuring stick; the
Beads graph is the plan of record. Words: [words.md](references/words.md).

Return the assessment in chat by default. In bridge mode, persist actionable
findings, evidence and repair plans in the relevant Beads. Save a separate report
only when the user requests one or an applicable project instruction requires
it; an existing reports directory alone is not a requirement. Beads must remain
self-contained even when a report is requested.

## Modes

Default to `full`. `quick` answers the five questions (step 7) in one reply.
`full` runs Phase 1 and stops with a proposed Bead delta. `bridge` runs both
phases; use it when the request already asks to fix, bridge, or file the gaps.
`delta` compares with the previous audit's recorded SHA, from chat, Beads or a
requested report (`bvr --robot-diff --diff-since <sha>`): report what moved,
without re-arguing closed decisions. Any mode may
focus on `integration`, `tracker`, `docs`, or `narrow` (one subsystem or claim).

## Phase 1: assess (no code or tracker changes)

1. **Fix the audit boundary**: date, full `HEAD` sha, branch, dirty paths (leave
   them alone), `br stats` totals, and any previous audit reference. Claims are
   about these bytes; a saved report is not a prerequisite.
2. **Read what was promised**: the project contract (`AGENTS.md` and its routed
   references), README, plan and spec documents, open epics. Load the project
   vocabulary once (`br list --type vocabulary --status all --format toon`).
3. **Write the vision checklist**: 8–25 numbered, testable promises, each with its
   source (`file:line` or section). A README written about the future still
   promises. Run `br search -a` before calling a promise a gap: a closed decision
   may have dropped it, which makes it a docs gap.
4. **Check reality, not reports.** Ignore Bead status, commit messages and agent
   summaries. Trace each promise from the shipped entry point to its code; hunt
   stubs, mocks and refusals on production paths; find the test or gate that
   would fail if it broke, minding what is [not evidence](references/words.md#not-evidence);
   run the shipped thing on real input when that is safe and cheap (never
   destructive, paid or publishing actions). Where closes carry typed verdicts,
   spot-check that `consumer:` references still reach, `gate:` checks still run,
   and `inert:` follow-ups still exist. Prefer independent read-only agents for
   this audit, given the checklist and code but not the Beads' claims; otherwise
   say the audit was self-performed.
5. **Classify** each promise and gap, then find the broken joints: the few missing
   links that stop the core value flowing end to end. Name the one end-to-end
   check that would prove the core promise through the shipped entry point. If it
   is missing or failing, that is the top gap.
6. **Check coverage.** For each gap and for the whole: "would finishing the live
   Beads close it?" Only open, in-progress or blocked Beads count; a closed Bead
   never covers a gap. Use `bvr --robot-search --search "<promise>"`, bounded
   `br search`, `br show`, `bvr --robot-insights` and `--robot-blocker-chain
   <id>`. Record tracker drift: closed Beads whose behavior is absent, open Beads
   that already ship (`br orphans`), duplicates.
7. **Report to the requested destination and stop**, using chat by default and
   the [template](references/report-template.md). For a saved report, give a
   concise chat handoff instead of duplicating it. The assessment
   answers the five questions: What specifically IS working right now? What is
   NOT working or not yet implemented? What is blocking us from getting there?
   If we implemented all open and in-progress Beads, would we close the gap
   completely, and why or why not? What goals from the vision are NOT covered by
   ANY live Bead? Then rank next moves by leverage (wiring built-but-unwired code
   usually leads), marked agent work or maintainer decision, and propose a Bead
   delta. One confirmation of the delta authorizes Phase 2; `bridge` mode skips
   the question. If live Beads already cover the vision, propose none.

## Phase 2: bridge (changes Beads)

8. **Resolve the actor** by the project's rules before any `br` mutation, such as
   `claude/<CLAUDE_CODE_SESSION_ID>` or `session.identity()` in a LouiseLM
   Session. A username, PID, old record or `assistant` is not attribution: ask.
9. **Prepare the bridge plan for Beads.** Draft it in the conversation, then
   retain the useful evidence and plan in the relevant issue descriptions or
   comments in step 11. Keep Beads canonical even when a separate report is
   requested; do not create an audit-only issue. Order gaps by vision impact,
   not ease. Per
   gap: current state, target, success criteria including a negative
   check that fails against a stub, dependencies, size (S–XL), and whether live
   Beads would close it. Then the critical path to the first end-to-end result, an
   earlier slice that ships value if one exists, parallel lanes on disjoint files,
   and non-goals. Without a Beads database, return the plan in chat; never
   initialize one. If a separate report is requested or required, use the
   requested location or the project's report directory, otherwise
   `docs/reality-check/<YYYY-MM-DD>.md`; revise that one file in place.
10. **Push ambition, 2 rounds (3 at most)**, with the [phrases](references/prompts.md):
    revise the same plan in place, keep only additions that clearly serve a
    promise, and retain consequential rejected options with their reason in the
    relevant Bead. A bolder approach is yours
    to propose; changing what the project promises is the maintainer's call, so
    present it or file a `needs-design` "Grill-me needed: ..." question.
11. **Write the Beads** through `louiselm-to-beads` with the step 8 actor (never
    its `assistant` fallback), labeled `reality-check`, using the template's
    bridge Bead content. Fix the mechanism, not every symptom: few Beads beat
    many. Update an existing owner instead of duplicating it. Each Bead blocks
    the work it unblocks; keep one dependency direction and `br dep cycles`
    empty. Reopen a closed Bead only when code contradicts its own acceptance
    text: comment that text and the `file:line` evidence, keep the original
    criteria. Close an already-shipped Bead only with the project's verdict and
    evidence. Never touch another agent's live claim.
12. **Refine in plan space until a round finds nothing**, at most 5 rounds: does
    each new or changed Bead make sense, is it optimal, does it stand alone, does
    it name unit and end-to-end tests with useful logs, could `bvr --robot-plan`
    find more parallel work? If round 5 still finds problems, report them.
13. **Hand off**: summarize the applied Bead delta in chat, flush Beads the
    project's way (for example `br sync --flush-only --actor "<actor>" --json`),
    and report Bead IDs, dependencies and open maintainer decisions. Update a
    separate report only if one was requested or required. Implement or commit
    only on request.

## Rules

1. **Promises, not vibes.** Judge code against the numbered checklist, or stubs
   pass as progress.
2. **Code is ground truth.** Status comes from reading and running code; Bead
   counts are not delivery (the "Bead completion illusion").
3. **Coverage is the key question.** Hunt `NO_BEAD` gaps on purpose: nobody will
   close them otherwise.
4. **Run the shipped thing.** Tests prove parts; only the shipped entry point
   proves the product. Say what you did not run.
5. **Beads before code, one plan.** Never jump from report to implementation;
   revise one plan in place, never fork it.

**No ceremony.** A gap is a promised capability that does not work for its user.
A missing certificate, receipt, ledger row, sign-off or audit document is not a
gap unless a promise depends on it. Never create or reopen Beads for paperwork,
and keep the report short. Every bridge Bead must change, or unblock, behavior
someone will use; if none does, the honest output is the verdict alone.
