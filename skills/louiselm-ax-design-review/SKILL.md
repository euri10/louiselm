---
name: louiselm-ax-design-review
description: Rework an existing project plan or design so it is maximally legible, controllable, efficient, and ergonomic for an AI coding agent that will implement and operate it.
---

# Agent-System Design Review

Use this skill **after an initial architecture, implementation plan, or design document already exists** and before substantial implementation begins.

The goal is not to add complexity for its own sake. The goal is to redesign the project as a coherent system that an AI coding agent can understand, navigate, modify, test, debug, and operate accurately with minimal wasted context, tool calls, retries, and human intervention.

## Objective

Treat the system as if **you are the primary engineer who must implement and operate it through tools**.

Ask:

> What structure, interfaces, state, documentation, observability, invariants, commands, and feedback loops would let an agent understand the current situation quickly, choose the right action confidently, make changes safely, and verify results cheaply?

Then update the project design and plans accordingly.

## Procedure

### 1. Reconstruct the system model

Read the existing design, requirements, repository structure, constraints, and implementation plan.

Build a compact mental model of:

- system boundaries
- major components
- data and control flow
- persistent state
- external dependencies
- public interfaces
- failure modes
- security boundaries
- operational lifecycle
- tests and verification paths

Identify ambiguity, duplicated concepts, hidden coupling, implicit state, and places where the implementation plan does not match the architecture.

Do not begin redesigning until the whole system is understood well enough to explain how a typical request flows end-to-end.

### 2. Review from the agent's point of view

For every important operation, ask:

- How does an agent discover that this capability exists?
- What is the smallest amount of context needed to use it correctly?
- Is there one canonical way to perform the operation?
- Are inputs and outputs structured and machine-readable?
- Can the agent inspect state before changing it?
- Can it predict the effects of a change?
- Can it verify success immediately afterward?
- Are errors specific, local, and actionable?
- Can failed actions be retried safely?
- Can destructive actions be previewed or scoped?
- Can the relevant implementation be located quickly?
- Does the system expose unnecessary choices or incidental complexity?

Prefer designs where the correct action is obvious from local context.

### 3. Improve agent legibility

Make the architecture easy to infer from the repository and APIs.

Prefer:

- explicit module boundaries
- narrow interfaces
- consistent naming
- stable schemas
- typed data
- deterministic commands
- discoverable entry points
- colocated docs and tests
- clear ownership of state
- one source of truth for each concept
- concise machine-readable metadata where useful

Avoid:

- magic behavior
- implicit global state
- duplicated configuration
- hidden side effects
- context-dependent commands
- multiple equivalent APIs
- large files containing unrelated responsibilities
- abstractions whose purpose is not visible from their interface

### 4. Improve agent control surfaces

Important system actions should have small, composable, inspectable interfaces.

Where appropriate, provide operations analogous to:

- `list`
- `get`
- `status`
- `plan`
- `apply`
- `validate`
- `diff`
- `test`
- `health`
- `logs`

A mutating operation should ideally support this loop:

1. inspect current state
2. formulate intended change
3. preview or validate
4. apply
5. inspect resulting state
6. run a focused verification

Prefer idempotent operations and explicit identifiers.

### 5. Improve feedback and observability

An agent should not need to infer whether an action worked.

Design for:

- concise error messages
- stable exit codes
- structured output when useful
- useful logs with correlation/context
- health/status commands
- validation commands
- fast focused tests
- clear distinction between warnings and failures
- enough state inspection to diagnose failures without reading the entire codebase

Errors should state:

- what failed
- where it failed
- the relevant identifier or input
- why it failed when known
- the next useful action when obvious

### 6. Minimize agent resource expenditure

Optimize for fewer tokens, file reads, shell commands, retries, and broad searches.

Prefer:

- short authoritative design docs over duplicated prose
- local README/AGENTS files at meaningful boundaries
- generated indexes rather than manually maintained inventories
- commands that return exactly the information needed
- narrow tests instead of always running the full suite
- stable file locations
- small modules
- strongly typed contracts
- explicit dependency direction

Do not add an abstraction unless it reduces total cognitive or operational cost.

### 7. Strengthen system coherence

Review the project as a **single synthetic system**, not a bag of components.

Check that:

- abstractions form clear layers
- dependency direction is intentional
- each layer exposes what the next layer actually needs
- terminology is consistent across code, docs, CLI, API, and tests
- state transitions are explicit
- cross-component invariants are documented and testable
- the same concept is not modeled differently in different subsystems
- operational tooling reflects the real architecture

When two abstractions overlap, either clarify their boundary or merge them.

### 8. Design for safe autonomous work

Where agents may modify or operate the system, add guardrails that improve correctness without requiring constant human approval.

Prefer:

- dry-run / preview modes
- bounded operations
- validation before mutation
- transactional updates where practical
- reversible changes
- idempotency
- explicit confirmation only for genuinely destructive actions
- invariant checks
- deterministic test fixtures
- machine-readable diagnostics

Do not make routine safe operations unnecessarily interactive.

### 9. Revisit the implementation plan

Update the plan so implementation order follows architectural dependencies and enables early verification.

Prefer milestones that produce complete vertical slices or usable control surfaces.

Each significant task should make clear:

- purpose
- affected components
- dependencies
- expected artifacts
- acceptance criteria
- verification command or method

Split tasks that require too much unrelated context at once.

### 10. Update the design artifacts

Do not merely provide recommendations.

Modify the relevant design documents, architecture notes, implementation plan, API/CLI specifications, schemas, or task breakdowns so the improved design becomes the new canonical plan.

Preserve existing decisions that remain sound.

Avoid wholesale rewrites when a smaller structural change is sufficient.

## Required review checklist

Before finishing, explicitly verify:

### Architecture

- [ ] system boundaries are clear
- [ ] dependency direction is clear
- [ ] abstractions do not substantially overlap
- [ ] state ownership is explicit
- [ ] critical invariants are identified

### Agent ergonomics

- [ ] important capabilities are discoverable
- [ ] common operations have canonical paths
- [ ] interfaces are narrow and structured
- [ ] state can be inspected before mutation
- [ ] outcomes can be verified afterward
- [ ] errors are actionable
- [ ] destructive operations are bounded or previewable

### Repository ergonomics

- [ ] important code is easy to locate
- [ ] naming is consistent
- [ ] configuration has clear ownership
- [ ] relevant docs are close to the code or indexed clearly
- [ ] tests map cleanly to system behavior

### Efficiency

- [ ] routine tasks do not require reading unrelated files
- [ ] common verification is fast and focused
- [ ] unnecessary tool calls and context expansion are minimized
- [ ] no new abstraction adds more cognitive cost than it removes

### Plan quality

- [ ] implementation order follows dependencies
- [ ] tasks have acceptance criteria
- [ ] tasks include verification steps
- [ ] tasks are small enough for a coding agent to execute reliably
- [ ] the plan reflects the architecture exactly

## Output

When this skill is invoked:

1. Briefly state the largest agent-ergonomics problems found.
2. Describe the architectural changes that materially improve the system.
3. Update the actual design/plan artifacts.
4. Summarize what changed and why.
5. Call out any unresolved design decisions that genuinely require user input.

Do **not** produce pages of speculative improvements when the existing design is already good.

Do **not** optimize for "AI friendliness" at the expense of human readability, correctness, security, or maintainability. Good agent ergonomics should normally improve those properties as well.

## Guiding principle

The ideal system makes the correct next action easy to discover, cheap to perform, safe to apply, and obvious to verify.
