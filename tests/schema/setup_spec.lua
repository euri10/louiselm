local MiniTest = require("mini.test")
local Config = require("louiselm.config")
local Louiselm = require("louiselm")
local Schema = require("louiselm.schema")

local T = MiniTest.new_set()

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

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

T["setup"]["rejects invalid config and reports every error"] = function()
  local ok, report, notifications = capture_setup({
    agents = {
      codex = { command = 42, surprise = true },
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
      codex = { command = "codex-acp", args = {}, env = { TOKEN = "secret" } },
      deepseek = { command = "acp-llm-adapter", skills = { policy = "inject" } },
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
        command = "codex-acp",
        latest = { command = "npm", args = { "view", "@agentclientprotocol/codex-acp", "version" } },
      },
      deepseek = { command = "acp-llm-adapter" },
    },
  })

  MiniTest.expect.equality(ok, true)
  MiniTest.expect.equality(report, nil)
end

T["setup"]["rejects a per-agent latest-version check missing its command"] = function()
  local ok, report = capture_setup({
    agents = { codex = { command = "codex-acp", latest = { args = { "view", "version" } } } },
  })

  MiniTest.expect.equality(ok, false)
  MiniTest.expect.equality(report.errors[1].path, "agents.codex.latest.command")
end

T["setup"]["accepts an optional per-agent installed-version override"] = function()
  local ok, report = capture_setup({
    agents = {
      deepseek = {
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
    agents = { codex = { command = "codex-acp", version = { args = { "--version" } } } },
  })

  MiniTest.expect.equality(ok, false)
  MiniTest.expect.equality(report.errors[1].path, "agents.codex.version.command")
end

T["setup"]["accepts declared agent capabilities"] = function()
  local ok, report = capture_setup({
    agents = {
      codex = { command = "codex-acp", capabilities = { "image-generation" } },
      claude = { command = "claude-agent-acp" },
    },
  })

  MiniTest.expect.equality(ok, true)
  MiniTest.expect.equality(report, nil)
end

T["setup"]["rejects an empty-string capability"] = function()
  local ok, report = capture_setup({
    agents = { codex = { command = "codex-acp", capabilities = { "image-generation", "" } } },
  })

  MiniTest.expect.equality(ok, false)
  MiniTest.expect.equality(report.errors[1].path, "agents.codex.capabilities[2]")
end

T["setup"]["rejects per-agent skill paths"] = function()
  local ok, report = capture_setup({
    agents = {
      codex = { command = "codex-acp", skills = { paths = { "/tmp/skills" } } },
    },
  })

  MiniTest.expect.equality(ok, false)
  MiniTest.expect.equality(report.errors[1].path, "agents.codex.skills.paths")
end

T["setup"]["rejects removed full-content injection with migration guidance"] = function()
  local ok, report = capture_setup({ skills = { full_content = true } })

  MiniTest.expect.equality(ok, false)
  MiniTest.expect.equality(
    report.errors[1].message,
    'skills.full_content: validation failed (was removed; use skills.policy = "inject" for LouiseLM-managed skills)'
  )
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
    agents = { renamed_codex = { command = "codex-acp", transcript_layout = "codex" } },
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

return T
