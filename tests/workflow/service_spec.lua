local MiniTest = require("mini.test")
local Service = require("louiselm.workflow.service")
---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim
local T = MiniTest.new_set()

T["builds a Run-scoped Agent environment"] = function()
  local environment = assert(Service.agent_environment({
    id = "11111111-1111-4111-8111-111111111111",
    token = "secret-token",
    database = "/tmp/project/.beads/beads.db",
  }, {
    shim = "/plugin/scripts/run-tools/br",
    br = "/usr/bin/br",
    capture = "/usr/bin/louiselm-capture",
    path = "/usr/bin",
  }))
  MiniTest.expect.equality(environment, {
    LOUISELM_RUN_ID = "11111111-1111-4111-8111-111111111111",
    LOUISELM_RUN_TOKEN = "secret-token",
    LOUISELM_REAL_BR = "/usr/bin/br",
    LOUISELM_CAPTURE = "/usr/bin/louiselm-capture",
    BEADS_DB = "/tmp/project/.beads/beads.db",
    PATH = "/plugin/scripts/run-tools:/usr/bin",
  })
end

T["starts the constrained Run admission command"] = function()
  local command
  local callback
  local scheduled
  local previous = nvim.schedule
  rawset(nvim, "schedule", function(fn)
    scheduled = fn
  end)
  local started = assert(Service.admit({
    id = "run",
    generated_work_max = 5,
    park_ttl_ms = 86400000,
  }, function(token, error_message)
    callback = { token, error_message }
  end, function(value, _, done)
    command = value
    done({ code = 0, stderr = "", stdout = '{"token":"run-token"}' })
    return true
  end))
  scheduled()
  rawset(nvim, "schedule", previous)
  MiniTest.expect.equality(started, true)
  MiniTest.expect.equality(command, {
    "louiselm-capture",
    "--require-interface=1",
    "run",
    "admit",
    "--id",
    "run",
    "--generated-work-max",
    "5",
    "--park-ttl-ms",
    "86400000",
  })
  MiniTest.expect.equality(callback, { "run-token", nil })
end

T["starts the constrained Session attachment command"] = function()
  local command
  assert(Service.attach({
    id = "run",
    session_id = "codex/session",
    agent = "codex",
    acp_session_id = "acp-session",
    cwd = "/tmp/project",
    load_session = true,
  }, function() end, function(value, _, done)
    command = value
    done({ code = 0, stderr = "" })
    return true
  end))
  MiniTest.expect.equality(command, {
    "louiselm-capture",
    "--require-interface=1",
    "run",
    "attach",
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
  })
end

T["starts the constrained Park command"] = function()
  local command
  local callback
  local scheduled
  local previous = nvim.schedule
  rawset(nvim, "schedule", function(fn)
    scheduled = fn
  end)
  local started = assert(Service.park({
    id = "run",
    session_id = "codex/session",
    agent = "codex",
    acp_session_id = "acp-session",
    cwd = "/tmp/project",
    load_session = true,
    claims = { "louiselm-qbr.3.3" },
  }, function(ok)
    callback = ok
  end, function(value, _, done)
    command = value
    done({ code = 0, stderr = "" })
    return true
  end))
  scheduled()
  rawset(nvim, "schedule", previous)
  MiniTest.expect.equality(started, true)
  MiniTest.expect.equality(command, {
    "louiselm-capture",
    "--require-interface=1",
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
  }, function() end, function()
    return true
  end)

  MiniTest.expect.equality(started, false)
  MiniTest.expect.equality(error_message, "Park requires an Agent that supports session/load")
end

T["accepts a cold Park with no live Beads claims"] = function()
  local command
  local scheduled
  local previous = nvim.schedule
  rawset(nvim, "schedule", function(fn)
    scheduled = fn
  end)
  assert(Service.park({
    id = "run",
    session_id = "codex/session",
    agent = "codex",
    acp_session_id = "acp-session",
    cwd = "/tmp/project",
    load_session = true,
    claims = {},
  }, function() end, function(value, _, done)
    command = value
    done({ code = 0, stderr = "" })
    return true
  end))
  scheduled()
  rawset(nvim, "schedule", previous)
  MiniTest.expect.equality(command[#command], "")
end

T["lists durable Parks asynchronously"] = function()
  local callback_value
  local scheduled
  local previous = nvim.schedule
  rawset(nvim, "schedule", function(fn)
    scheduled = fn
  end)
  assert(Service.list(function(runs, error_message)
    callback_value = { runs, error_message }
  end, function(_, _, done)
    done({
      code = 0,
      stderr = "",
      stdout = table.concat({
        '[{"id":"run","agent":"codex","acp_session_id":"acp","working_dir":"/tmp",',
        '"state":"cold_parked","parked_at_ms":0,"park_expires_at_ms":1,"generated_work":{"ceiling":5,',
        '"consumed":0,"reserved":0},"claims":["issue"]}]',
      }),
    })
    return true
  end))
  scheduled()
  rawset(nvim, "schedule", previous)
  MiniTest.expect.equality(callback_value, {
    {
      {
        id = "run",
        agent = "codex",
        acp_session_id = "acp",
        cwd = "/tmp",
        state = "cold_parked",
        parked_at_ms = 0,
        expires_at_ms = 1,
        generated_work = { ceiling = 5, consumed = 0, reserved = 0 },
        claims = { "issue" },
      },
    },
    nil,
  })
end

T["reports malformed durable Park data"] = function()
  local callback_value
  local scheduled
  local previous = nvim.schedule
  rawset(nvim, "schedule", function(fn)
    scheduled = fn
  end)
  assert(Service.list(function(runs, error_message)
    callback_value = { runs, error_message }
  end, function(_, _, done)
    done({ code = 0, stderr = "", stdout = '[{"state":"cold_parked"}]' })
    return true
  end))
  scheduled()
  rawset(nvim, "schedule", previous)
  MiniTest.expect.equality(callback_value, { {}, "cold Park service returned malformed data" })
end

return T
