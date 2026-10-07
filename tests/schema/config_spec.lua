local MiniTest = require("mini.test")
local Config = require("louiselm.config")
local Schema = require("louiselm.schema")

local T = MiniTest.new_set()

T["setup schema accepts an omitted or explicit MCP catalog without modifying input"] = function()
  MiniTest.expect.equality(Schema.validate(Config.schema, {}), {})
  local input = {
    mcp = {
      servers = {
        echo = { type = "stdio", command = "/bin/echo" },
        docs = { type = "http", url = "https://example.test/mcp" },
      },
      default_servers = { "docs" },
    },
    agents = { agent = { command = "agent", provider = "test-service", mcp_servers = {} } },
  }
  MiniTest.expect.equality(Schema.validate(Config.schema, input), {})
  MiniTest.expect.equality(input.mcp.servers.echo.args, nil)
  MiniTest.expect.equality(input.mcp.servers.echo.env, nil)
  MiniTest.expect.equality(input.mcp.servers.docs.headers, nil)
end

T["setup schema rejects transport-specific fields and reports all independent MCP errors"] = function()
  local errors = Schema.validate(Config.schema, {
    mcp = {
      servers = {
        echo = { type = "stdio", command = "echo", env = false },
        docs = { type = "http", url = "file:///tmp/server", args = {} },
      },
      default_servers = { "echo", "echo" },
      surprise = true,
    },
  })
  local paths = {}
  for _, err in ipairs(errors) do
    paths[#paths + 1] = err.path
  end
  table.sort(paths)
  MiniTest.expect.equality(paths, {
    "mcp.default_servers",
    "mcp.servers.docs",
    "mcp.servers.docs.url",
    "mcp.servers.echo.command",
    "mcp.servers.echo.env",
    "mcp.surprise",
  })
end

return T
