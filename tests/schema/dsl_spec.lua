local MiniTest = require("mini.test")
local Schema = require("louiselm.schema")

local T = MiniTest.new_set()

T["define"] = MiniTest.new_set()

T["define"]["normalizes primitive fields and metadata"] = function()
  local validator = function(value)
    return value ~= ""
  end
  local schema, err = Schema.define({
    name = {
      type = "string",
      default = "louiselm",
      description = "The display name.",
      validator = validator,
    },
    retries = { type = "number", default = 3 },
    enabled = { type = "boolean", default = true },
  })

  assert(schema ~= nil, err)
  assert(err == nil)
  MiniTest.expect.equality(schema.type, "table")
  MiniTest.expect.equality(schema.fields.name.type, "string")
  MiniTest.expect.equality(schema.fields.name.default, "louiselm")
  MiniTest.expect.equality(schema.fields.name.description, "The display name.")
  MiniTest.expect.equality(schema.fields.name.validator, validator)
  MiniTest.expect.equality(schema.fields.retries.type, "number")
  MiniTest.expect.equality(schema.fields.enabled.type, "boolean")
end

T["define"]["normalizes nested, array, and one-of fields"] = function()
  local schema, err = Schema.define({
    agent = {
      type = "table",
      fields = {
        command = { type = "string" },
        args = {
          type = "array-of",
          items = { type = "string" },
        },
      },
    },
    value = {
      type = "one-of",
      options = {
        { type = "string" },
        { type = "number" },
      },
    },
  })

  assert(schema ~= nil, err)
  assert(err == nil)
  MiniTest.expect.equality(schema.fields.agent.type, "table")
  MiniTest.expect.equality(schema.fields.agent.fields.command.type, "string")
  MiniTest.expect.equality(schema.fields.agent.fields.args.type, "array-of")
  MiniTest.expect.equality(schema.fields.agent.fields.args.items.type, "string")
  MiniTest.expect.equality(schema.fields.value.type, "one-of")
  MiniTest.expect.equality(schema.fields.value.options[1].type, "string")
  MiniTest.expect.equality(schema.fields.value.options[2].type, "number")
end

T["define"]["rejects malformed descriptions"] = function()
  local schema, err = Schema.define({
    broken = { type = "unknown" },
  })

  MiniTest.expect.equality(schema, nil)
  MiniTest.expect.equality(err, "field 'broken': unsupported type 'unknown'")
end

return T
