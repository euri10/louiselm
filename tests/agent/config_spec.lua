local MiniTest = require("mini.test")
local Config = require("louiselm.agent.config")

local T = MiniTest.new_set()

T["validate"] = MiniTest.new_set()

T["validate"]["copies optional upgrade argv and leaves it unset by default"] = function()
  local upgrade = { "npm", "install", "-g", "mock-acp@latest" }
  local normalized, errors = Config.normalize({
    mock = { command = "mock-acp", upgrade = upgrade },
    other = { command = "other-acp" },
  })
  MiniTest.expect.equality(errors, {})
  assert(normalized)
  MiniTest.expect.equality(normalized.mock.upgrade, upgrade)
  MiniTest.expect.equality(normalized.mock.upgrade == upgrade, false)
  MiniTest.expect.equality(normalized.other.upgrade, nil)
end

T["validate"]["rejects malformed upgrade argv"] = function()
  for _, upgrade in ipairs({ false, "npm install", {}, { "" }, { "npm", false }, { [2] = "npm" } }) do
    local normalized, errors = Config.normalize({ mock = { command = "mock-acp", upgrade = upgrade } })
    MiniTest.expect.equality(normalized, nil)
    MiniTest.expect.equality(errors[1].path:find("agents.mock.upgrade", 1, true), 1)
  end
end

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
  MiniTest.expect.equality(normalized.claude.skills.policy, "native")
end

T["validate"]["preserves an optional transcript layout"] = function()
  local normalized, errors = Config.normalize({
    renamed_codex = { command = "codex-acp", transcript_layout = "codex" },
  })

  MiniTest.expect.equality(errors, {})
  assert(normalized ~= nil)
  MiniTest.expect.equality(normalized.renamed_codex.transcript_layout, "codex")
end

T["validate"]["rejects a malformed transcript layout without coercion"] = function()
  local normalized, errors = Config.normalize({
    codex = { command = "codex-acp", transcript_layout = false },
  })

  MiniTest.expect.equality(normalized, nil)
  MiniTest.expect.equality(errors[1].path, "agents.codex.transcript_layout")
  MiniTest.expect.equality(errors[1].type, "wrong_type")
end

T["validate"]["resolves per-agent skill policy against the global default"] = function()
  local normalized, errors = Config.normalize({
    claude = { command = "claude-agent-acp" },
    deepseek = { command = "acp-llm-adapter", skills = { policy = "inject" } },
  }, "off")

  MiniTest.expect.equality(errors, {})
  assert(normalized ~= nil)
  MiniTest.expect.equality(normalized.claude.skills.policy, "off")
  MiniTest.expect.equality(normalized.deepseek.skills.policy, "inject")
end

T["validate"]["rejects unknown per-agent skill settings"] = function()
  local normalized, errors = Config.normalize({
    claude = { command = "claude-agent-acp", skills = { paths = { "/tmp/skills" } } },
  })

  MiniTest.expect.equality(normalized, nil)
  MiniTest.expect.equality(errors[1].path, "agents.claude.skills.paths")
end

T["validate"]["rejects an empty per-agent skill override"] = function()
  local normalized, errors = Config.normalize({ claude = { command = "claude-agent-acp", skills = {} } })

  MiniTest.expect.equality(normalized, nil)
  MiniTest.expect.equality(errors[1].path, "agents.claude.skills.policy")
  MiniTest.expect.equality(errors[1].type, "missing_required")
end

T["validate"]["accepts an optional latest-version check shaped like command/args/env"] = function()
  local definitions = {
    codex = {
      command = "codex-agent-acp",
      latest = {
        command = "npm",
        args = { "view", "@agentclientprotocol/codex-acp", "version" },
        env = { NPM_CONFIG_LOGLEVEL = "error" },
      },
    },
  }

  local normalized, errors = Config.normalize(definitions)

  MiniTest.expect.equality(errors, {})
  assert(normalized ~= nil)
  MiniTest.expect.equality(normalized.codex.latest, definitions.codex.latest)
  MiniTest.expect.equality(normalized.codex.latest ~= definitions.codex.latest, true)
  MiniTest.expect.equality(normalized.codex.latest.args ~= definitions.codex.latest.args, true)
end

T["validate"]["defaults latest.args to an empty array when omitted"] = function()
  local normalized, errors = Config.normalize({
    codex = { command = "codex-agent-acp", latest = { command = "npm" } },
  })

  MiniTest.expect.equality(errors, {})
  assert(normalized ~= nil)
  MiniTest.expect.equality(normalized.codex.latest, { command = "npm", args = {} })
end

T["validate"]["leaves latest nil when the agent has no latest-version check"] = function()
  local normalized, errors = Config.normalize({ codex = { command = "codex-agent-acp" } })

  MiniTest.expect.equality(errors, {})
  assert(normalized ~= nil)
  MiniTest.expect.equality(normalized.codex.latest, nil)
end

