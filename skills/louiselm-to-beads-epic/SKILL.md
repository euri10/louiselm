---
name: louiselm-to-beads-epic
description: Turn the current conversation context and codebase understanding into a beads epic. Use when the user wants to convert the current context, plan, feature discussion, or product requirements into an epic in the local beads issue tracker.
---

# LouiseLM To Beads Epic

This skill takes the current conversation context and codebase understanding and creates a beads epic. Do not interview the user from scratch; synthesize what is already known. Ask only targeted confirmation questions when module boundaries, testing expectations, priority, or scope are genuinely ambiguous.

Use the `br` CLI, not legacy `bd`. Always pass `--json` for agent-readable output, resolve the actor at runtime with `ACTOR="${BR_ACTOR:-assistant}"`, and sync explicitly with `br sync --flush-only` after creating or updating beads.

## Process

1. Explore the repo enough to understand the current implementation, if you have not already. Use the project's domain vocabulary throughout the epic, and respect any ADRs or local architecture notes in the area being planned.

2. Sketch the major modules that will need to be built or modified. Actively look for opportunities to extract deep modules that can be tested in isolation.

   A deep module is one that encapsulates substantial behavior behind a small, stable, testable interface.

3. Check with the user that these modules match their expectations. Also confirm which modules they expect to be covered by tests. Keep this confirmation concise; do not restart discovery unless the user changes the scope.

4. Draft the epic description using the template below. Prefer a clear implementation plan over file-path-heavy detail. Do not include specific file paths or code snippets unless a prototype produced a compact shape that records an important decision more precisely than prose can.

5. Create the bead as an epic:

   ```bash
   ACTOR="${BR_ACTOR:-assistant}"
   br create --actor "$ACTOR" "Epic title" --priority 1 --type epic --labels ready-for-agent --description "$DESCRIPTION" --json
   br sync --flush-only
   ```

   Choose the priority from context. Use `1` for important near-term work, `2` for normal planned work, and `4` for future/backlog ideas. Apply `ready-for-agent` unless the user explicitly says the epic is blocked or still exploratory.

6. Report the created bead ID and title to the user. If child beads are obvious and the user asked for them, create them as `task`, `feature`, `bug`, `docs`, or `question` beads and link each child to the epic with `br update --actor "$ACTOR" <child-id> --parent <epic-id> --json`.

## Epic Template

```md
## Problem Statement

The problem the user is facing, from the user's perspective.

## Solution

The solution to the problem, from the user's perspective.

## User Stories

A long, numbered list of user stories. Each user story should use:

1. As an <actor>, I want a <feature>, so that <benefit>

Cover the full feature, including edge cases, operations, and failure modes when relevant.

## Implementation Decisions

A list of decisions that were made. Include:

- Modules that will be built or modified
- Interfaces that will change
- Technical clarifications from the developer
- Architectural decisions
- Schema changes
- API contracts
- Specific interactions

## Testing Decisions

A list of testing decisions that were made. Include:

- What makes a good test for this feature
- Which modules will be tested
- Prior art for similar tests in the codebase
- Property, integration, or benchmark coverage when appropriate

## Out of Scope

Things explicitly excluded from this epic.

## Further Notes

Any remaining context, risks, or follow-up questions.
```

## Beads Notes

- Keep the epic description durable. Avoid details likely to become stale, especially exact file paths, line numbers, or temporary prototype code.
- Use labels sparingly. `ready-for-agent` is enough for a ready implementation epic unless project conventions require more.
- If the epic is not ready, use a label such as `needs-design` or `needs-clarification` instead of `ready-for-agent`, and explain the blocker in the description.
- Never run bare interactive TUI tools while using this skill.
