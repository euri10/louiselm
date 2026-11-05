# Lua and Neovim policy

Required by [AGENTS.md](../AGENTS.md) for Lua/Neovim code, dependencies, types,
API documentation, design or review. Read with [testing](agent-testing.md).

## Runtime and tooling

{{confirmed_neovim_version_and_lua_scope}}

Target the selected Neovim runtime and its embedded LuaJIT/Lua 5.1. This policy
is for Neovim projects; standalone Lua needs its own confirmed runtime policy.
Use StyLua for formatting, LuaLS/LuaCATS for static analysis and public contracts,
and `mini.test` for tests. Keep one tool responsible for each concern.

Prefer existing Lua/Neovim facilities to new dependencies. Test tooling belongs
to the project and must run without the maintainer's personal configuration.
Pin/provision the chosen test dependencies deliberately.

## Language and types

Keep variables and functions local unless exported. Use `snake_case` for
ordinary names, `PascalCase` for LuaCATS types and `UPPER_SNAKE_CASE` for
constants. Return a small public API table; use dot calls for stateless functions
and colon methods for instance state.

Prefer plain tables and functions. Do not mutate caller-owned tables without a
documented contract; copy at ownership boundaries. Keep sequences dense, do not
depend on `pairs()` order, and remember that zero and the empty string are
truthy. Never silently coerce invalid or missing input.

Annotate public APIs, configuration, callbacks and boundary values with LuaCATS.
Document failure behavior. Avoid annotations for obvious locals and do not hide
uncertainty behind `any`. Fix diagnostics; narrow suppressions need a reason.

Expected failures return explicit values such as `nil, err` or `false, err`.
Check both results of `pcall`. Reserve `error()` for internal invariants or
API misuse; tests may assert freely.

## Neovim boundaries and lifecycle

`require()` defines and returns APIs. Explicit setup/construction owns
autocmds, keymaps, buffers, processes and other effects; disposal releases them.
Keep pure transformations independent of Neovim when possible. Core code
returns errors/events; presentation code decides how to display them.

Prefer stable `vim.*` APIs and structured calls. Do not depend on private APIs,
monkey-patch globals or manipulate runtime module paths in production without
a concrete project requirement. Avoid blocking the editor's main loop.

Use argument arrays and explicit cwd/environment for child processes.
Process/RPC/libuv callbacks may run in fast-event context; schedule editor/UI
operations at the narrowest boundary. Tests must model that scheduling and
late results after disposal. Cancellation completes at most once and cannot
revive disposed state.

## Quality gates

{{lua_gate_status_and_first_task_prerequisites}}

Default commands, once the first task supplies the isolated test initializer:

```sh
stylua --check .
lua-language-server --check . --checklevel=Warning
nvim --headless --noplugin -u ./tests/minimal_init.lua -c "lua MiniTest.run()" -c "qa!"
```

The initializer must load the project and pinned `mini.test`, collect the
intended tests and isolate persistent state. Verify that the headless command
returns nonzero for a deliberate failed assertion and that startup errors
cannot be hidden by the final quit. Adapt the documented runner if needed;
do not report a planned or empty harness as passing.

Test observable API and editor behavior. Dispose resources even after assertion
failures. Use controlled local protocol/process doubles, not real agents,
credentials or a user's live editor.
