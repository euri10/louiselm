local MiniTest = require("mini.test")
local Config = require("louiselm.agent.config")

local T = MiniTest.new_set()

T["validate"] = MiniTest.new_set()

T["validate"]["accepts named agent definitions and returns owned copies"] = function()
  local definitions = {
    claude = {
      command = "claude-agent-acp",
      args = { "--verbose" },
      env = { ANTHROPIC_LOG = "debug" },
      options = { model = "sonnet" },
    },
  }

  local normalized, errors = Config.normalize(definitions)

  MiniTest.expect.equality(errors, {})
  assert(normalized ~= nil)
  MiniTest.expect.equality(normalized ~= definitions, true)
  MiniTest.expect.equality(normalized.claude.command, definitions.claude.command)
  MiniTest.expect.equality(normalized.claude.args, definitions.claude.args)
  MiniTest.expect.equality(normalized.claude.args == definitions.claude.args, false)
end

T["validate"]["collects malformed definitions without coercion"] = function()
  local normalized, errors = Config.normalize({
    claude = {
      command = 42,
      args = { "ok", false },
      env = { TOKEN = 123 },
      extra = true,
    },
    ["bad.name"] = "not a definition",
  })

  MiniTest.expect.equality(normalized, nil)
  MiniTest.expect.equality(#errors, 5)
  MiniTest.expect.equality(errors[1].path, "agents.bad.name")
  MiniTest.expect.equality(errors[2].path, "agents.claude.args[2]")
  MiniTest.expect.equality(errors[3].path, "agents.claude.command")
  MiniTest.expect.equality(errors[3].type, "wrong_type")
  MiniTest.expect.equality(errors[4].path, "agents.claude.env.TOKEN")
  MiniTest.expect.equality(errors[5].path, "agents.claude.extra")
end

return T
