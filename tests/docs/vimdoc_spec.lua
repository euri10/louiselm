local MiniTest = require("mini.test")

local Config = require("louiselm.config")
local Vimdoc = require("louiselm.docs.vimdoc")

local T = MiniTest.new_set()

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

T["vimdoc"] = MiniTest.new_set()

T["vimdoc"]["renders registered commands and schema configuration into help"] = function()
  local output = Vimdoc.generate(Config.schema, {
    LouiselmResume = { desc = "Resume a prior ACP Session", nargs = "?", bang = true },
    LouiselmChat = { desc = "Open the LouiseLM chat", nargs = "0", bang = false },
  })

  MiniTest.expect.equality(output:find("*louiselm.txt*", 1, true) ~= nil, true)
  MiniTest.expect.equality(output:find("*louiselm-sessions*", 1, true) ~= nil, true)
  MiniTest.expect.equality(output:find("*louiselm-workflow*", 1, true) ~= nil, true)
  MiniTest.expect.equality(output:find("*louiselm-permissions*", 1, true) ~= nil, true)
  MiniTest.expect.equality(output:find("*louiselm-troubleshooting*", 1, true) ~= nil, true)
  MiniTest.expect.equality(output:find("*louiselm-commands*", 1, true) ~= nil, true)
  MiniTest.expect.equality(output:find("*:LouiselmChat*", 1, true) ~= nil, true)
  MiniTest.expect.equality(output:find(":LouiselmChat", 1, true) ~= nil, true)
  MiniTest.expect.equality(output:find(":LouiselmResume[!]", 1, true) ~= nil, true)
  MiniTest.expect.equality(output:find("Arguments: optional", 1, true) ~= nil, true)
  MiniTest.expect.equality(output:find("*louiselm-configuration*", 1, true) ~= nil, true)
  MiniTest.expect.equality(output:find("*louiselm-config-agents*", 1, true) ~= nil, true)
  MiniTest.expect.equality(output:sub(-2) ~= "\n\n", true)
end

T["vimdoc"]["accepts command descriptions from stable and nightly Neovim fields"] = function()
  local output = Vimdoc.generate(Config.schema, {
    LouiselmStable = { definition = "Stable command", nargs = "0", bang = false },
    LouiselmNightly = { definition = "", desc = "Nightly command", nargs = "0", bang = false },
  })

  MiniTest.expect.equality(output:find("Stable command", 1, true) ~= nil, true)
  MiniTest.expect.equality(output:find("Nightly command", 1, true) ~= nil, true)
end

T["vimdoc"]["matches the committed help and tags for registered commands"] = function()
  local health = require("louiselm.health")
  assert(health.configure({}, Config.schema))
  assert(health.register())
  local commands = nvim.api.nvim_get_commands({ builtin = false })
  local expected = Vimdoc.generate(Config.schema, commands)
  health.reset()
  local actual = table.concat(nvim.fn.readfile("doc/louiselm.txt"), "\n") .. "\n"
  local tags = table.concat(nvim.fn.readfile("doc/tags"), "\n")

  MiniTest.expect.equality(actual, expected)
  MiniTest.expect.equality(tags:find("louiselm\tlouiselm.txt", 1, true) ~= nil, true)
  MiniTest.expect.equality(tags:find("louiselm-commands\tlouiselm.txt", 1, true) ~= nil, true)
  MiniTest.expect.equality(tags:find("louiselm-workflow\tlouiselm.txt", 1, true) ~= nil, true)
  MiniTest.expect.equality(tags:find(":LouiselmChat\tlouiselm.txt", 1, true) ~= nil, true)
  MiniTest.expect.equality(tags:find(":LouiselmPreflight\tlouiselm.txt", 1, true) ~= nil, true)
end

return T
