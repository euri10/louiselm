# louiselm.nvim Development Contract

## 1. Project Scope

louiselm.nvim is an early-stage Neovim plugin with no external users. Breaking
changes are acceptable. Change the code directly; do not add compatibility
shims, deprecated aliases, or migration paths unless explicitly requested.

Priority: **correctness (including safety and security) → clarity → simplicity →
performance**.

Fix root causes at the narrowest shared boundary. Prefer deletion, existing
code, Lua/Neovim facilities, and plain functions over new abstractions. Do not
build extension points for hypothetical consumers. The established extension
boundaries are the headless session API, typed events, permission policies, and
agent configuration.

## 2. Runtime and Dependencies

- Target the latest stable Neovim at release and its embedded LuaJIT/Lua 5.1.
  Do not support standalone Lua or add Lua 5.2+ syntax, APIs, or version shims.
- Lua runtime dependencies are forbidden; use LuaJIT and stable Neovim APIs.
- `mini.test` is the sole test dependency.
- Any additional development dependency requires a demonstrated gap and
  explicit approval. Never vendor a dependency or utility for convenience.

## 3. Required Tooling

StyLua owns formatting, `lua-language-server` owns static analysis/LuaCATS, and
`mini.test` owns tests. Do not add overlapping tools.

The direct quality gates are:

```bash
stylua --check .
lua-language-server --check . --checklevel=Warning
nvim --headless --noplugin -u ./tests/minimal_init.lua \
  -c "lua MiniTest.run()" -c "qa!"
```

Use a repository wrapper if it becomes the documented entrypoint. CI must
enforce all gates once their configuration/harness exists. Report unavailable
gates; never claim they passed or add unrelated tooling merely to run them.

Manual test workflow from the repository root:

```bash
./scripts/install-test-deps
nvim --headless --noplugin -u "$PWD/tests/minimal_init.lua" \
  -c 'lua MiniTest.run()' -c 'qa!'
```

Run one file with `MiniTest.run_file("tests/schema/dsl_spec.lua")`. For
interactive debugging, start `nvim -u ./tests/minimal_init.lua` and run
`:lua MiniTest.run()`; starting Neovim without `--headless` intentionally keeps
the editor open.

## 4. Test-Driven Development

Use red-green-refactor for meaningful behavior changes: write the smallest
failing test, confirm it fails for the intended reason, implement only enough
to pass, then simplify while green. Pure refactors start from passing
characterization coverage.

Documentation, formatting, generated output, and trivial mechanical changes do
not need an artificial red test. State why when test-first work is impractical.

- Test observable behavior through public APIs, not implementation details.
- Cover non-trivial branches, parsers, state transitions, cancellation, and
  error paths without requiring a test per function or coverage percentage.
- Keep tests deterministic: no network, credentials, or real agent binaries.
  Use the mock ACP agent for process/protocol integration tests.
- Give UI code behavioral headless smoke tests, not pixel assertions.
- Run focused tests while developing and the complete suite before handoff.

## 5. Lua Style

- Use `snake_case` for files, modules, functions, and variables; `PascalCase`
  for LuaCATS types; `UPPER_SNAKE_CASE` only for true constants.
- Keep variables/functions local unless exported in the module API. Accidental
  globals are defects. Return one small public API table per module.
- Use dot calls for stateless module functions and colon methods only for
  instance state. Prefer guard clauses and early returns over deep nesting.
- Keep functions and modules cohesive; do not enforce numeric line limits.
- Prefer plain tables and functions. Use metatables only when they materially
  simplify identity or lifecycle.
- One implementation does not justify an interface or factory.
- Comments explain constraints and non-obvious reasons, not syntax. Do not
  leave speculative TODO scaffolding.

### Table Semantics

- Do not mutate caller-owned tables unless documented. Copy only at ownership
  boundaries; do not deep-copy defensively everywhere.
- Keep array-like tables dense. `#table` is valid only for dense sequences.
- Never depend on `pairs()` order. Only `false` and `nil` are falsey; `0` and
  `""` are truthy.
- Do not silently coerce strings, numbers, booleans, or missing values.
- Make in-place mutation explicit in its name or API documentation.

## 6. Types and Documentation

LuaCATS is mandatory for public APIs, configuration, protocol values, callbacks,
and events. Do not annotate trivial locals when inference is clear.

