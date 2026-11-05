---
name: louiselm-grill-me
description: Grill the user relentlessly about a plan, decision, or idea while preserving the project's shared vocabulary and routing the confirmed outcome into Beads. Use when the user wants to stress-test their thinking, or uses any 'grill' trigger phrases in the LouiseLM workflow.
---

Interview me relentlessly about every aspect of this until we reach a shared understanding. Walk down each branch of the decision tree, resolving dependencies between decisions one-by-one. For each question, provide your recommended answer.

Ask the questions one at a time, waiting for feedback on each question before continuing. Asking multiple questions at once is bewildering.

If a fact can be found by exploring the environment (filesystem, tools, etc.), look it up rather than asking me. The decisions, though, are mine — put each one to me and wait for my answer.

## Project vocabulary

Before the first semantic question, load the applicable project vocabulary once. In a Beads project, run:

```bash
br list --type vocabulary --status all --format toon
```

Use only records whose issue type is `vocabulary` and whose status is `reference`. The all-status query is intentional: a new project may have no reference records yet, and `br` rejects an undeclared custom status when that is true. If the command reports that Beads is unavailable, continue the grill and say that vocabulary cannot be persisted; never initialize Beads implicitly. Do not scan the backlog, history, or codebase for missing terms.

Treat loaded definitions as compact constraints on meaning, not as a reason to litigate every synonym. If two interpretations would materially change architecture, lifecycle, protocol behavior, authority, or user-visible behavior, stop and ask for clarification. Harmless naming drift may be normalized to the canonical term. If vocabulary, code, an existing work item, or the user's current intent materially contradicts one another, stop the current branch and resolve the conflict with the user; do not choose an authority silently. If the conflict remains unresolved, carry it as a needs-design vocabulary-question candidate for louiselm-to-beads rather than continuing toward implementation.

Keep vocabulary changes provisional during the interview. Do not create or edit vocabulary records while asking questions.

Do not act on it until I confirm we have reached a shared understanding.

When the interview reaches shared understanding, present a compact final summary with a `Vocabulary delta` containing exactly one of:

- `Create`: canonical terms and their definitions, distinctions, and provenance.
- `Update`: terms whose meaning changes, with the semantic change stated.
- `No change`: no vocabulary promotion is needed.

Ask me to confirm that summary. That single confirmation authorizes both vocabulary promotion and any resulting work items. Then invoke the louiselm-to-beads skill with the confirmed summary, even when the result contains vocabulary changes but no implementation work. Do not mutate vocabulary silently or add a second confirmation ceremony.
