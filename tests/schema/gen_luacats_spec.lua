local MiniTest = require("mini.test")
local Generator = require("louiselm.schema.gen_luacats")
local Schema = require("louiselm.schema")

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

return T
