# Rust policy

Required by [AGENTS.md](../AGENTS.md) for Rust code, manifests, dependencies,
API documentation, design or review. Read with [testing](agent-testing.md).

## Toolchain and dependencies

{{confirmed_rust_toolchain_targets_and_package_scope}}

Use a deliberately selected stable toolchain, explicit edition and Cargo.
Declare an MSRV only when it is tested. Keep the toolchain used locally and in
CI consistent. Commit the project's lockfile and update dependencies
deliberately; validation uses `--locked`.

Prefer safe standard-library and existing dependency APIs. A new crate needs a
demonstrated gap; do not introduce an async runtime or error-handling dependency
merely because another project uses one.

## Lints and documentation

Configure package-wide lints, inherited from workspace lints when applicable,
so libraries, binaries and tests are covered. Deny warnings, missing public
documentation, `clippy::all` and `clippy::pedantic` (group priority `-1`).
Deny production `unwrap`, `expect`, panic, `todo!` and `unimplemented!`
through their Clippy lints.

Fix findings rather than weakening groups or raising thresholds. A narrow
exception names the lint and explains why the code is correct and clearer;
prefer `#[expect(..., reason = "...")]` where appropriate. Test-only
assertion/panic allowances stay in test scopes. Never commit placeholder
`todo!` or `unimplemented!`, including in tests.

Document public APIs, with `# Errors` and `# Panics` where applicable.
Document ownership, blocking and lifecycle obligations that affect callers.
`cargo doc` alone does not enforce missing docs; keep the lint enabled.

## Errors, ownership and boundaries

Expected input, configuration and I/O failures return explicit `Result`
errors. Preserve the cause and sanitize at presentation boundaries. Never use
assertions as external-input validation or silently discard a failed result.

Forbid handwritten unsafe code by default. An actual need to change that policy
requires a project decision, alternatives and an explicit invariant at the
smallest boundary. A passing test does not establish soundness.

Use borrowed parameters when ownership is unnecessary. Clone for a concrete
ownership need, not to hide one. Keep APIs private or crate-visible until an
external consumer needs them. Use enums for mutually exclusive states and
checked conversions for externally supplied sizes and identifiers.

Keep protocol/I/O representations at their translation boundaries. Prefer
plain functions and cohesive modules over traits or generics for hypothetical
consumers. Mutable state, tasks and child processes have explicit owners.
For async code, do not block executor threads or hold synchronous locks across
`.await`; cancellation and disposal must release owned resources.

## Quality gates

{{rust_gate_status_and_first_task_prerequisites}}

Default commands, run from the Cargo project/workspace root once implemented:

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps
```

Choose feature/target coverage from this project's supported configurations and
use the same invocations locally and in CI. Do not add `--all-features` without
checking compatibility, or copy another project's fixture-specific flags.
Cargo tests include doc-tests where applicable. Keep focused tests convenient,
but run the complete affected suite before handoff.
