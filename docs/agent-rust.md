# Rust policy

Required by [AGENTS.md](../AGENTS.md) for every Rust crate. Read once before
applicable work, together with [testing](agent-testing.md).

### Rust

These rules apply to every Rust crate, including their binaries and tests.
Keep shared policy here; crate-local instructions may add concrete constraints
but must not silently weaken it.

#### Toolchain and lints

- Use stable Rust matching `RUST_VERSION` in CI and default rustfmt. Keep the
  edition explicit in each manifest. Declare an MSRV only when it is tested;
  do not claim compatibility from an untested `rust-version` field.
- Configure package-wide Cargo lints so libraries, binaries, and tests are all
  covered. Deny `clippy::all` and `clippy::pedantic`, with group priority `-1`;
  individual lints keep priority `0`. Also deny `missing_docs`,
  `unsafe_op_in_unsafe_fn`, `clippy::unwrap_used`, `clippy::expect_used`,
  `clippy::panic`, `clippy::todo`, `clippy::unimplemented`, and
  `clippy::undocumented_unsafe_blocks`. The unsafe-code level is specified below.
- Fix lint findings. Never disable `warnings`, `all`, or `pedantic`, or add
  crate/module-wide production suppressions. A narrow exception must name the
  lint and explain why the code is correct and clearer as written. Prefer
  `#[expect(..., reason = "...")]` when the lint is expected to fire; use a
  justified `#[allow]` only when an expectation is inappropriate, such as a
  configuration-specific diagnostic. "Clippy is noisy" is not a justification.
- Do not suppress missing API/error/panic documentation or increase lint
  thresholds to avoid fixing code. Split mixed responsibilities; do not create
  artificial helpers merely to satisfy a length lint. A cohesive function may
  receive a narrow, justified exception under the preceding rule.

#### Unsafe and failure handling

- Forbid `unsafe_code` in capture-service and usage-cli. Deny it by default in skills-core;
  exceptions are limited to reviewed platform operations. Before adding or
  expanding one, record why safe stdlib/existing-dependency APIs do not suffice,
  the alternatives considered, and the evidence supporting the chosen boundary.
  Existing unsafe code receives no automatic exemption.
- Each unsafe operation needs a precise `// SAFETY:` argument covering its
  actual obligations, including descriptor ownership and post-fork restrictions
  where applicable. Keep unsafe blocks minimal and encapsulate them behind a
  safe API that enforces its invariants. Tests supplement the argument; a green
  test or the absence of the keyword does not establish soundness.
- No `static mut`. Raw-pointer operations, manual `Send`/`Sync`, FFI, and
  lifetime manipulation follow the same unsafe exception policy; they are not
  shortcuts around Rust's ownership model.
- Expected input, I/O, configuration, and protocol failures return typed
  `Result` errors. Preserve their causes and sanitize them at presentation
  boundaries. No production `unwrap()`. An invariant-only `expect()` or panic
  needs a narrow lint exception explaining why external input cannot violate
  the invariant; document any exposed panic conditions. Never use assertions
  as external-input validation.
- Assertions, `unwrap()`, `expect()`, and explicit panic branches are permitted
  in tests. Their lint allowances belong only to test functions, `#[cfg(test)]`
  modules, or integration-test crates, with a test-fixture justification. Never
  disable panic/unwrap lints for a library merely because it is built for tests.
  `todo!()` and `unimplemented!()` are forbidden in committed code, including tests.
- Do not swallow errors or substitute defaults for failed validation. Ignoring
  a best-effort cleanup error requires a comment explaining why correctness and
  security remain intact. Cleanup that proves containment or permits identity
  reuse is not best-effort.

#### Ownership, boundaries, and concurrency

- Prefer `&str`, `&[T]`, and `&Path` when ownership is unnecessary. Clone only
  for a concrete ownership requirement; do not add `Arc<Mutex<_>>` to bypass a
  design problem. Use enums for mutually exclusive states and checked
  conversions/arithmetic for external sizes, indices, and identifiers.
- Keep helpers private or `pub(crate)` unless consumers need them. Separate
  policy and state transitions from filesystem, process, and transport effects.
  Use plain functions and existing dependencies before new traits, layers, or
  crates. Do not add dependencies solely to hide an unsafe block.
- Closed record and wire types deny unknown fields. Never combine that with
  `#[serde(flatten)]`: flattening deserializes through a map the deny rejects,
  so the type writes its own output and then refuses to read it back. Nothing
  fails at compile time, and the round trip breaks only at runtime (broker
  audit entries, 2026-09-10). Nest the value under a named field instead.
- Keep Tokio as capture-service's async runtime; do not introduce a second
  runtime or require one in synchronous skills-core code. Blocking filesystem,
  process, and network work must stay off async executor threads. Do not hold
  synchronous locks across `.await`; keep lock scopes short and document lock
  ordering when multiple locks are acquired.
- Release an advisory file lock explicitly; dropping the descriptor is not
  enough. `flock` belongs to the open file description, so a child forked
  before its exec inherits it and holds the lock past the parent's close. The
  failure looks like an unrelated intermittent "would block" in whatever runs
  next, and it has now bitten three sites: `broker/state_identity.rs`,
  `workspace/retention/storage.rs` and `workspace/promotion.rs`
  (louiselm-xx07b). Prove the release with a test that keeps a cloned
  descriptor open, not with timing.
- Spawn processes with argument arrays and explicit environment/cwd where
  relevant. Every task and child process has an owner responsible for completion,
  cancellation, and cleanup. Disposal must handle late callbacks and prove that
  confined descendants are gone before releasing their authority or identity.
- Document exported APIs with Rustdoc, including meaningful `# Errors` and
  `# Panics` sections where applicable. Document ownership, blocking behavior,
  and lifecycle obligations when callers must act on them. `cargo doc` alone
  does not enforce missing documentation; keep the lint enabled.
- Apply [testing](agent-testing.md)'s behavior-first tests to Rust too. Preserve real concurrency,
  failure, and privileged-boundary coverage. A passing unit suite does not
  replace required host conformance tests or maintainer acceptance of the actual
  installed workflow. No mandatory new property-test or benchmark dependency.
