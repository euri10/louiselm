local MiniTest = require("mini.test")
local Report = require("louiselm.schema.report")

local T = MiniTest.new_set()

T["format"] = MiniTest.new_set()

T["format"]["formats five simultaneous errors"] = function()
  local errors = {
    {
      type = "unknown_key",
      path = "agents.claude.modle",
      key = "modle",
      suggestion = "model",
    },
    {
      type = "wrong_type",
      path = "agents.claude.retries",
      expected = "number",
      got = "string",
      example = 3,
    },
    {
      type = "missing_required",
      path = "agents.claude.command",
      key = "command",
    },
    {
      type = "validation_failed",
      path = "agents.claude.model",
      message = "must be supported",
    },
    {
      type = "unknown_key",
      path = "settings",
      key = "settings",
    },
  }

  local report = Report.format(errors)

  MiniTest.expect.equality(report.ok, false)
  MiniTest.expect.equality(report.count, 5)
  MiniTest.expect.equality(#report.errors, 5)
  MiniTest.expect.equality(#report.lines, 5)
  MiniTest.expect.equality(report.text, table.concat(report.lines, "\n"))
  MiniTest.expect.equality(errors[1].message, nil)
  MiniTest.expect.equality(report.errors[1].message, "agents.claude.modle: unknown key; did you mean 'model'?")
  MiniTest.expect.equality(
    report.errors[2].message,
    "agents.claude.retries: wrong type (expected number, got string; example: 3)"
  )
  MiniTest.expect.equality(report.errors[3].message, "agents.claude.command: missing required key")
  MiniTest.expect.equality(report.errors[4].message, "agents.claude.model: validation failed (must be supported)")
  MiniTest.expect.equality(report.errors[5].message, "settings: unknown key")
end

T["format"]["returns an empty successful report without errors"] = function()
  local report = Report.format({})

  MiniTest.expect.equality(report.ok, true)
  MiniTest.expect.equality(report.count, 0)
  MiniTest.expect.equality(report.errors, {})
  MiniTest.expect.equality(report.lines, {})
  MiniTest.expect.equality(report.text, "")
end

return T
