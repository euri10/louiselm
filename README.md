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

To use an existing `mini.nvim` checkout instead of `.deps/mini.nvim`:

```sh
MINI_NVIM_PATH=/path/to/mini.nvim nvim --headless --noplugin \
  -u "$PWD/tests/minimal_init.lua" -c 'lua MiniTest.run()' -c 'qa!'
```