T["validate"]["rejects a malformed latest-version check without coercion"] = function()
  local normalized, errors = Config.normalize({
    codex = {
      command = "codex-agent-acp",
      latest = { command = 7, args = { "view", false }, bogus = true },
    },
  })

  MiniTest.expect.equality(normalized, nil)
  MiniTest.expect.equality(#errors, 3)
  MiniTest.expect.equality(errors[1].path, "agents.codex.latest.args[2]")
  MiniTest.expect.equality(errors[2].path, "agents.codex.latest.bogus")
  MiniTest.expect.equality(errors[2].type, "unknown_key")
  MiniTest.expect.equality(errors[3].path, "agents.codex.latest.command")
  MiniTest.expect.equality(errors[3].type, "wrong_type")
end

T["validate"]["rejects a latest-version check that is not a table"] = function()
  local normalized, errors = Config.normalize({
    codex = { command = "codex-agent-acp", latest = "npm view codex-acp version" },
  })

  MiniTest.expect.equality(normalized, nil)
  MiniTest.expect.equality(errors[1].path, "agents.codex.latest")
  MiniTest.expect.equality(errors[1].type, "wrong_type")
end

T["validate"]["accepts an optional installed-version check shaped like command/args/env"] = function()
  local definitions = {
    deepseek = {
      command = "/opt/acp-debug.sh",
      args = { "acp-llm-adapter", "serve", "--backend", "deepseek" },
      version = {
        command = "/opt/acp-debug.sh",
        args = { "acp-llm-adapter", "--version" },
      },
    },
  }

  local normalized, errors = Config.normalize(definitions)

  MiniTest.expect.equality(errors, {})
  assert(normalized ~= nil)
  MiniTest.expect.equality(normalized.deepseek.version, definitions.deepseek.version)
  MiniTest.expect.equality(normalized.deepseek.version ~= definitions.deepseek.version, true)
  MiniTest.expect.equality(normalized.deepseek.version.args ~= definitions.deepseek.version.args, true)
end

T["validate"]["defaults version.args to an empty array when omitted"] = function()
  local normalized, errors = Config.normalize({
    codex = { command = "codex-agent-acp", version = { command = "codex-agent-acp" } },
  })

  MiniTest.expect.equality(errors, {})
  assert(normalized ~= nil)
  MiniTest.expect.equality(normalized.codex.version, { command = "codex-agent-acp", args = {} })
end

T["validate"]["leaves version nil when the agent has no installed-version override"] = function()
  local normalized, errors = Config.normalize({ codex = { command = "codex-agent-acp" } })

  MiniTest.expect.equality(errors, {})
  assert(normalized ~= nil)
  MiniTest.expect.equality(normalized.codex.version, nil)
end

T["validate"]["rejects a malformed installed-version check without coercion"] = function()
  local normalized, errors = Config.normalize({
    codex = {
      command = "codex-agent-acp",
      version = { command = 7, args = { "view", false }, bogus = true },
    },
  })

  MiniTest.expect.equality(normalized, nil)
  MiniTest.expect.equality(#errors, 3)
  MiniTest.expect.equality(errors[1].path, "agents.codex.version.args[2]")
  MiniTest.expect.equality(errors[2].path, "agents.codex.version.bogus")
  MiniTest.expect.equality(errors[2].type, "unknown_key")
  MiniTest.expect.equality(errors[3].path, "agents.codex.version.command")
  MiniTest.expect.equality(errors[3].type, "wrong_type")
end

T["validate"]["rejects an installed-version check that is not a table"] = function()
  local normalized, errors = Config.normalize({
    codex = { command = "codex-agent-acp", version = "acp-llm-adapter --version" },
  })

  MiniTest.expect.equality(normalized, nil)
  MiniTest.expect.equality(errors[1].path, "agents.codex.version")
  MiniTest.expect.equality(errors[1].type, "wrong_type")
end

T["validate"]["accepts declared capabilities and returns an owned copy"] = function()
  local definitions = {
    codex = { command = "codex-agent-acp", capabilities = { "image-generation" } },
  }

  local normalized, errors = Config.normalize(definitions)

  MiniTest.expect.equality(errors, {})
  assert(normalized ~= nil)
  MiniTest.expect.equality(normalized.codex.capabilities, { "image-generation" })
  MiniTest.expect.equality(normalized.codex.capabilities ~= definitions.codex.capabilities, true)
end

T["validate"]["leaves capabilities nil when the agent declares none"] = function()
  local normalized, errors = Config.normalize({ codex = { command = "codex-agent-acp" } })

  MiniTest.expect.equality(errors, {})
  assert(normalized ~= nil)
  MiniTest.expect.equality(normalized.codex.capabilities, nil)
end

T["validate"]["accepts an empty capabilities list"] = function()
  local normalized, errors = Config.normalize({ codex = { command = "codex-agent-acp", capabilities = {} } })

  MiniTest.expect.equality(errors, {})
  assert(normalized ~= nil)
  MiniTest.expect.equality(normalized.codex.capabilities, {})
end

T["validate"]["rejects a capabilities value that is not a table"] = function()
  local normalized, errors = Config.normalize({
    codex = { command = "codex-agent-acp", capabilities = "image-generation" },
  })

  MiniTest.expect.equality(normalized, nil)
  MiniTest.expect.equality(errors[1].path, "agents.codex.capabilities")
  MiniTest.expect.equality(errors[1].type, "wrong_type")
end

T["validate"]["rejects a non-dense capabilities table"] = function()
  local normalized, errors = Config.normalize({
    codex = { command = "codex-agent-acp", capabilities = { [1] = "image-generation", [3] = "ocr" } },
  })

  MiniTest.expect.equality(normalized, nil)
  MiniTest.expect.equality(errors[1].path, "agents.codex.capabilities")
  MiniTest.expect.equality(errors[1].type, "invalid_value")
end

T["validate"]["rejects non-string and empty-string capability entries without coercion"] = function()
  local normalized, errors = Config.normalize({
    codex = { command = "codex-agent-acp", capabilities = { "image-generation", "", 7 } },
  })

  MiniTest.expect.equality(normalized, nil)
  MiniTest.expect.equality(#errors, 2)
  MiniTest.expect.equality(errors[1].path, "agents.codex.capabilities[2]")
  MiniTest.expect.equality(errors[1].type, "invalid_value")
  MiniTest.expect.equality(errors[2].path, "agents.codex.capabilities[3]")
  MiniTest.expect.equality(errors[2].type, "invalid_value")
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
