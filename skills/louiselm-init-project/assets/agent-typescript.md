# TypeScript policy

Required by [AGENTS.md](../AGENTS.md) for TypeScript code, dependencies, types,
API documentation, design or review. Read with [testing](agent-testing.md).

## Runtime and tooling

{{confirmed_typescript_runtime_targets_and_module_strategy}}

Use npm with a committed `package-lock.json`, TypeScript with `strict: true`,
Biome for formatting/linting and Vitest for tests. Keep these tools in project
development dependencies. Use `npm ci` to install the committed resolution;
do not change package managers or repair the lockfile as a validation side effect.

Choose Node/browser targets and module resolution for the actual project. A
TypeScript choice does not select a UI/server framework or require a browser
test environment. Pin the toolchain deliberately and verify supported runtimes.

## Types, errors and APIs

Treat external values as `unknown` until validated. Model alternatives with
explicit unions and narrow them. Avoid `any`, non-null assertions and casts
that conceal missing validation. Keep any justified suppression local with its
reason; do not weaken strict checking to pass a gate.

Document exported behavior, failure and lifecycle contracts. Keep helpers
private until a consumer needs them. Prefer plain functions and records; add
interfaces, classes and generics for actual boundaries.

Handle expected failures at a boundary that can recover or present them.
Preserve causes internally; never use an empty catch or a default that converts
failure into success. Await or explicitly own promises, including their failure
and cancellation paths. Do not leave floating background work.

## Boundaries and lifecycle

Imports define APIs; explicit startup owns I/O, listeners and timers. Keep
domain transformations separate from DOM, filesystem, process and transport
effects. Mutable state and acquired resources have owners and cleanup.

Honor cancellation where the external API supports it, and prevent late
results from mutating disposed state. Use explicit arguments for processes and
validate content before inserting it at a trust boundary. Keep credentials and
sensitive payloads out of client bundles and logs.

## Quality gates

{{typescript_gate_status_and_first_task_prerequisites}}

The first implementation task creates these package scripts and their configs:

| Script | Default implementation |
| --- | --- |
| `check` | `biome check .` |
| `typecheck` | `tsc --noEmit` |
| `test` | `vitest run` |

Once implemented, run:

```sh
npm ci
npm run check
npm run typecheck
npm test
```

Type checking is a separate gate from executing tests. The test command must
run once, return failures and reject an unintentionally empty suite. Select the
Vitest environment for the code under test; add browser/DOM tooling only when
the behavior requires it. Restore timers, mocks and resources after each case.

Tool references: [npm ci](https://docs.npmjs.com/cli/v11/commands/npm-ci/),
[TypeScript strict](https://www.typescriptlang.org/tsconfig/strict.html),
[Biome](https://biomejs.dev/installation/quick-start/),
[Vitest](https://vitest.dev/guide/).
