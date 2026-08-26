local MiniTest = require("mini.test")
local Service = require("louiselm.workflow.service")
local T = MiniTest.new_set()

T["starts the constrained Park command"] = function()
  local command
  local callback
  local scheduled
  local previous = vim.schedule
  vim.schedule = function(fn)
    scheduled = fn
  end
  local started = assert(
    Service.park(
      { id = "run", session_id = "codex/session", claims = { "louiselm-qbr.3.3" }, expires_at_ms = 1 },
      function(ok)
        callback = ok
      end,
      function(value, _, done)
        command = value
        done({ code = 0, stderr = "" })
      end
    )
  )
  scheduled()
  vim.schedule = previous
  MiniTest.expect.equality(started, true)
  MiniTest.expect.equality(command, {
    "louiselm-capture",
    "run",
    "park",
    "--id",
    "run",
    "--session-id",
    "codex/session",
    "--claims",
    "louiselm-qbr.3.3",
    "--expires-at-ms",
    "1",
  })
  MiniTest.expect.equality(callback, true)
end

return T
