local MiniTest = require("mini.test")
local MCP = require("louiselm.mcp")

local T = MiniTest.new_set()

T["omission preserves an empty MCP selection"] = function()
  local config, errors = MCP.normalize(nil)
  MiniTest.expect.equality(errors, {})
  MiniTest.expect.equality(config, { servers = {}, default_servers = {} })
  MiniTest.expect.equality(MCP.resolve(assert(config)), {})
end

T["defaults and Agent overrides resolve owned ACP v1 server descriptions"] = function()
  local raw = {
    servers = {
      echo = { type = "stdio", command = "/usr/bin/echo", args = { "hello" }, env = { Z = "last", A = "first" } },
      docs = { type = "http", url = "https://example.test/mcp", headers = { Authorization = "Bearer test-secret" } },
    },
    default_servers = { "echo", "docs" },
  }
  local config, errors = MCP.normalize(raw, { agent = { mcp_servers = { "docs" } } })
  MiniTest.expect.equality(errors, {})
  assert(config)
  local wire = MCP.resolve(config)
  MiniTest.expect.equality(wire, {
    {
      name = "echo",
      command = "/usr/bin/echo",
      args = { "hello" },
      env = { { name = "A", value = "first" }, { name = "Z", value = "last" } },
    },
    {
      name = "docs",
      type = "http",
      url = "https://example.test/mcp",
      headers = { { name = "Authorization", value = "Bearer test-secret" } },
    },
  })
  MiniTest.expect.equality(MCP.resolve(config, {}), {})
  MiniTest.expect.equality(MCP.resolve(config, { "docs" }), { wire[2] })
  raw.servers.echo.args[1], raw.servers.echo.env.A, raw.default_servers[1] = "changed", "changed", "docs"
  wire[1].args[1], wire[1].env[1].value, wire[2].headers[1].value = "changed", "changed", "changed"
  MiniTest.expect.equality(MCP.resolve(config)[1].args, { "hello" })
  MiniTest.expect.equality(MCP.resolve(config)[1].env[1].value, "first")
  MiniTest.expect.equality(MCP.resolve(config)[2].headers[1].value, "Bearer test-secret")
end

T["defaults only populate the selected transport after validation"] = function()
  local raw = {
    servers = {
      local_server = { type = "stdio", command = "/bin/server" },
      remote = { type = "http", url = "http://localhost:8080/mcp" },
    },
  }
  local config = assert(MCP.normalize(raw))
  MiniTest.expect.equality(raw.servers.local_server.args, nil)
  MiniTest.expect.equality(
    config.servers.local_server,
    { type = "stdio", command = "/bin/server", args = {}, env = {} }
  )
  MiniTest.expect.equality(config.servers.remote, { type = "http", url = "http://localhost:8080/mcp", headers = {} })
end

T["rejects closed shape and cross-reference errors together"] = function()
  local normalized, errors = MCP.normalize({
    extra = true,
    servers = { echo = { type = "stdio", command = 42, surprise = "test-secret" } },
    default_servers = { "missing" },
  }, { agent = { mcp_servers = { "absent" } } })
  MiniTest.expect.equality(normalized, nil)
  local paths = {}
  for _, err in ipairs(errors) do
    paths[#paths + 1] = err.path
    MiniTest.expect.equality(err.message:find("test-secret", 1, true), nil)
  end
  MiniTest.expect.equality(paths, {
    "agents.agent.mcp_servers[1]",
    "mcp.default_servers[1]",
    "mcp.extra",
    "mcp.servers.echo.command",
    "mcp.servers.echo.surprise",
  })
end

T["refuses malformed selections without coercion"] = function()
  for _, selection in ipairs({ false, "echo", { "echo", "echo" }, { [2] = "echo" }, { "" }, { false }, { "bad\nname" } }) do
    local config, errors = MCP.normalize({ default_servers = selection })
    MiniTest.expect.equality(config, nil)
    MiniTest.expect.equality(errors[1].path:find("mcp.default_servers", 1, true), 1)
  end
end

T["refuses invalid transport fields, executable paths and external values"] = function()
  for _, server in ipairs({
    { command = "/bin/server" },
    { type = "sse", url = "https://example.test" },
    { type = "stdio" },
    { type = "stdio", command = "server" },
    { type = "stdio", command = "/bin/server\0secret" },
    { type = "stdio", command = "/bin/server", url = "https://example.test" },
    { type = "stdio", command = "/bin/server", args = { [2] = "bad" } },
    { type = "stdio", command = "/bin/server", args = { "test-secret\0bad" } },
    { type = "stdio", command = "/bin/server", env = { ["BAD=NAME"] = "test-secret" } },
    { type = "stdio", command = "/bin/server", env = { TOKEN = "test-secret\0bad" } },
    { type = "http" },
    { type = "http", url = "file:///tmp/test-secret" },
    { type = "http", url = "https://" },
    { type = "http", url = "https://example.test/\nsecret" },
    { type = "http", url = "https://example.test", command = "/bin/server" },
    { type = "http", url = "https://example.test", headers = { ["Bad Header"] = "test-secret" } },
    {
      type = "http",
      url = "https://example.test",
      headers = { Authorization = "test-secret", authorization = "different" },
    },
    { type = "http", url = "https://example.test", headers = { Authorization = "test-secret\r\nInjected: yes" } },
    { type = "http", url = "https://example.test", headers = { Authorization = false } },
  }) do
    local config, errors = MCP.normalize({ servers = { example = server } })
    MiniTest.expect.equality(config, nil)
    assert(#errors > 0)
    for _, err in ipairs(errors) do
      MiniTest.expect.equality(err.message:find("test-secret", 1, true), nil)
      MiniTest.expect.equality(err.path:find("mcp.servers.example", 1, true), 1)
    end
  end
end

return T
