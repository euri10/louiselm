local MiniTest = require("mini.test")
local Config = require("louiselm.config")
local Generator = require("louiselm.schema.gen_luacats")
local Schema = require("louiselm.schema")

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local T = MiniTest.new_set()

T["generate"] = MiniTest.new_set()

T["generate"]["emits deterministic nested LuaCATS classes"] = function()
  local schema = assert(Schema.define({
    agent = {
      type = "table",
      description = "Agent configuration.",
      fields = {
        command = { type = "string", description = "Executable." },
        args = { type = "array-of", items = { type = "string" } },
      },
    },
    enabled = { type = "boolean", default = true },
    value = {
      type = "one-of",
      options = {
        { type = "string" },
        { type = "number" },
      },
    },
  }))

  local output = Generator.generate(schema)

  MiniTest.expect.equality(
    output,
    table.concat({
      "---@class louiselm.Config",
      "---@field agent louiselm.ConfigAgent Agent configuration.",
      "---@field enabled? boolean",
      "---@field value string|number",
      "",
      "---@class louiselm.ConfigAgent",
      "---@field args string[]",
      "---@field command string Executable.",
      "",
    }, "\n")
  )
end

T["generate"]["emits primitive root annotations"] = function()
  local schema = assert(Schema.define({
    retries = { type = "number" },
    name = { type = "string", default = "louiselm" },
  }))

  local output = Generator.generate(schema)

  MiniTest.expect.equality(output:find("---@class louiselm.Config", 1, true) ~= nil, true)
  MiniTest.expect.equality(output:find("---@field name? string", 1, true) ~= nil, true)
  MiniTest.expect.equality(output:find("---@field retries number", 1, true) ~= nil, true)
end

T["committed types"] = MiniTest.new_set()

-- The generator shipped in 2026-08-06 with no artifact and no consumer: users got
-- no `setup()` completion for months while its unit tests stayed green
-- (louiselm-luacats-generator-inert-5y8i). This case is the gate that keeps the
-- committed file real, mirroring `--check` in scripts/generate-luacats.
T["committed types"]["lua/louiselm/types.lua matches the config schema"] = function()
  local path = nvim.fn.getcwd() .. "/lua/louiselm/types.lua"

  MiniTest.expect.equality(nvim.fn.filereadable(path), 1)
  MiniTest.expect.equality(table.concat(nvim.fn.readfile(path), "\n") .. "\n", Generator.generate(Config.schema))
end

return T
