local MiniTest = require("mini.test")
local Config = require("louiselm.config")
local Louiselm = require("louiselm")
local Schema = require("louiselm.schema")

local T = MiniTest.new_set()

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

-- Deliberately `unknown`: these cases feed setup() unknown keys and missing required
-- fields to prove the runtime validator rejects them. Typing this parameter would let
-- the static type reject them first, at the call site, and the militant-validator
-- coverage would never reach the code it exists to test.
---@param config unknown
local function capture_setup(config)
  local notifications = {}
  ---@diagnostic disable-next-line: undefined-global
  local original_notify = vim.notify
  ---@diagnostic disable-next-line: undefined-global
  vim.notify = function(message, level)
    notifications[#notifications + 1] = { message = message, level = level }
  end
  local ok, result = Louiselm.setup(config)
  ---@diagnostic disable-next-line: undefined-global
  vim.notify = original_notify
  return ok, result, notifications
end

T["setup"] = MiniTest.new_set()

T["setup"]["requires Provider and validates fixed and routed service mappings"] = function()
  for _, definition in ipairs({
    { command = "agent" },
    { command = "agent", provider = "" },
    { command = "agent", provider = {} },
    { command = "agent", provider = { { provider = "service", options = {} } } },
    { command = "agent", provider = { { provider = "service", options = { route = 7 } } } },
  }) do
    local ok, report = capture_setup({ agents = { agent = definition } })
    MiniTest.expect.equality(ok, false)
    MiniTest.expect.equality(report.errors[1].path:find("agents.agent.provider", 1, true), 1)
  end
  local ok = capture_setup({
    agents = {
      agent = {
        command = "agent",
        provider = {
          { provider = "service", options = { model = "wire-id", enabled = false } },
        },
      },
    },
  })
  MiniTest.expect.equality(ok, true)
end

T["setup"]["accepts upgrade argv and rejects malformed commands"] = function()
  local ok, report = capture_setup({
    agents = {
      mock = {
        provider = "test-service",
        command = "mock-acp",
        upgrade = { "npm", "install", "-g", "mock-acp@latest" },
      },
    },
  })
  MiniTest.expect.equality(ok, true)
  MiniTest.expect.equality(report, nil)
  for _, upgrade in ipairs({ false, "npm install", {}, { "" }, { "npm", false }, { [2] = "npm" } }) do
    local accepted, invalid =
      capture_setup({ agents = { mock = { provider = "test-service", command = "mock-acp", upgrade = upgrade } } })
    MiniTest.expect.equality(accepted, false)
    MiniTest.expect.equality(invalid.errors[1].path:find("agents.mock.upgrade", 1, true), 1)
  end
end

T["setup"]["rejects invalid config and reports every error"] = function()
  local ok, report, notifications = capture_setup({
    agents = {
      codex = { provider = "test-service", command = 42, surprise = true },
    },
    extra = true,
  })

  MiniTest.expect.equality(ok, false)
  MiniTest.expect.equality(report.count, 3)
  MiniTest.expect.equality(#notifications, 1)
  MiniTest.expect.equality(notifications[1].message, report.text)
  ---@diagnostic disable-next-line: undefined-global
  MiniTest.expect.equality(notifications[1].level, vim.log.levels.ERROR)
end

T["setup"]["starts with valid config"] = function()
  local ok, report, notifications = capture_setup({
    agents = {
      codex = { provider = "test-service", command = "codex-acp", args = {}, env = { TOKEN = "secret" } },
      deepseek = { provider = "test-service", command = "acp-llm-adapter", skills = { policy = "inject" } },
    },
    skills = { paths = {}, policy = "native" },
  })

  MiniTest.expect.equality(ok, true)
  MiniTest.expect.equality(report, nil)
  MiniTest.expect.equality(#notifications, 0)
end

T["setup"]["accepts disabling global keymaps"] = function()
  local ok, report, notifications = capture_setup({ keymaps = false })

  MiniTest.expect.equality(ok, true)
  MiniTest.expect.equality(report, nil)
  MiniTest.expect.equality(#notifications, 0)
end

T["setup"]["accepts an optional per-agent latest-version check"] = function()
  local ok, report = capture_setup({
    agents = {
      codex = {
        provider = "test-service",
        command = "codex-acp",
        latest = { command = "npm", args = { "view", "@agentclientprotocol/codex-acp", "version" } },
      },
      deepseek = { provider = "test-service", command = "acp-llm-adapter" },
    },
  })

  MiniTest.expect.equality(ok, true)
  MiniTest.expect.equality(report, nil)
end

T["setup"]["rejects a per-agent latest-version check missing its command"] = function()
  local ok, report = capture_setup({
    agents = { codex = { provider = "test-service", command = "codex-acp", latest = { args = { "view", "version" } } } },
  })

  MiniTest.expect.equality(ok, false)
  MiniTest.expect.equality(report.errors[1].path, "agents.codex.latest.command")
end

T["setup"]["accepts an optional per-agent installed-version override"] = function()
  local ok, report = capture_setup({
    agents = {
      deepseek = {
        provider = "test-service",
        command = "acp-debug.sh",
        args = { "acp-llm-adapter", "serve", "--backend", "deepseek" },
        version = { command = "acp-debug.sh", args = { "acp-llm-adapter", "--version" } },
      },
    },
  })

  MiniTest.expect.equality(ok, true)
  MiniTest.expect.equality(report, nil)
end

T["setup"]["rejects a per-agent installed-version override missing its command"] = function()
  local ok, report = capture_setup({
    agents = { codex = { provider = "test-service", command = "codex-acp", version = { args = { "--version" } } } },
  })

  MiniTest.expect.equality(ok, false)
  MiniTest.expect.equality(report.errors[1].path, "agents.codex.version.command")
end

T["setup"]["accepts declared agent capabilities"] = function()
  local ok, report = capture_setup({
    agents = {
      codex = { provider = "test-service", command = "codex-acp", capabilities = { "image-generation" } },
      claude = { provider = "test-service", command = "claude-agent-acp" },
    },
  })

  MiniTest.expect.equality(ok, true)
  MiniTest.expect.equality(report, nil)
end

T["setup"]["rejects an empty-string capability"] = function()
  local ok, report = capture_setup({
    agents = { codex = { provider = "test-service", command = "codex-acp", capabilities = { "image-generation", "" } } },
  })

  MiniTest.expect.equality(ok, false)
  MiniTest.expect.equality(report.errors[1].path, "agents.codex.capabilities[2]")
end

T["setup"]["rejects per-agent skill paths"] = function()
  local ok, report = capture_setup({
    agents = {
      codex = { provider = "test-service", command = "codex-acp", skills = { paths = { "/tmp/skills" } } },
    },
  })

  MiniTest.expect.equality(ok, false)
  MiniTest.expect.equality(report.errors[1].path, "agents.codex.skills.paths")
end

-- Replaces "rejects removed full-content injection with migration guidance", which
-- asserted the retained migration message. The key stayed in the schema only to
-- carry that message, which made it a settable field in generated completion and
-- vimdoc (louiselm-types-advertise-full-content-naz6). Rejection is preserved; the
-- closed schema supplies it now, so the field no longer has to exist to say no.
T["setup"]["rejects removed full-content injection as an unknown key"] = function()
  local ok, report = capture_setup({ skills = { full_content = true } })

  MiniTest.expect.equality(ok, false)
  MiniTest.expect.equality(report.errors[1].message, "skills.full_content: unknown key")
end

T["setup"]["registers chat commands for a valid setup"] = function()
  pcall(nvim.api.nvim_del_user_command, "LouiselmResume")

  local ok = capture_setup({})
  local commands = nvim.api.nvim_get_commands({ builtin = false })

  MiniTest.expect.equality(ok, true)
  MiniTest.expect.equality(commands.LouiselmResume ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmResume.bang, true)
end

T["setup"]["accepts a per-Agent transcript layout"] = function()
  local ok = capture_setup({
    agents = { renamed_codex = { provider = "test-service", command = "codex-acp", transcript_layout = "codex" } },
  })

  MiniTest.expect.equality(ok, true)
end

T["setup"]["rejects invalid skills without mutating caller config"] = function()
  local config = { skills = { paths = { "/tmp/skills" }, policy = "maybe" } }

  local ok, report = capture_setup(config)

  MiniTest.expect.equality(ok, false)
  MiniTest.expect.equality(report.errors[1].path, "skills.policy")
  MiniTest.expect.equality(config, { skills = { paths = { "/tmp/skills" }, policy = "maybe" } })
end

T["setup"]["keeps generated Agent Skills policy docs aligned"] = function()
  local outputs = {
    Schema.generate_vimdoc(Config.schema),
    Schema.generate_luacats(Config.schema),
  }
  for _, output in ipairs(outputs) do
    local normalized = output:gsub("%s+", " ")
    MiniTest.expect.equality(normalized:find("native delegates to the adapter", 1, true) ~= nil, true)
    MiniTest.expect.equality(normalized:find("inject uses LouiseLM discovery", 1, true) ~= nil, true)
    MiniTest.expect.equality(normalized:find("off disables automation", 1, true) ~= nil, true)
  end
end

T["setup"]["rejects a recorder command without an output placeholder"] = function()
  local config = { capture = { recorder = { "pw-record" } } }

  local ok, report = capture_setup(config)

  MiniTest.expect.equality(ok, false)
  MiniTest.expect.equality(report.errors[1].path, "capture.recorder")
  MiniTest.expect.equality(config, { capture = { recorder = { "pw-record" } } })
end

T["setup"]["accepts sibling roots for cross-workspace Beads lookup"] = function()
  local ok, report = capture_setup({ beads = { sibling_roots = { "~/code" } } })

  MiniTest.expect.equality(ok, true)
  MiniTest.expect.equality(report, nil)
end

T["setup"]["defaults sibling roots to an empty list"] = function()
  local ok = capture_setup({})

  MiniTest.expect.equality(ok, true)
end

T["setup"]["rejects a non-string sibling root without mutating caller config"] = function()
  local config = { beads = { sibling_roots = { 42 } } }

  local ok, report = capture_setup(config)

  MiniTest.expect.equality(ok, false)
  MiniTest.expect.equality(report.errors[1].path, "beads.sibling_roots[1]")
  MiniTest.expect.equality(config, { beads = { sibling_roots = { 42 } } })
end

return T
