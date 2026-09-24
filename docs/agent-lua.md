# Lua and Neovim policy

Required by [AGENTS.md](../AGENTS.md) for Lua/Neovim work, dependencies, types and
public API documentation. Read once before applicable work.

## Runtime and dependencies

- Target the latest stable Neovim at release and its embedded LuaJIT/Lua 5.1.
  Do not support standalone Lua or add Lua 5.2+ syntax, APIs, or version shims.
- Lua runtime dependencies are forbidden; use LuaJIT and stable Neovim APIs.
  One named exception: `lyaml`, which `skills/discovery.lua` uses to parse
  SKILL.md frontmatter. Hand-rolling a YAML subset is explicitly rejected —
  frontmatter parsing is load-bearing rather than incidental, since a
  persisted workflow may itself be a frontmatter document discovered by the
  same mechanism (louiselm-4gif, louiselm-uqip). This is one exception with a
  rationale, not an open door; the bullet below still governs everything else.
- `mini.test` is the sole *direct* Lua test dependency. The suite additionally
  needs `lyaml` present, reached through the skills code rather than required
  by a test — see [testing](agent-testing.md) for the install commands and why
  its absence surfaces as scattered assertion failures.
- Any additional development dependency requires a demonstrated gap and
  explicit approval. Never vendor a dependency or utility for convenience.
- `sqlite3` >= 3.38 with JSON support is a required runtime/test executable
  (approved in louiselm-3x9p). Turn recording uses asynchronous CLI calls,
  rollback journal DELETE and synchronous EXTRA; do not enable WAL.

### Lua

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

#### Table Semantics

- Do not mutate caller-owned tables unless documented. Copy only at ownership
  boundaries; do not deep-copy defensively everywhere.
- Keep array-like tables dense. `#table` is valid only for dense sequences.
- Never depend on `pairs()` order. Only `false` and `nil` are falsey; `0` and
  `""` are truthy.
- Do not silently coerce strings, numbers, booleans, or missing values.
- Make in-place mutation explicit in its name or API documentation.

## Types and documentation

For Lua, LuaCATS is mandatory for public APIs, configuration, protocol values,
callbacks, and events. Do not annotate trivial locals when inference is clear.
Rust uses the [Rust policy](agent-rust.md).

Every exported function documents its purpose, parameters, returns, and failure
behavior. Comment internals only where intent or an invariant is not obvious.
Change generated files through their source or generator, never by hand.

Treat diagnostics as errors: no unresolved globals, undefined fields, unchecked
nullable values, type mismatches, or uncertainty hidden behind `any`. Never
disable a diagnostic for a file/project. A narrow
`---@diagnostic disable-next-line` requires an unrepresentable external API and
a comment explaining why.

## Neovim APIs, async work, and processes

Prefer APIs in this order:

1. Stable, non-deprecated `vim.*` Lua APIs
2. Structured `vim.api.nvim_*` calls
3. `vim.fn` when no proper Lua API exists
4. Ex-command strings only as a last resort

Never use private `vim._*` APIs. Do not monkey-patch globals or Neovim APIs,
mutate `package.path` at runtime, or depend on deprecated APIs, except for
Attention's documented `vim.paste` activity hook below.

Attention may wrap Neovim's documented `vim.paste` hook solely to restart its
inactivity delay. Count every invocation, including each streamed chunk, as
activity; scripted `nvim_paste()` calls count too. Do not inspect, copy, log,
or retain the `lines` payload.

Use one explicitly owned dispatcher for all active Attention controllers, not
one wrapper per controller. Pass the same `lines` and `phase` to the previously
installed handler, return its result unchanged, and preserve its error behavior.
Disposal must release each controller, preserve the delegation chain, and
restore the previous handler only while the dispatcher still owns `vim.paste`;
never overwrite a later replacement. Repeated controller setup and disposal
must not accumulate wrappers. This exception grants no other global or Neovim
API patch authority.

- Around 44 modules reference Neovim nowhere — the schema, workflow-definition,
  routing, permission, skills, and doc-generator layers. That is what lets them
  be tested as pure logic. Do not introduce the first `vim.*` call into such a
  module to replace a local helper: swapping a six-line `trim` for `vim.trim`
  buys nothing and costs the module its independence. Check with
  `grep -L 'local nvim = vim' <file>` before reaching for the stdlib in an
  unfamiliar file. Where a module already uses Neovim, prefer `vim.*` over a
  hand-rolled equivalent (`louiselm-stdlib-helper-cleanup-kw5m`).
- Never block Neovim's main loop with waits, polling, sleeps, or heavy work.
- Spawn processes with `vim.system()` and argument arrays, never shell-built
  command strings, `os.execute`, or `io.popen`.
- Pass cwd/environment explicitly; never interpolate untrusted command text.
- Treat callbacks from `vim.system()`, RPC/stdout handlers, libuv, and process
  exits as fast-event callbacks by default. They may validate or copy data, but
  must not call editor/UI APIs such as `nvim.api`, buffer/window operations, or
  notifications directly. Marshal editor/UI work to the main loop with
  `vim.schedule()` at the narrowest shared boundary, and test that boundary.
- Cancellation/disposal releases resources. Ignore late results for disposed
  sessions; they must not revive closed state.
- A completion callback or terminal event must fire at most once.
