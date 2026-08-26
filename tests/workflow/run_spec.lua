local MiniTest = require("mini.test")
local Workflow = require("louiselm.workflow")

local T = MiniTest.new_set()

local function worker(status)
  local value = { status = status, cancelled = 0, disposed = 0 }
  function value:inspect()
    return { status = self.status }
  end
  function value:cancel()
    self.cancelled = self.cancelled + 1
    if self.status == "prompting" then
      self.status = "ready"
    end
    return true
  end
  function value:dispose()
    self.disposed = self.disposed + 1
    self.status = "disposed"
    return true
  end
  return value
end

T["ownership"] = MiniTest.new_set()

T["ownership"]["synthesizes a reserved Park outcome on every stage"] = function()
  local manifest = {
    first = { workflow = "reference", outcomes = {} },
    other = { workflow = "other", outcomes = {} },
  }

  local synthesized = Workflow.synthesize("reference", manifest)

  MiniTest.expect.equality(#synthesized.first.outcomes, 1)
  MiniTest.expect.equality(synthesized.first.outcomes[1], {
    name = "__louiselm_park",
    resolver = "human",
    terminal = true,
  })
  MiniTest.expect.equality(#manifest.first.outcomes, 0)
  MiniTest.expect.equality(synthesized.other, manifest.other)
end

T["ownership"]["validates the synthesized graph with its mandatory escape"] = function()
  local result = Workflow.validate("reference", {
    first = { workflow = "reference", entry = true, outcomes = {} },
  })

  MiniTest.expect.equality(result.ok, true)
end

T["ownership"]["adopts every worker and exposes its Run owner"] = function()
  local run = assert(Workflow.new_run())
  local session = worker("ready")

  assert(run:adopt_session(session))

  MiniTest.expect.equality(session.owner_run, run)
  MiniTest.expect.equality(#run.workers, 1)
end

T["ownership"]["creates stage Sessions through the Run owner"] = function()
  local session = worker("ready")
  local api = {
    create_session = function(_, _, _, callback)
      callback(session)
      return session
    end,
  }
  local run = assert(Workflow.new_run({ session_api = api }))

  local created = assert(run:create_session("stage-agent"))

  MiniTest.expect.equality(created, session)
  MiniTest.expect.equality(session.owner_run, run)
  MiniTest.expect.equality(#run.workers, 1)
end

T["cancellation"] = MiniTest.new_set()

T["cancellation"]["parks a wedged worker without disposing its Session"] = function()
  local run = assert(Workflow.new_run())
  local session = worker("prompting")
  session.cancel = function()
    session.cancelled = session.cancelled + 1
    return true
  end
  assert(run:adopt_session(session))

  assert(run:park())

  MiniTest.expect.equality(run.status, "parked")
  MiniTest.expect.equality(session.cancelled, 1)
  MiniTest.expect.equality(session.disposed, 0)
  MiniTest.expect.equality(#run.workers, 1)
end

T["cancellation"]["emergency stop disposes the whole Run"] = function()
  local run = assert(Workflow.new_run())
  local first, second = worker("ready"), worker("prompting")
  assert(run:adopt_session(first))
  assert(run:adopt_session(second))

  assert(run:emergency_stop())

  MiniTest.expect.equality(run.status, "disposed")
  MiniTest.expect.equality(first.disposed, 1)
  MiniTest.expect.equality(second.disposed, 1)
end

T["cancellation"]["cooperatively cancels ordinary workers without disposal"] = function()
  local scheduled
  local run = assert(Workflow.new_run({
    cancellation_timeout_ms = 25,
    schedule = function(_, callback)
      scheduled = callback
    end,
  }))
  local session = worker("prompting")
  assert(run:adopt_session(session))

  assert(run:cancel())
  scheduled()

  MiniTest.expect.equality(session.cancelled, 1)
  MiniTest.expect.equality(session.disposed, 0)
  MiniTest.expect.equality(run.status, "cancelled")
end

T["cancellation"]["disposes a worker that does not acknowledge by the bound"] = function()
  local scheduled
  local session = worker("prompting")
  session.cancel = function()
    session.cancelled = session.cancelled + 1
    return true
  end
  local run = assert(Workflow.new_run({
    cancellation_timeout_ms = 25,
    schedule = function(_, callback)
      scheduled = callback
    end,
  }))
  assert(run:adopt_session(session))

  assert(run:cancel())
  scheduled()

  MiniTest.expect.equality(session.cancelled, 1)
  MiniTest.expect.equality(session.disposed, 1)
  MiniTest.expect.equality(run.status, "cancelled")
end

T["cancellation"]["reaches every owned worker"] = function()
  local scheduled
  local run = assert(Workflow.new_run({
    schedule = function(_, callback)
      scheduled = callback
    end,
  }))
  local first, second = worker("prompting"), worker("prompting")
  assert(run:adopt_session(first))
  assert(run:adopt_session(second))

  assert(run:cancel())
  scheduled()

  MiniTest.expect.equality(first.cancelled, 1)
  MiniTest.expect.equality(second.cancelled, 1)
end

return T
