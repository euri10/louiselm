local MiniTest = require("mini.test")
local Louiselm = require("louiselm")
local Schema = require("louiselm.schema")

local T = MiniTest.new_set()

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local function capture_setup(config, schema)
  local notifications = {}
  ---@diagnostic disable-next-line: undefined-global
  local original_notify = vim.notify
  ---@diagnostic disable-next-line: undefined-global
  vim.notify = function(message, level)
    notifications[#notifications + 1] = { message = message, level = level }
  end
  local ok, result = Louiselm.setup(config, schema)
  ---@diagnostic disable-next-line: undefined-global
  vim.notify = original_notify
  return ok, result, notifications
end

T["setup"] = MiniTest.new_set()

T["setup"]["rejects invalid config and reports every error"] = function()
  local schema = assert(Schema.define({
    name = { type = "string" },
    retries = { type = "number" },
  }))

  local ok, report, notifications = capture_setup({ name = 42, extra = true }, schema)

  MiniTest.expect.equality(ok, false)
  MiniTest.expect.equality(report.count, 3)
  MiniTest.expect.equality(#notifications, 1)
  MiniTest.expect.equality(notifications[1].message, report.text)
  ---@diagnostic disable-next-line: undefined-global
  MiniTest.expect.equality(notifications[1].level, vim.log.levels.ERROR)
end

T["setup"]["starts with valid config"] = function()
  local schema = assert(Schema.define({
    name = { type = "string" },
  }))

  local ok, report, notifications = capture_setup({ name = "louiselm" }, schema)

  MiniTest.expect.equality(ok, true)
  MiniTest.expect.equality(report, nil)
  MiniTest.expect.equality(#notifications, 0)
end

T["setup"]["registers chat commands for a valid setup"] = function()
  pcall(nvim.api.nvim_del_user_command, "LouiselmResume")
  local schema = assert(Schema.define({
    name = { type = "string" },
  }))

  local ok = capture_setup({ name = "louiselm" }, schema)
  local commands = nvim.api.nvim_get_commands({ builtin = false })

  MiniTest.expect.equality(ok, true)
  MiniTest.expect.equality(commands.LouiselmResume ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmResume.bang, true)
end

T["setup"]["warns for present deprecated keys and still starts"] = function()
  local deprecation = assert(Schema.deprecated("old_name", {
    message = "old_name is obsolete",
    migration = "name",
  }))
  local schema = assert(Schema.define({
    old_name = {
      type = "string",
      deprecated = deprecation,
    },
  }))

  local ok, report, notifications = capture_setup({ old_name = "louiselm" }, schema)

  MiniTest.expect.equality(ok, true)
  MiniTest.expect.equality(report, nil)
  MiniTest.expect.equality(#notifications, 1)
  MiniTest.expect.equality(
    notifications[1].message,
    "deprecated key 'old_name': old_name is obsolete; migrate to 'name'"
  )
  ---@diagnostic disable-next-line: undefined-global
  MiniTest.expect.equality(notifications[1].level, vim.log.levels.WARN)
end

return T
