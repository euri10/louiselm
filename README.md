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
  client:new_session({ cwd = vim.fn.getcwd() }, function(session, session_err)
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

Use `chat:switch("session-1")` for another attached session. Prompts entered in
the buffer, including slash commands, are passed to the session unchanged.
Call `chat:dispose()` to remove its buffers and event listeners; it does not
dispose the sessions it displays.

To use an existing `mini.nvim` checkout instead of `.deps/mini.nvim`:

```sh
MINI_NVIM_PATH=/path/to/mini.nvim nvim --headless --noplugin \
  -u "$PWD/tests/minimal_init.lua" -c 'lua MiniTest.run()' -c 'qa!'
```
