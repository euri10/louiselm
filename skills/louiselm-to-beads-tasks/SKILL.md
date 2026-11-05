---
name: louiselm-to-beads-tasks
description: Break a plan, spec, or PRD into independently-grabbable beads tasks using tracer-bullet vertical slices. Use when the user wants to convert a plan into implementation tasks, create execution-ready beads work items, or decompose an epic into actionable tasks.
---

# LouiseLM To Beads Tasks

Convert product/implementation plans into small, independently executable beads tasks.

Use `br` (not legacy `bd`) and always include `--json` for machine-readable output. Resolve actor at runtime:

```bash
ACTOR="${BR_ACTOR:-assistant}"
```

## Quick Start

1. Read the source plan/PRD and identify vertical slices that can ship value independently.
2. Create one beads task per slice with clear acceptance criteria and explicit boundaries.
3. Add dependencies only where truly required.
4. Run `br sync --flush-only` after batch creation/updates.

## Workflow

1. **Ingest context**  
   Use current conversation context, existing epic description, and repository conventions. Ask only focused clarification questions if scope or sequencing is ambiguous.

2. **Derive tracer-bullet slices**  
   Prefer end-to-end thin slices over layered "all backend then all frontend" breakdowns. Each task should be independently grabbable and testable.

3. **Define each task**  
   Every task should include:
   - Problem/goal in one sentence
   - Scope (in and out)
   - Acceptance criteria
   - Test expectations
   - Key constraints/risks

4. **Create tasks with `br`**  
   For each task:

   ```bash
   ACTOR="${BR_ACTOR:-assistant}"
   br create --actor "$ACTOR" "Task title" \
     --type task \
     --priority 2 \
     --labels ready-for-agent \
     --description "$DESCRIPTION" \
     --json
   ```

   Priority guidance: `1` urgent/critical path, `2` normal planned work, `3` opportunistic, `4` backlog/future.

5. **Attach to epic and wire dependencies**  
   - If an epic exists: `br update --actor "$ACTOR" <task-id> --parent <epic-id> --json`
   - For blocking relationships: `br dep add <blocked-id> <blocking-id> --json`
   - Keep dependency chains minimal; maximize parallel-ready tasks.

6. **Sync and report**  
   ```bash
   br sync --flush-only
   ```
   Return created task IDs, titles, and dependency graph summary.

## Task Description Template

```md
## Goal
One-sentence user-visible outcome.

## Scope
- In scope: ...
- Out of scope: ...

## Acceptance Criteria
1. ...
2. ...

## Testing
- Unit/integration expectations
- Relevant regression checks

## Notes
- Constraints, risks, or assumptions
```

## Rules

- Favor 1-3 day tasks over large ambiguous chunks.
- Avoid task overlap; boundaries must be explicit.
- Do not create fake dependencies for organizational convenience.
- Use `ready-for-agent` by default unless blocked; if blocked, label with the blocker (e.g., `needs-clarification`) and state why.
- Never run interactive TUI flows.
