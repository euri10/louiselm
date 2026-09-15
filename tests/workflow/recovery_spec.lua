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

local function loading_fixture(synchronous_failure)
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
      return { status = "ready" }
    end,
    dispose = function(self)
      self.disposed = true
      return true
    end,
  }
  f.owner.options.load_session = function(_, _, options, callback)
    f.ready = callback
    f.event = options.on_event
    if synchronous_failure then
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
    nvim.schedule(function()
      callback(run)
    end)
    return true
  end
  f.snapshot({ run })
  assert(f.owner:resume(summary, function(result, err)
    f.completions = (f.completions or 0) + 1
    f.resumed = result
    f.resume_error = err
  end))
  assert(nvim.wait(1000, function()
    return f.ready ~= nil
  end))
  return f
end

T["immediate startup failure finalizes and completes resume once"] = function()
  local f = loading_fixture(true)
  assert(nvim.wait(1000, function()
    return f.resume_error ~= nil
  end))
  MiniTest.expect.equality(f.resume_error, "startup failed")
  MiniTest.expect.equality({ f.finalizations, f.completions }, { 1, 1 })
end

T["failed asynchronous admission preserves its error and never attaches"] = function()
  local f = fixture()
  local admit, attach = Service.admit, Service.attach
  local attached = false
  local session = {
    inspect = function()
      return { agent = "qa", acp_session_id = "admission-failure", working_dir = nvim.fn.getcwd() }
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
