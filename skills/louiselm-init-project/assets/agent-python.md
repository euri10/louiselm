# Python policy

Required by [AGENTS.md](../AGENTS.md) for Python code, dependencies, typing,
API documentation, design or review. Read with [testing](agent-testing.md).

## Runtime and tooling

{{confirmed_python_version_and_package_scope}}

Use uv for project environments/dependencies, Ruff for formatting and linting,
ty for type checking and pytest for tests. Keep configuration in
`pyproject.toml` where supported and commit `uv.lock`. The first
implementation task supplies the manifest, selected development dependencies
and lockfile; documentation alone does not install them.

Run tools from the project environment using `uv run --locked`. A manifest/lock
mismatch must fail validation instead of silently updating resolution. Declare
the supported Python version deliberately and test any claimed version range.
Prefer the standard library and existing dependencies for concrete needs.

## Types, errors and APIs

Type public functions and important boundary values. Use precise types;
untrusted data starts unvalidated and must be checked before becoming domain
state. Do not use `Any`, casts or broad ignores to silence actual uncertainty.
Keep narrow typing/lint exceptions local and explain the invariant they rely on.

Document public behavior, inputs, return values, raised exceptions and resource
obligations. Let inference describe obvious locals. Use ordinary functions and
data structures; add classes or abstractions for real state or reuse.

Raise specific exceptions for expected failures and preserve causes when
translating them. Catch only where the caller can recover or add useful context.
Do not use bare `except`, swallow failures, or replace invalid input with
successful-looking defaults. Assertions express internal invariants and tests,
not external-input validation.

## Boundaries and lifecycle

Imports define APIs; explicit entrypoints own filesystem, network and process
effects. Avoid mutable default arguments and hidden shared state. Use context
managers for acquired resources and explicit cancellation/cleanup for background
work. Keep blocking work off async loops when async is actually needed.

Use subprocess argument lists, explicit cwd/environment and checked results.
Avoid evaluating code from external input. Keep credentials and sensitive
values out of logging and exception presentation.

## Quality gates

{{python_gate_status_and_first_task_prerequisites}}

Default commands, once the project environment and test suite exist:

```sh
uv run --locked ruff format --check .
uv run --locked ruff check .
uv run --locked ty check
uv run --locked pytest
```

Use pytest fixtures to own temporary files, environment and cleanup. Test
observable outcomes and specific error paths; patch at external boundaries.
Do not turn “no tests collected” into a successful gate or require network
services/credentials for the normal suite.

Tool references: [uv projects](https://docs.astral.sh/uv/guides/projects/),
[Ruff](https://docs.astral.sh/ruff/),
[ty checking](https://docs.astral.sh/ty/type-checking/),
[pytest](https://docs.pytest.org/en/stable/getting-started.html).
