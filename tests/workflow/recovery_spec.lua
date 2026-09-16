local MiniTest = require("mini.test")
local Recovery = require("louiselm.workflow.recovery")
local RunClient = require("louiselm.workflow.run_client")
local Service = require("louiselm.workflow.service")
local T = MiniTest.new_set()

---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim

-- RunClient marshals its real libuv callbacks onto the editor loop.
local function deliver(callback)
  local delivered = false
  local async
  async = nvim.uv.new_async(function()
    async:close()
    nvim.schedule(function()
      callback()
      delivered = true
    end)
  end)
  async:send()
  assert(nvim.wait(1000, function()
    return delivered
  end))
end

local function fixture()
  local f = { connected = 0, closed = 0, listed = 0, results = {} }
  local read, connect, list = RunClient.read_operator_capability, RunClient.connect, Service.list
  local owner = Recovery.new({
    enabled = true,
    attention = true,
    beads = true,
    load_session = function() end,
    find_run = function() end,
    is_live = function()
      return true
    end,
    on_park = function() end,
    on_error = function(message)
      f.error = message
    end,
  })
  rawset(RunClient, "read_operator_capability", function(_, callback)
    f.read = callback
    return true
  end)
  rawset(RunClient, "connect", function(_, callback, options)
    f.connected = f.connected + 1
    f.snapshot = callback
    f.fail = options.on_error
    f.client = {
      pipe = {
        is_closing = function()
          return false
        end,
      },
      dispose = function()
        f.closed = f.closed + 1
        return true
      end,
    }
    return f.client
  end)
  rawset(Service, "list", function(callback)
    f.listed = f.listed + 1
    nvim.schedule(function()
      callback({})
    end)
    return true
  end)
  MiniTest.finally(function()
    owner:dispose()
    rawset(RunClient, "read_operator_capability", read)
    rawset(RunClient, "connect", connect)
    rawset(Service, "list", list)
  end)
  f.owner = owner
  f.result = function(runs, err)
    f.results[#f.results + 1] = { runs = runs, error = err }
  end
  return f
end

T["requires explicit composite opt-ins before any recovery I/O"] = function()
  for _, options in ipairs({ {}, { enabled = true }, { enabled = true, attention = true } }) do
    local owner = Recovery.new(options)
    local started, err = owner:list(function()
      error("disabled recovery must not call its completion")
    end)
    MiniTest.expect.equality(started, false)
    MiniTest.expect.equality(assert(err):find("enabled = true", 1, true) ~= nil, true)
    local session = {
      inspect = function()
        error("prerequisites must be checked before examining a Session")
      end,
    }
    local parked, park_error = owner:park(session, function() end)
    MiniTest.expect.equality(parked, false)
    MiniTest.expect.equality(park_error, err)
    owner:dispose()
  end
end

T["checks Agent resume capability and history before service admission"] = function()
  for _, case in ipairs({
    { load = false, turns = 1, message = "supports session/load" },
    { load = true, turns = 0, message = "sent at least one prompt" },
  }) do
    local options = { enabled = true, attention = true, beads = true }
    local owner = Recovery.new(options)
    local session = {
      client = { agent_capabilities = { loadSession = case.load } },
      inspect = function()
        return { agent = "agent", acp_session_id = "acp", working_dir = "/tmp", current_turn = case.turns }
      end,
    }
    local started, err = owner:park(session, function()
      error("ineligible Sessions must fail before asynchronous effects")
    end)
    MiniTest.expect.equality(started, false)
    MiniTest.expect.equality(assert(err):find(case.message, 1, true) ~= nil, true)
    owner:dispose()
  end
end

T["shares initialization and releases its connected client"] = function()
  local f = fixture()
  assert(f.owner:list(f.result))
  assert(f.owner:list(f.result))
  deliver(function()
    f.read("capability")
    f.snapshot({})
  end)
  assert(nvim.wait(1000, function()
    return #f.results == 2
  end))
  MiniTest.expect.equality({ f.connected, f.listed }, { 1, 2 })
  f.owner:dispose()
  f.owner:dispose()
  MiniTest.expect.equality(f.closed, 1)
end

T["disposal during capability read prevents a late connection"] = function()
  local f = fixture()
  assert(f.owner:list(f.result))
  f.owner:dispose()
  deliver(function()
    f.read("capability")
  end)
  MiniTest.expect.equality(f.connected, 0)
  MiniTest.expect.equality(#f.results, 0)
end

T["owns the connecting client before the first snapshot"] = function()
  local f = fixture()
  assert(f.owner:list(f.result))
  deliver(function()
    f.read("capability")
  end)
  f.owner:dispose()
  MiniTest.expect.equality(f.closed, 1)
  f.snapshot({})
  MiniTest.expect.equality(f.listed, 0)
end

T["failed initialization releases the client and permits retry"] = function()
  local f = fixture()
  assert(f.owner:list(f.result))
  deliver(function()
    f.read("capability")
    f.fail("connection failed")
  end)
  MiniTest.expect.equality(f.closed, 1)
  MiniTest.expect.equality(f.results, { { error = "connection failed" } })
  assert(f.owner:list(f.result))
  deliver(function()
    f.read("capability")
    f.snapshot({})
  end)
  assert(nvim.wait(1000, function()
    return #f.results == 2
  end))
  MiniTest.expect.equality(f.connected, 2)
end

T["reconnects after receiver restart and waits for fresh revisions before listing"] = function()
  local f = fixture()
  assert(f.owner:list(f.result))
  deliver(function()
    f.read("old-capability")
    f.snapshot({})
  end)
  assert(nvim.wait(1000, function()
    return #f.results == 1
  end))
  local old_snapshot, old_error = f.snapshot, f.fail
  f.client.pipe.is_closing = function()
    return true
  end
  deliver(function()
    f.fail("Run socket disconnected")
  end)
  assert(f.owner:list(f.result))
  assert(f.owner:list(f.result))
  MiniTest.expect.equality(f.listed, 1)
  deliver(function()
    f.read("new-capability")
  end)
  MiniTest.expect.equality(f.connected, 2)
  MiniTest.expect.equality(f.closed, 1)
  MiniTest.expect.equality(f.listed, 1)
  local result
  function f.client:resume(id, revision, _, callback)
    result = { id, revision }
    nvim.schedule(function()
      callback(nil, "fixture stops before mutation")
    end)
    return true
  end
  deliver(function()
    old_snapshot({})
    old_error("late old connection error")
    f.snapshot({
      {
        id = "run",
        revision = 7,
        state = "cold_parked",
        generated_work_ceiling = 1,
        generated_work_consumed = 0,
        generated_work_reserved = 0,
        pending_mutation_ids = {},
        park_expires_at_ms = 60000,
      },
    })
  end)
  assert(nvim.wait(1000, function()
    return #f.results == 3
  end))
  MiniTest.expect.equality(f.error, "Run socket disconnected")
  local completed
  assert(f.owner:resume({
    id = "run",
    agent = "codex",
    acp_session_id = "acp",
    cwd = "/tmp",
    state = "cold_parked",
    parked_at_ms = 100,
    expires_at_ms = 60000,
    generated_work = { ceiling = 1, consumed = 0, reserved = 0 },
    claims = {},
  }, function(_, err)
    completed = err
  end))
  assert(nvim.wait(1000, function()
    return completed ~= nil
  end))
  MiniTest.expect.equality(result, { "run", 7 })
  MiniTest.expect.equality(completed, "fixture stops before mutation")
end

local function loading_fixture(options)
  options = options or {}
  local f = fixture()
  local summary = {
    id = "run",
    agent = "codex",
    acp_session_id = "acp",
    cwd = "/tmp",
    state = "cold_parked",
    expires_at_ms = 60000,
    generated_work = { ceiling = 1, consumed = 0, reserved = 0 },
    claims = {},
  }
  local run = {
    id = "run",
    revision = 1,
    state = "cold_parked",
    generated_work_ceiling = 1,
    generated_work_consumed = 0,
    generated_work_reserved = 0,
    pending_mutation_ids = {},
    park_expires_at_ms = 60000,
  }
  f.session = {
    disposed = false,
    inspect = function()
      return { status = "ready", agent = "codex", acp_session_id = "acp" }
    end,
    cancel = function()
      return true
    end,
    dispose = function(self)
      self.disposed = true
      return true
    end,
  }
  if options.retained then
    f.run = assert(require("louiselm.workflow.run").new({ id = "run" }))
    assert(f.run:adopt_session(f.session))
    assert(f.run:accept_park())
    f.owner.options.find_run = function()
      return f.run
    end
  end
  f.loads = 0
  f.owner.options.load_session = function(_, _, load_options, callback)
    f.loads = f.loads + 1
    f.ready = callback
    f.event = load_options.on_event
    if f.run ~= nil then
      callback(nil, "session is already open in another client")
      return nil, "session is already open in another client"
    end
    if options.synchronous_failure then
      callback(nil, "startup failed")
      return nil, "startup failed"
    end
    return f.session
  end
  assert(f.owner:list(f.result))
  f.read("capability")
  function f.client:resume(_, _, _, callback)
    nvim.schedule(function()
      run.state = "resuming"
      callback(run)
    end)
    return true
  end
  function f.client:finalize_resume(_, _, _, succeeded, callback)
    f.finalizations = (f.finalizations or 0) + 1
    f.finalized = succeeded
    local function complete()
      if options.finalization_failure then
        callback(nil, "Run changed while resuming")
        return
      end
      run.state = succeeded and "active" or "cold_parked"
      callback(run)
    end
    if options.defer_finalization then
      f.complete_finalization = complete
    else
      nvim.schedule(complete)
    end
    return true
  end
  f.snapshot({ run })
  assert(f.owner:resume(summary, function(result, err)
    f.completions = (f.completions or 0) + 1
    f.resumed = result
    f.resume_error = err
  end))
  assert(nvim.wait(1000, function()
    return f.ready ~= nil or f.finalizations ~= nil
  end))
  return f
end

T["immediate startup failure finalizes and completes resume once"] = function()
  local f = loading_fixture({ synchronous_failure = true })
  assert(nvim.wait(1000, function()
    return f.resume_error ~= nil
  end))
  MiniTest.expect.equality(f.resume_error, "startup failed")
  MiniTest.expect.equality({ f.finalizations, f.completions }, { 1, 1 })
end

T["same-editor resume reuses its retained Session and finalizes the durable Run"] = function()
  -- Physical QA Run ab88e3e6-f03f-4344-a067-caf6ac8cd61b, 2026-09-16:
  -- Park retained its ACP writer; a second session/load was rejected (.9.9.9).
  local f = loading_fixture({ retained = true })
  assert(nvim.wait(1000, function()
    return f.completions ~= nil
  end))
  MiniTest.expect.equality(f.loads, 0)
  MiniTest.expect.equality(f.resume_error, nil)
  MiniTest.expect.equality(f.resumed.session, f.session)
  MiniTest.expect.equality(f.resumed.relay, nil)
  MiniTest.expect.equality(f.finalized, true)
  MiniTest.expect.equality(f.run.status, "active")
  f.owner:dispose()
  MiniTest.expect.equality(f.session.disposed, false)
end

T["failed retained-Session finalization preserves its caller-owned Session"] = function()
  local f = loading_fixture({ retained = true, finalization_failure = true })
  assert(nvim.wait(1000, function()
    return f.completions ~= nil
  end))
  MiniTest.expect.equality(f.loads, 0)
  MiniTest.expect.equality(f.resume_error, "Run changed while resuming")
  MiniTest.expect.equality(f.resumed, nil)
  MiniTest.expect.equality(f.run.status, "parked")
  f.owner:dispose()
  MiniTest.expect.equality(f.session.disposed, false)
end

T["disposal during retained-Session finalization leaves its caller-owned Session alive"] = function()
  local f = loading_fixture({ retained = true, defer_finalization = true })
  MiniTest.expect.equality(f.loads, 0)
  f.owner:dispose()
  deliver(f.complete_finalization)
  MiniTest.expect.equality(f.completions, nil)
  MiniTest.expect.equality(f.session.disposed, false)
end

T["failed asynchronous admission preserves its error and never attaches"] = function()
  local f = fixture()
  -- louiselm-dont9: keep the real async query without host br or its workspace.
  local directory = nvim.fn.tempname()
  local original_path = nvim.env.PATH
  MiniTest.finally(function()
    nvim.env.PATH = original_path
    assert(nvim.fn.delete(directory, "rf") == 0)
  end)
  assert(nvim.fn.mkdir(directory, "p", 448) == 1)
  assert(nvim.fn.writefile({
    "#!/bin/sh",
    '[ "$*" = "list --assignee qa/admission-failure --status in_progress --json" ] || exit 1',
    [[printf '%s\n' '{"issues":[]}']],
  }, directory .. "/br") == 0)
  assert(nvim.fn.setfperm(directory .. "/br", "rwx------") == 1)
  nvim.env.PATH = directory
  local admit, attach = Service.admit, Service.attach
  local attached = false
  local session = {
    client = { agent_capabilities = { loadSession = true } },
    inspect = function()
      return { agent = "qa", acp_session_id = "admission-failure", working_dir = directory, current_turn = 1 }
    end,
  }
  rawset(Service, "admit", function(_, callback)
    nvim.schedule(function()
      callback(nil, "admission unavailable")
    end)
    return true
  end)
  rawset(Service, "attach", function(_, callback)
    attached = true
    nvim.schedule(function()
      callback(false, "attachment should not happen")
    end)
    return true
  end)
  MiniTest.finally(function()
    rawset(Service, "admit", admit)
    rawset(Service, "attach", attach)
  end)
  local failure
  assert(f.owner:park(session, function(_, err)
    failure = err
  end))
  deliver(function()
    f.read("capability")
    f.snapshot({})
  end)
  assert(nvim.wait(3000, function()
    return failure ~= nil
  end))
  MiniTest.expect.equality(failure, "admission unavailable")
  MiniTest.expect.equality(attached, false)
  MiniTest.expect.equality(session.owner_run, nil)
end

T["failed load disposes the starting Session even when the callback returns nil"] = function()
  local f = loading_fixture()
  -- louiselm-rqqp: production load failure returns nil, not the starting Session.
  f.ready(nil, "session/load failed")
  assert(nvim.wait(1000, function()
    return f.resume_error ~= nil
  end))
  MiniTest.expect.equality(f.resume_error, "session/load failed")
  MiniTest.expect.equality(f.finalized, false)
  MiniTest.expect.equality(f.session.disposed, true)
end

T["disposal owns the Session before its load callback and ignores late results"] = function()
  local f = loading_fixture()
  f.owner:dispose()
  MiniTest.expect.equality(f.session.disposed, true)
  f.event({ type = "chunk", session_id = "loaded", data = { content = { type = "text", text = "late" } } })
  f.ready(f.session)
  MiniTest.expect.equality(f.resumed, nil)
  MiniTest.expect.equality(f.finalized, nil)
end

return T
