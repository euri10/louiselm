---
name: louiselm-to-beads
description: Route conversations, plans, and requirements into beads work items at the appropriate abstraction level. Dispatches to louiselm-to-beads-epic, louiselm-to-beads-feature, or louiselm-to-beads-tasks based on input complexity. Use when you want to convert a louiselm-grill-me session, plan discussion, PRD, or feature context into actionable beads work (epic/feature/task/bug).
---

# LouiseLM To Beads: Route Context → Beads Work Items

Convert planning conversations and requirements into properly-scoped beads work items that stay within a single model's context window.

## Quick start

When you have a plan or conversation you want to convert to beads:

1. **Identify the input type:**
   - Rough idea / high-level vision → **epic**
   - Focused feature discussion / louiselm-grill-me output → **feature**
   - Detailed plan / spec with clear steps → **tasks**
   - Edge case / defect / repair → **bug**

2. **Route to the appropriate skill:**
   - `louiselm-to-beads-epic`: Multi-phase initiatives, architectural changes, major features
   - `louiselm-to-beads-feature`: Single-focused capability, narrow scope, grill-tested idea
   - `louiselm-to-beads-tasks`: Vertically-sliced, independently-grabbable work (tracer bullets)
   - `louiselm-to-beads-bug`: A defect with evidence — filed before it is fixed, closed with the fix

3. **Verify grabbability:** Output tasks must be:
   - Solvable by a low-context model (Haiku tier)
   - Completable without context-window exhaustion
   - Ideally ≤ 5-10 files, ≤ 100 LOC changes each, not a hard requirement, adapt to the case

## Workflows

### When to use each routing

**Epic** (louiselm-to-beads-epic):

- Multi-month initiative or major system redesign
- Multiple independent features feeding into one goal
- Unclear decomposition—needs planning first
- Example: "Rewrite auth layer to support federated identity"

**Feature** (louiselm-to-beads-feature):

- Single, well-scoped capability
- Grill-tested idea with clear requirements
- Fits in one sprint / < 2 weeks
- Example: "Add dark mode toggle to settings"

**Tasks** (louiselm-to-beads-tasks):

- Detailed spec or plan with numbered steps
- Ready to implement—decomposition done
- Each task is a vertical slice (one file / feature pair)
- Example: "Implement user settings storage, add UI toggle, wire up persistence"

### Dispatch checklist

- [ ] Input is a conversation, plan, or requirements context
- [ ] Assess: Is this a rough idea (epic) or a defined scope (feature/tasks)?
- [ ] If epic-scale, call louiselm-to-beads-epic
- [ ] If feature-scale, call louiselm-to-beads-feature
- [ ] If detailed plan, call louiselm-to-beads-tasks
- [ ] Verify final tasks avoid the "dumb zone"—can one model finish it?

## Grabbability rules

Tasks exit the "dumb zone" when:

- ✅ One model holds full context (files touched, test strategy, edge cases)
- ✅ No hidden dependencies on unreviewed prior work
- ✅ Clear acceptance criteria (test passes, feature works, lint clean)
- ✅ Minimal context pollution—avoid "read 50 files to understand intent"

**Red flags:**

- ❌ Task mentions "see the other half in bug #123"
- ❌ "Understand the auth flow first" (vague prereq)
- ❌ Task touches 20+ files or requires global architectural knowledge
- ❌ Acceptance criteria is "it should be better somehow"

## Vocabulary promotion

When converting a semantic plan, load the applicable project vocabulary once before drafting work. In a Beads project, use the empty-safe query:

```bash
br list --type vocabulary --status all --format toon
```

Consume only records whose issue type is `vocabulary` and whose status is `reference`. Do not scan for missing terms. If there is no Beads database, report that vocabulary and work items cannot be persisted and do not initialize Beads implicitly.

When the confirmed source context includes a `Vocabulary delta`, promote it in the same confirmed operation as the work items:

- Create or update one `vocabulary` record per canonical term with status `reference`.
- Store the term in the title, its concise conceptual definition in the description, usage distinctions in notes, and provenance in design.
- Re-read the affected records immediately before promotion. If an existing record changed since it was loaded, stop and reconcile the conflict through a `question` bead labeled `needs-design`; never overwrite a concurrent semantic update blindly.
- Promote vocabulary even when the plan produces no implementation issue.
- Do not create or edit vocabulary records before the user confirms the final shared-understanding summary.

The vocabulary record is canonical Beads state, not a second Markdown glossary. Keep definitions short and conceptual so loading them remains cheap and so the reference records remain outside actionable work scheduling.

## Advanced features

See the individual skill docs:

- [louiselm-to-beads-epic](../louiselm-to-beads-epic/SKILL.md) — for multi-phase work
- [louiselm-to-beads-feature](../louiselm-to-beads-feature/SKILL.md) — for scoped capabilities
- [louiselm-to-beads-tasks](../louiselm-to-beads-tasks/SKILL.md) — for implementation decomposition
- [louiselm-to-beads-bug](../louiselm-to-beads-bug/SKILL.md) — for defects, with the evidence that proves them
- [beads CLI (`br`)](/skills/br) — manage beads, track status, sync to git
