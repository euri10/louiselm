# louiselm.nvim

## Development

Install the pinned `mini.test` dependency from `mini.nvim`:

```sh
./scripts/install-test-deps
```

The installer uses `mini.nvim` `v0.18.0` at commit
`1345d191bb3da9c7b0e977f4387c5761f9bff68d`. Run the test suite with:

```sh
nvim --headless --noplugin -u "./tests/minimal_init.lua" -c 'lua MiniTest.run()' -c 'qa!'
```

Run one test file:

```sh
nvim --headless --noplugin -u "$pwd/tests/minimal_init.lua" \
  -c 'lua minitest.run_file("tests/schema/dsl_spec.lua")' -c 'qa!'
```

For interactive debugging, start `nvim -u ./tests/minimal_init.lua` and run
`:lua MiniTest.run()`. Without `--headless`, Neovim intentionally stays open.

## ACP client

For a minimal manual session, run this from the repository root:

```sh
nvim -u ./manual_init.lua
```

The config installs this checkout with `vim.pack.add`. Verify it loaded with
`:lua print(require("louiselm.acp").PROTOCOL_VERSION)`.

ACP agents communicate over newline-delimited JSON-RPC on stdio. The client
starts the configured process, negotiates protocol version 1, then exposes
session requests and streamed notifications without requiring a UI:

```lua
local acp = require("louiselm.acp")
local client = assert(acp.connect({ command = "claude-agent-acp", args = {} }))

client:initialize(nil, function(result, err)
  if err ~= nil then
    return
  end
  client:new_session({ cwd = vim.fn.getcwd(), mcpServers = {} }, function(session, session_err)
    if session_err == nil then
      client:prompt({ sessionId = session.sessionId, prompt = { { type = "text", text = "Hello" } } })
    end
  end)
end)
```

Use `on_notification` and `on_request` connection callbacks for streamed
updates and agent permission requests.

## Headless sessions

The session API manages multiple named ACP sessions without requiring a UI:

```lua
local sessions = assert(require("louiselm.session").new({
  claude = { command = "claude-agent-acp", args = {} },
}))

local session = assert(sessions:create_session("claude", { cwd = vim.fn.getcwd() }, function(value, err)
  assert(err == nil, err)
end))
session:on(function(event)
  if event.type == "chunk" then
    print(event.data.content.text)
  end
end)
session:prompt("Review this project")
```

Each session is addressable through `get_session(id)`, inspectable with
`session:inspect()`, and exposes `prompt`, `cancel`, and `dispose` methods.
Events include streamed chunks, tool-call start/finish, permission requests,
turn completion, and agent/process errors. Call `sessions:dispose()` when the
owner is finished to terminate all remaining agent processes.

## Chat UI

The optional chat controller renders a session into a scratch markdown buffer,
shows streamed chunks and tool calls, and keeps one buffer per session:

```lua
local chat = assert(require("louiselm.ui.chat").new(sessions, {
  agents = { "claude" },
}))
chat:new_session()
```

For a quick interactive check, run `nvim -u ./tests/minimal_init.lua` and use
`:LouiselmChat`. It launches `claude-agent-acp` by default; set
`LOUISELM_AGENT_COMMAND` to use another ACP executable.

Use `chat:switch("session-1")` for another attached session. Prompts entered in
the buffer, including slash commands, are passed to the session unchanged.
Call `chat:dispose()` to remove its buffers and event listeners; it does not
dispose the sessions it displays.

## Permissions

Sessions default to an `ask-human` policy: agent permission requests are held
until an event consumer responds. For explicitly scoped automation, pass a
policy object when creating a session:

```lua
local permission = require("louiselm.permission")
local policy = assert(permission.auto_approve_scoped({
  paths = { vim.fn.getcwd() },
  commands = { { "git", "status" } },
}))
local session = assert(sessions:create_session("claude", {
  permission_policy = policy,
}))
```

File edits and command argv are checked without shell expansion. Unknown ACP
permission requests remain askable and are never auto-approved.

## Chat context

Context providers stay thin: they point the agent at files and skills while
visual selections carry their selected text. Queue context on an attached chat
view before submitting its prompt:

```lua
chat:mention_buffer()
chat:send_selection()
chat:pick_file()
chat:pick_skill()
```

Pass discovered skills to the chat controller to enable the skill picker:

```lua
local found, errors = require("louiselm.skills").discover({ skill_root })
assert(#errors == 0, "skill discovery failed")
local chat = assert(require("louiselm.ui.chat").new(sessions, { skills = found }))
```

To use an existing `mini.nvim` checkout instead of `.deps/mini.nvim`:

```sh
MINI_NVIM_PATH=/path/to/mini.nvim nvim --headless --noplugin \
  -u "$PWD/tests/minimal_init.lua" -c 'lua MiniTest.run()' -c 'qa!'
```

## Skills

Discover Agent Skills metadata from configured directories and inject a thin
index when an agent does not provide native skill loading:

```lua
local skills = require("louiselm.skills")
local found, errors = skills.discover({ vim.fn.expand("~/.config/agentskills") })
local policy = assert(skills.policy("inject")) -- native, inject, or off
local index = assert(skills.inject(found))
```

The injected index contains only each skill's name, description, and
`SKILL.md` path. Use `skills.overlap(native_skill_dir, configured_paths)` to
detect native/configured skill trees that resolve to the same directory.
