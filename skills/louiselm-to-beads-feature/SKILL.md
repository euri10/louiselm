---
name: louiselm-to-beads-feature
description: Turn the current conversation context (especially a louiselm-grill-me session) into a beads feature. Use when the user wants to convert a feature discussion, louiselm-grill-me conversation, or focused product requirement into a feature in the local beads issue tracker. Features sit below epics and above tasks in the hierarchy.
---

# LouiseLM To Beads Feature

This skill takes the current conversation context — typically the output of a louiselm-grill-me session — and creates a beads feature bead. Do not restart discovery from scratch; synthesize what is already known. Ask only targeted confirmation questions when scope, acceptance criteria, or parent epic linkage are genuinely ambiguous.

Use the `br` CLI, not legacy `bd`. Always pass `--json` for agent-readable output, resolve the actor at runtime with `ACTOR="${BR_ACTOR:-assistant}"`, and sync explicitly with `br sync --flush-only` after creating or updating beads.

## Process

1. Explore the repo enough to understand the current implementation in the relevant area, if you have not already. Use the project's domain vocabulary throughout the feature description.

2. Identify the concrete, user-visible outcome this feature delivers. A feature should be narrower than an epic — it covers one coherent capability, not a whole product area.

3. Check with the user:
   - Does the feature belong to an existing epic? If so, get the epic ID.
   - What is the expected priority?
   - Are there obvious tasks already known?
   Keep this confirmation concise; do not restart discovery unless the user changes the scope.

4. Draft the feature description using the template below. Focus on user-visible behavior and acceptance criteria over implementation internals.

5. Create the bead as a feature:

   ```bash
   ACTOR="${BR_ACTOR:-assistant}"
   br create --actor "$ACTOR" "Feature title" --priority 2 --type feature --labels ready-for-agent --description "$DESCRIPTION" --json
   br sync --flush-only
   ```

   Priority guidance: `1` urgent/critical path, `2` normal planned work, `3` opportunistic, `4` backlog/future.
   Apply `ready-for-agent` unless the feature is blocked or still exploratory; use `needs-design` or `needs-clarification` otherwise.

6. If the feature belongs to an epic, link it:

   ```bash
   br update --actor "$ACTOR" <feature-id> --parent <epic-id> --json
   ```

7. Report the created bead ID and title to the user. If child tasks are obvious and the user asked for them, offer to run the `louiselm-to-beads-tasks` skill next to decompose this feature into tasks.

## Feature Template

```md
## Problem Statement

The specific user problem this feature solves.

## Solution

What the feature does, from the user's perspective. One focused capability.

## User Stories

A numbered list of user stories scoped to this feature:

1. As a <actor>, I want <feature>, so that <benefit>

Cover the core flow and the most important edge cases. Keep it tighter than an epic — if the list grows past ~6 stories, consider splitting the feature.

## Acceptance Criteria

Concrete, testable conditions that must be true for this feature to be considered done:

1. Given <context>, when <action>, then <outcome>

## Implementation Notes

- Modules or files likely affected
- Key interfaces that change
- Important constraints or risks
- Anything non-obvious from the louiselm-grill-me conversation

## Testing Notes

- What makes a good test for this feature
- Which behavior must be covered by tests
- Any existing test patterns to follow

## Out of Scope

Things explicitly excluded from this feature.
```

## Beads Notes

- A feature is one coherent capability below an epic. If the scope feels like multiple epics, use `louiselm-to-beads-epic` instead.
- Keep the description durable. Avoid exact file paths, line numbers, or transient prototype details.
- Use labels sparingly. `ready-for-agent` is enough for a ready feature.
- After creating the feature, suggest running `louiselm-to-beads-tasks` to decompose it into individually executable tasks.
- Never run bare interactive TUI tools while using this skill.
