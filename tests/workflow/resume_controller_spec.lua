local MiniTest = require("mini.test")
local ResumeController = require("louiselm.workflow.resume_controller")
local Workflow = require("louiselm.workflow")
local T = MiniTest.new_set()

local function worker(status)
  local value = { status = status, disposed = 0 }
  function value:inspect()
    return { status = self.status }
  end
  function value:cancel()
    self.status = "ready"
    return true
  end
  function value:dispose()
    self.disposed = self.disposed + 1
    return true
  end
  return value
end

local function parked_run()
  local run = assert(Workflow.new_run())
  assert(run:accept_park())
  return run
end

local function view(revision, state)
  return {
    id = "run",
    revision = revision,
    state = state,
    generated_work_ceiling = 1,
    generated_work_consumed = 1,
    generated_work_reserved = 0,
    pending_mutation_ids = {},
    park_expires_at_ms = 0,
  }
end

T["keeps raise separate and resumes a warm Run directly"] = function()
  local run = parked_run()
  local raised
  local client = {}
  function client:raise(id, revision, ceiling, callback)
    MiniTest.expect.equality({ id, revision, ceiling }, { "run", 2, 4 })
    callback(view(3, "parked"))
    return true
  end
  function client:resume(id, revision, operation_id, callback)
    MiniTest.expect.equality({ id, revision, operation_id }, { "run", 3, "operation" })
    callback(view(4, "active"))
    return true
  end
  local controller = assert(ResumeController.new({
    client = client,
    find_run = function()
      return run
    end,
    load_cold = function()
      error("warm resume must not load")
    end,
    operation_id = function(callback)
      callback("operation")
    end,
  }))
  assert(controller:raise(view(2, "parked"), 4, function(value)
    raised = value
  end))
  MiniTest.expect.equality(raised.revision, 3)
  local resumed
  assert(controller:resume(raised, function(value)
    resumed = value
  end))
  MiniTest.expect.equality(resumed.state, "active")
  MiniTest.expect.equality(run.status, "active")
end

T["loads cold state before finalizing active"] = function()
  local run = assert(Workflow.new_run())
  local session = worker("ready")
  assert(run:adopt_session(session))
  local client = {}
  function client:resume(_, _, operation_id, callback)
    MiniTest.expect.equality(operation_id, "operation")
    callback(view(6, "resuming"))
    return true
  end
  function client:finalize_resume(id, revision, operation_id, succeeded, callback)
    MiniTest.expect.equality({ id, revision, operation_id, succeeded }, { "run", 6, "operation", true })
    callback(view(7, "active"))
    return true
  end
  local controller = assert(ResumeController.new({
    client = client,
    find_run = function()
      return run
    end,
    load_cold = function(_, callback)
      callback(session)
    end,
    operation_id = function(callback)
      callback("operation")
    end,
  }))
  local result
  assert(controller:resume(view(5, "cold_parked"), function(value)
    result = value
  end))
  MiniTest.expect.equality(result.state, "active")
  MiniTest.expect.equality(run.status, "active")
end

T["finalizes a failed cold load without reporting the Run active"] = function()
  local finalized
  local client = {}
  function client:resume(_, _, _, callback)
    callback(view(6, "resuming"))
    return true
  end
  function client:finalize_resume(_, _, _, succeeded, callback)
    finalized = succeeded
    callback(view(7, "cold_parked"))
    return true
  end
  local controller = assert(ResumeController.new({
    client = client,
    find_run = function()
      return nil
    end,
    load_cold = function(_, callback)
      callback(nil, "session/load failed")
    end,
    operation_id = function(callback)
      callback("operation")
    end,
  }))
  local result
  local failure
  assert(controller:resume(view(5, "cold_parked"), function(value, error_message)
    result = value
    failure = error_message
  end))
  MiniTest.expect.equality(finalized, false)
  MiniTest.expect.equality(result, nil)
  MiniTest.expect.equality(failure, "session/load failed")
end

T["disposes the loaded Session when finalization fails"] = function()
  local session = worker("ready")
  local client = {}
  function client:resume(_, _, _, callback)
    callback(view(6, "resuming"))
    return true
  end
  function client:finalize_resume(_, _, _, _, callback)
    callback(nil, "Run changed while loading")
    return true
  end
  local controller = assert(ResumeController.new({
    client = client,
    find_run = function()
      return nil
    end,
    load_cold = function(_, callback)
      callback(session)
    end,
    operation_id = function(callback)
      callback("operation")
    end,
  }))
  local failure
  assert(controller:resume(view(5, "cold_parked"), function(result, error_message)
    MiniTest.expect.equality(result, nil)
    failure = error_message
  end))
  MiniTest.expect.equality(session.disposed, 1)
  MiniTest.expect.equality(failure, "Run changed while loading")
end

T["disposes reconstruction delivered after controller disposal"] = function()
  local complete_load
  local session = worker("ready")
  local client = {}
  function client:resume(_, _, _, callback)
    callback(view(2, "resuming"))
    return true
  end
  local controller = assert(ResumeController.new({
    client = client,
    find_run = function()
      return nil
    end,
    load_cold = function(_, callback)
      complete_load = callback
    end,
    operation_id = function(callback)
      callback("operation")
    end,
  }))
  assert(controller:resume(view(1, "cold_parked"), function()
    error("late callback")
  end))
  assert(controller:dispose())
  complete_load(session)
  MiniTest.expect.equality(session.disposed, 1)
end

return T
