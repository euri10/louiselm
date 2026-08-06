local MiniTest = require("mini.test")
local Schema = require("louiselm.schema")
local Validate = require("louiselm.schema.validate")

local T = MiniTest.new_set()

local function error_of_type(errors, error_type, path)
  for _, validation_error in ipairs(errors) do
    if validation_error.type == error_type and validation_error.path == path then
      return validation_error
    end
  end
  return nil
end

T["validate"] = MiniTest.new_set()

T["validate"]["collects unknown keys at any nesting depth"] = function()
  local schema = assert(Schema.define({
    agents = {
      type = "table",
      fields = {
        claude = {
          type = "table",
          fields = {
            model = { type = "string", default = "sonnet" },
          },
        },
      },
    },
  }))

  local errors = Validate.validate(schema, {
    agents = {
      claude = { modle = "sonnet" },
    },
  })

  local validation_error = assert(error_of_type(errors, "unknown_key", "agents.claude.modle"))
  MiniTest.expect.equality(validation_error.key, "modle")
  MiniTest.expect.equality(validation_error.suggestion, "model")
end

T["validate"]["reports root wrong type"] = function()
  local schema = assert(Schema.define({
    enabled = { type = "boolean", default = true },
  }))

  local errors = Validate.validate(schema, "enabled")

  local validation_error = assert(error_of_type(errors, "wrong_type", ""))
  MiniTest.expect.equality(validation_error.expected, "table")
  MiniTest.expect.equality(validation_error.got, "string")
  MiniTest.expect.equality(validation_error.example, {})
end

T["validate"]["collects multiple errors in one pass"] = function()
  local schema = assert(Schema.define({
    name = { type = "string" },
    retries = { type = "number" },
    agent = {
      type = "table",
      fields = {
        command = { type = "string" },
      },
    },
  }))

  local errors = Validate.validate(schema, {
    name = 42,
    retries = "many",
    agent = {
      extra = true,
    },
    settings = {},
  })

  MiniTest.expect.equality(#errors, 5)
  MiniTest.expect.equality(error_of_type(errors, "unknown_key", "agent.extra") ~= nil, true)
  MiniTest.expect.equality(error_of_type(errors, "missing_required", "agent.command") ~= nil, true)
  MiniTest.expect.equality(error_of_type(errors, "wrong_type", "name") ~= nil, true)
  MiniTest.expect.equality(error_of_type(errors, "wrong_type", "retries") ~= nil, true)
  MiniTest.expect.equality(error_of_type(errors, "unknown_key", "settings") ~= nil, true)
end

T["validate"]["reports custom validation failures"] = function()
  local schema = assert(Schema.define({
    retries = {
      type = "number",
      validator = function(value)
        return value >= 0, "must not be negative"
      end,
    },
  }))

  local errors = Validate.validate(schema, { retries = -1 })

  local validation_error = assert(error_of_type(errors, "validation_failed", "retries"))
  MiniTest.expect.equality(validation_error.message, "must not be negative")
end

T["validate"]["validates arrays and one-of fields"] = function()
  local schema = assert(Schema.define({
    args = {
      type = "array-of",
      items = { type = "string" },
    },
    value = {
      type = "one-of",
      options = {
        { type = "string" },
        { type = "number" },
      },
    },
  }))

  local errors = Validate.validate(schema, {
    args = { "ok", false },
    value = {},
  })

  MiniTest.expect.equality(#errors, 2)
  MiniTest.expect.equality(errors[1].path, "args[2]")
  MiniTest.expect.equality(errors[1].expected, "string")
  MiniTest.expect.equality(errors[2].path, "value")
  MiniTest.expect.equality(errors[2].expected, "string or number")
end

return T
