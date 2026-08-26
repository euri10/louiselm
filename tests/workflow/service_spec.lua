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
  local started = assert(Service.park({
    id = "run",
    session_id = "codex/session",
    agent = "codex",
    acp_session_id = "acp-session",
    cwd = "/tmp/project",
    load_session = true,
    claims = { "louiselm-qbr.3.3" },
    expires_at_ms = 1,
  }, function(ok)
    callback = ok
  end, function(value, _, done)
    command = value
    return done({ code = 0, stderr = "" })
  end))
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
    "--agent",
    "codex",
    "--acp-session-id",
    "acp-session",
    "--cwd",
    "/tmp/project",
    "--load-session",
    "true",
    "--claims",
    "louiselm-qbr.3.3",
    "--expires-at-ms",
    "1",
  })
  MiniTest.expect.equality(callback, true)
end

T["rejects cold Park without session/load admission"] = function()
  local started, error_message = Service.park({
    id = "run",
    session_id = "codex/session",
    agent = "codex",
    acp_session_id = "acp-session",
    cwd = "/tmp/project",
    load_session = false,
    claims = { "louiselm-qbr.3.3" },
    expires_at_ms = 1,
  }, function() end, function()
    return true
  end)

  MiniTest.expect.equality(started, false)
  MiniTest.expect.equality(error_message, "Park requires an Agent that supports session/load")
end

return T
