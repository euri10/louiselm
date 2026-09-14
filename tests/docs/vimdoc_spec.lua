local MiniTest = require("mini.test")

local Config = require("louiselm.config")
local Schema = require("louiselm.schema")
local Vimdoc = require("louiselm.docs.vimdoc")

local T = MiniTest.new_set()

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

T["vimdoc"] = MiniTest.new_set()

T["vimdoc"]["renders registered commands and schema configuration into help"] = function()
  local output = Vimdoc.generate(Config.schema, {
    LouiselmResume = { desc = "Resume a prior ACP Session", nargs = "?", bang = true },
    LouiselmChat = { desc = "Open the LouiseLM chat", nargs = "0", bang = false },
  }, {})

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
  }, {})

  MiniTest.expect.equality(output:find("Stable command", 1, true) ~= nil, true)
  MiniTest.expect.equality(output:find("Nightly command", 1, true) ~= nil, true)
end

T["vimdoc"]["matches the committed help and tags for registered commands"] = function()
  local health = require("louiselm.health")
  assert(health.configure({}, Config.schema))
  assert(health.register())
  local commands = nvim.api.nvim_get_commands({ builtin = false })
  local expected = Vimdoc.generate(Config.schema, commands, require("louiselm.ui.keymaps").defaults())
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

T["vimdoc"]["links only external tags Neovim's own help defines"] = function()
  local tags = "\n" .. table.concat(nvim.fn.readfile(nvim.env.VIMRUNTIME .. "/doc/tags"), "\n") .. "\n"

  for tag in pairs(Vimdoc.EXTERNAL_TAGS) do
    MiniTest.expect.equality(tags:find("\n" .. tag .. "\t", 1, true) ~= nil, true)
  end
end

T["vimdoc"]["opens on a table of contents linking every section"] = function()
  local output = Vimdoc.generate(Config.schema, {
    LouiselmChat = { desc = "Open the LouiseLM chat", nargs = "0", bang = false },
  }, {})

  MiniTest.expect.equality(output:find("*louiselm-contents*", 1, true) ~= nil, true)
  for _, tag in ipairs({
    "louiselm",
    "louiselm-sessions",
    "louiselm-mappings",
    "louiselm-commands",
    "louiselm-configuration",
    "louiselm-api",
    "louiselm-troubleshooting",
  }) do
    MiniTest.expect.equality(output:find("|" .. tag .. "|", 1, true) ~= nil, true)
  end
end

T["vimdoc"]["resolves every emitted cross-reference"] = function()
  local output = Vimdoc.generate(Config.schema, {
    LouiselmChat = { desc = "Open the LouiseLM chat", nargs = "0", bang = false },
  }, {})

  local tags = {}
  for tag in output:gmatch("%*(%S-)%*") do
    tags[tag] = true
  end
  for link in output:gmatch("|(%S-)|") do
    if not tags[link] and not Vimdoc.EXTERNAL_TAGS[link] then
      MiniTest.expect.equality("dangling |" .. link .. "|", "a resolvable tag")
    end
  end
end

T["vimdoc"]["keeps every emitted line inside 78 columns"] = function()
  local output = Vimdoc.generate(Config.schema, {
    LouiselmChat = { desc = "Open the LouiseLM chat", nargs = "0", bang = false },
  }, {})

  for line in output:gmatch("([^\n]*)\n") do
    if #line > 78 then
      MiniTest.expect.equality(#line .. " columns: " .. line, "at most 78 columns")
    end
  end
end

T["vimdoc"]["ends with a help modeline"] = function()
  local output = Vimdoc.generate(Config.schema, {}, {})

  MiniTest.expect.equality(output:find("vim:tw=78:ts=8:noet:ft=help:norl:", 1, true) ~= nil, true)
end

T["vimdoc"]["documents default mappings with a link to each command"] = function()
  local output = Vimdoc.generate(Config.schema, {
    LouiselmChat = { desc = "Open the LouiseLM chat", nargs = "0", bang = false },
  }, {
    { mode = "n", lhs = "<leader>lc", rhs = "<cmd>LouiselmChat<cr>", desc = "Louiselm chat" },
  })

  MiniTest.expect.equality(output:find("*louiselm-mappings*", 1, true) ~= nil, true)
  MiniTest.expect.equality(output:find("<leader>lc", 1, true) ~= nil, true)
  MiniTest.expect.equality(output:find("|:LouiselmChat|", 1, true) ~= nil, true)
end

T["vimdoc"]["documents a configuration example the schema accepts"] = function()
  local errors = Schema.validate(Config.schema, Vimdoc.setup_example())

  MiniTest.expect.equality(errors, {})
end

return T
