# Testing and acceptance

Required by [AGENTS.md](../AGENTS.md) before executable changes, test/reliability
design, gate changes or CI work. Read once before applicable work.

## Verification status

{{gate_status_by_component_with_evidence_or_first_task_reference}}

Selected language policies own the concrete commands. A planned command is a
contract for implementation, not evidence that a harness, manifest, dependency
or CI job exists. The first implementation task owns wiring the needed gates
alongside the first working behavior. Update status and commands when they
become executable, and record the actual results.

## Development loop

For meaningful behavior changes, use red-green-refactor:

1. Write the smallest test of the intended behavior. Run it and confirm the
   failure is for the expected reason.
2. Make the focused change that passes it.
3. Refactor with the tests green.

For a pure refactor, start from passing characterization coverage. Documentation,
formatting and trivial mechanical changes need no artificial red test. Explain
when test-first work is impractical and what evidence covers the change.

Establish a relevant baseline without disturbing another person's work. Use
the current tree with failures identified, a clean worktree or existing CI
evidence; do not stash unrelated changes to create a baseline.

## Test the behavior

- Assert observable behavior through a public boundary, including meaningful
  failure paths. Assert the intended error, not merely that something failed.
- Keep tests deterministic and independent of live services, credentials and
  personal configuration. Use controlled local fixtures and explicit inputs.
- Control time, randomness, environment and iteration order where the property
  depends on them. Avoid whole-object equality when incidental fields vary.
- Test doubles must preserve the production contract, including async shape,
  scheduling and event order where applicable. Derive external protocol
  fixtures from observed or authoritative examples, documenting their source.
- Test cancellation, cleanup and late work at actual lifecycle boundaries.
  Ensure cleanup still runs after a failed assertion.
- Check defaults and unset cases for optional behavior. Verify that the intended
  consumer actually reaches a new feature; a green isolated unit is insufficient.
- Generators need consumed output and a freshness check. Do not call a generator
  complete solely because its return value passes a unit test.

Avoid tests that mirror implementation details or assert wording without a
behavioral reason. Preserve public behavior, boundary, lifecycle and failure
coverage when refactoring. Removing a test requires identifying the changed
contract or its replacement coverage.

## Gates

Run focused checks while developing, then the complete affected suite and the
selected formatting, static-analysis and documentation gates before handoff.
Use the documented commands consistently in local work and CI when CI exists.
Verify committed dependency resolution; a check must not silently repair a
manifest/lock mismatch.

A newly wired test runner must return failure for a deliberate failing test,
and a check must reject the condition it promises to detect. An empty suite or
a command that hides a child exit status is not a working gate.

Report unavailable checks and pre-existing failures explicitly. Do not fix
another agent's in-flight work as part of verification.

## Acceptance

{{first_milestone_acceptance_and_required_manual_or_integration_checks}}

Define acceptance in observable terms before implementation. Separate automated
gate evidence from any required live/manual acceptance. If the agreed criterion
is a reproduction in the running application, keep the issue open until that
behavior is verified. A syntax check, mock or local fixture proves only the
boundary it exercises.