Every exported function documents its purpose, parameters, returns, and failure
behavior. Comment internals only where intent or an invariant is not obvious.
Change generated files through their source or generator, never by hand.

Treat diagnostics as errors: no unresolved globals, undefined fields, unchecked
nullable values, type mismatches, or uncertainty hidden behind `any`. Never
disable a diagnostic for a file/project. A narrow
`---@diagnostic disable-next-line` requires an unrepresentable external API and
a comment explaining why.

## 7. Architecture and Lifecycle

- Separate transformation, validation, and state transitions from Neovim UI,
  filesystem, and process I/O.
- `require()` must only define and return APIs. Importing a module must not
  start processes, create buffers, register autocmds/keymaps, or mutate options.
- Explicit `setup`, construction, start, cancel, and dispose own side effects.
- Mutable state has a clear owner/lifecycle, never a hidden module singleton;
  sessions live in the explicit registry.
- Repeated setup/dispose cycles must not leak handlers, buffers, or processes.
- Avoid circular module dependencies. Split modules when responsibilities
  diverge, not in anticipation of future growth.

Core modules never notify, print, open UI, or choose presentation. They return
structured errors and typed events; setup, health, and UI modules display them.

## 8. Errors and Validation

- Expected failures return explicit values such as `nil, err` or `false, err`.
  Check both results of every `pcall`; never swallow its error.
- Reserve `error()` for violated internal invariants or clear API misuse.
- Use `assert()` freely in tests, not as production input validation.
- Preserve context internally, sanitize user messages, and make user, config,
  protocol, and process failures specific and actionable.

Configuration uses a closed schema:

- Validate the untouched user table before defaults. Unknown keys/types are
  errors; collect all errors in one pass.
- Never hide invalid input with permissive deep merging or mutate user config.
  Apply defaults only after validation succeeds.
- Deprecations warn with an exact replacement path; do not silently translate
  old keys.

For ACP/JSON-RPC, validate consumed fields and reject malformed/contradictory
messages, but ignore unknown optional fields from newer peers.

## 9. Neovim APIs, Async Work, and Processes

Prefer APIs in this order:

1. Stable, non-deprecated `vim.*` Lua APIs
2. Structured `vim.api.nvim_*` calls
3. `vim.fn` when no proper Lua API exists
4. Ex-command strings only as a last resort

Never use private `vim._*` APIs. Do not monkey-patch globals or Neovim APIs,
mutate `package.path` at runtime, or depend on deprecated APIs.

- Never block Neovim's main loop with waits, polling, sleeps, or heavy work.
- Spawn processes with `vim.system()` and argument arrays, never shell-built
  command strings, `os.execute`, or `io.popen`.
- Pass cwd/environment explicitly; never interpolate untrusted command text.
- Marshal editor/UI work to the main loop when callback context requires it.
- Cancellation/disposal releases resources. Ignore late results for disposed
  sessions; they must not revive closed state.
- A completion callback or terminal event must fire at most once.

## 10. Security

- Validate all external input before changing state.
- Never log tokens, environments, prompts, tool payloads, or sensitive data by
  default.
- Do not execute generated Lua or use `load`, `loadstring`, the `debug` library,
  LuaJIT FFI, or global/package monkey-patching.
- Use `dofile` only for trusted project development files when a module cannot.

## 11. Editing and Completion Discipline

- Preserve existing user changes. Make the smallest root-cause change; avoid
  unrelated refactors.
- Revise existing files instead of creating versioned copies.
- Never run destructive Git or filesystem commands without explicit instruction.
- Do not stash user work to manufacture a clean test baseline.
- Update documentation and LuaCATS contracts with public behavior changes.

Before completing a code task:

- [ ] The new test failed first for the intended reason, when TDD applies.
- [ ] Focused tests, the full `mini.test` suite, and `stylua --check .` pass.
- [ ] `lua-language-server` reports zero diagnostics.
- [ ] Public APIs/failures are documented and typed; no error is swallowed or
      sensitive value logged.
- [ ] No unnecessary dependency, compatibility shim, or abstraction was added.
- [ ] Any unavailable check is called out explicitly in the handoff.

If it passes tests but violates an invariant, it is still wrong. If it is
correct but needlessly complicated, simplify it before handoff.
