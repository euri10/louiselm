local MiniTest = require("mini.test")
local Workflow = require("louiselm.workflow")

---@diagnostic disable-next-line: undefined-global -- Neovim injects its runtime API.
local nvim = vim

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
    first = { workflow = "reference", entry = true, ["park-expiry"] = "1h", outcomes = {} },
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

T["ownership"]["a skill invoked by a stage Agent inherits Run ownership and stops when the Run cancels"] = function()
  -- The motivating scenario for the whole ownership contract (louiselm-qbr.3.2):
  -- a stage's Agent invokes a work-producing skill mid-turn. The skill's own
  -- work is not a separate, detachable concern -- it reaches the Run through
  -- the same `create_session` every other stage worker uses, so it is owned
  -- the moment it exists, and Cancellation reaches it exactly like any other
  -- owned worker, with no special-casing for how it came to exist.
  local stage_session = worker("ready")
  local skill_session
  local api = {
    create_session = function(_, _, _, callback)
      skill_session = worker("prompting")
      callback(skill_session)
      return skill_session
    end,
  }
  local scheduled
  local run = assert(Workflow.new_run({
    session_api = api,
    cancellation_timeout_ms = 25,
    schedule = function(_, callback)
      scheduled = callback
    end,
  }))
  assert(run:adopt_session(stage_session))

  local created = assert(run:create_session("skill-agent"))
  MiniTest.expect.equality(created, skill_session)
  MiniTest.expect.equality(skill_session.owner_run, run)
  MiniTest.expect.equality(#run.workers, 2)

  assert(run:cancel())
  scheduled()

  MiniTest.expect.equality(skill_session.cancelled, 1)
  MiniTest.expect.equality(run.status, "cancelled")
end

T["cancellation"] = MiniTest.new_set()

T["durable Park"] = MiniTest.new_set()

T["durable Park"]["restores claims and generated-work accounting on resume"] = function()
  local run = assert(Workflow.new_run())
  assert(run:accept_park())

  assert(run:accept_resume({ "issue-1", "issue-2" }, { ceiling = 5, consumed = 2, reserved = 1 }))

  MiniTest.expect.equality(run.status, "active")
  MiniTest.expect.equality(run.claims, { "issue-1", "issue-2" })
  MiniTest.expect.equality(run.generated_work, { ceiling = 5, consumed = 2, reserved = 1 })
end

T["durable Park"]["accepts a service Park without persisting it again"] = function()
  local persisted = 0
  local session = worker("prompting")
  local run = assert(Workflow.new_run({
    park_record = {
      id = "already-durable",
      session_id = "codex/session",
      agent = "codex",
      acp_session_id = "session",
      cwd = "/tmp/project",
      load_session = true,
      claims = { "issue" },
    },
    park_service = function()
      persisted = persisted + 1
      return true
    end,
  }))
  assert(run:adopt_session(session))
  assert(run:accept_park())
  MiniTest.expect.equality(run.status, "parked")
  MiniTest.expect.equality(session.cancelled, 1)
  MiniTest.expect.equality(persisted, 0)
end

T["durable Park"]["derives cold resume metadata from the owned Session"] = function()
  local captured
  local session = worker("ready")
  session.state = {
    agent = "codex",
    acp_session_id = "acp-session",
    working_dir = "/tmp/project",
    current_turn = 1,
  }
  function session:inspect()
    return self.state
  end
  session.client = { agent_capabilities = { loadSession = true } }
  local run = assert(Workflow.new_run({
    park_service = function(record, callback)
      captured = record
      callback(true)
      return true
    end,
  }))
  assert(run:adopt_session(session))

  assert(run:park_cold({ id = "run", claims = { "issue" } }))
  MiniTest.expect.equality(captured, {
    id = "run",
    session_id = "codex/acp-session",
    agent = "codex",
    acp_session_id = "acp-session",
    cwd = "/tmp/project",
    load_session = true,
    claims = { "issue" },
  })
end

T["durable Park"]["retains the cold Park record while the async service confirms persistence"] = function()
  -- Regression for louiselm-psz9: the real park_service is an async
  -- subprocess (`louiselm-capture run park`) that confirms well after
  -- park_cold returns. Chat:park's reuse check
  -- (`run.park_record and run.park_record.id`) depends on the record
  -- surviving past that confirmation so a second `:LouiselmPark` on an
  -- already cold-Parked Session reuses the admitted Run id instead of
  -- minting a fresh one that was never admitted.
  local complete
  local session = worker("ready")
  session.state = {
    agent = "codex",
    acp_session_id = "acp-session",
    working_dir = "/tmp/project",
    current_turn = 1,
  }
  function session:inspect()
    return self.state
  end
  session.client = { agent_capabilities = { loadSession = true } }
  local run = assert(Workflow.new_run({
    park_service = function(_, callback)
      complete = callback
      return true
    end,
  }))
  assert(run:adopt_session(session))

  assert(run:park_cold({ id = "run", claims = { "issue" } }))
  MiniTest.expect.equality(run.park_record ~= nil, true)
  MiniTest.expect.equality(run.park_record.id, "run")

  complete(true)
  MiniTest.expect.equality(run.status, "parked")
  MiniTest.expect.equality(run.park_record.id, "run")
end

T["durable Park"]["rolls back the cold Park record when the async service reports failure"] = function()
  local complete
  local session = worker("ready")
  session.state = {
    agent = "codex",
    acp_session_id = "acp-session",
    working_dir = "/tmp/project",
    current_turn = 1,
  }
  function session:inspect()
    return self.state
  end
  session.client = { agent_capabilities = { loadSession = true } }
  local run = assert(Workflow.new_run({
    park_service = function(_, callback)
      complete = callback
      return true
    end,
  }))
  assert(run:adopt_session(session))

  local failure
  assert(run:park_cold({ id = "run", claims = { "issue" } }, function(ok, error_message)
    failure = { ok, error_message }
  end))
  complete(false, "service unavailable")

  MiniTest.expect.equality(run.status, "active")
  MiniTest.expect.equality(run.park_record, nil)
  MiniTest.expect.equality(failure, { false, "service unavailable" })
end

T["durable Park"]["rejects cold Park when the Session is not reloadable"] = function()
  local session = worker("ready")
  session.state = { agent = "codex", acp_session_id = "acp-session", working_dir = "/tmp/project" }
  function session:inspect()
    return self.state
  end
  session.client = { agent_capabilities = {} }
  local run = assert(Workflow.new_run())
  assert(run:adopt_session(session))

  local started, error_message = run:park_cold({ id = "run", claims = { "issue" } })
  MiniTest.expect.equality(started, false)
  MiniTest.expect.equality(error_message, "cold Park requires an Agent that supports session/load")
end

T["durable Park"]["rejects cold Park when the Session has never sent a prompt"] = function()
  -- Regression for louiselm-aaw0.1: a freshly created ACP Session that
  -- advertises `loadSession` still has nothing durable for the Agent to
  -- resume from until at least one prompt has been sent. Parking it anyway
  -- produces a Park that `:LouiselmResumePark` can never load.
  local session = worker("ready")
  session.state = {
    agent = "claude",
    acp_session_id = "acp-session",
    working_dir = "/tmp/project",
    current_turn = 0,
  }
  function session:inspect()
    return self.state
  end
  session.client = { agent_capabilities = { loadSession = true } }
  local run = assert(Workflow.new_run())
  assert(run:adopt_session(session))

  local started, error_message = run:park_cold({ id = "run", claims = { "issue" } })
  MiniTest.expect.equality(started, false)
  MiniTest.expect.equality(error_message, "cold Park requires a Session that has sent at least one prompt")
end

T["durable Park"]["parks successfully loaded history without another prompt"] = function()
  local session = worker("ready")
  function session:inspect()
    return {
      status = self.status,
      source = "loaded",
      agent = "codex",
      acp_session_id = "acp-session",
      working_dir = "/tmp/project",
      current_turn = 0,
    }
  end
  session.client = { agent_capabilities = { loadSession = true } }
  local persisted = false
  local run = assert(Workflow.new_run({
    park_service = function(_, callback)
      persisted = true
      nvim.schedule(function()
        callback(true)
      end)
      return true
    end,
  }))
  assert(run:adopt_session(session))
  assert(run:park_cold({ id = "loaded-run", claims = {} }))
  assert(nvim.wait(1000, function()
    return run.status == "parked"
  end))
  MiniTest.expect.equality(persisted, true)
end

T["durable Park"]["does not treat an unfinished or failed load as persisted history"] = function()
  for _, status in ipairs({ "starting", "error" }) do
    local session = worker(status)
    function session:inspect()
      return {
        status = self.status,
        source = "loaded",
        agent = "codex",
        acp_session_id = "acp-session",
        working_dir = "/tmp/project",
        current_turn = 0,
      }
    end
    session.client = { agent_capabilities = { loadSession = true } }
    local run = assert(Workflow.new_run())
    assert(run:adopt_session(session))
    local started = run:park_cold({ id = "unfinished-load", claims = {} })
    MiniTest.expect.equality(started, false)
  end
end

T["durable Park"]["does not transition before service confirmation"] = function()
  local complete
  local run = assert(Workflow.new_run({
    park_record = {
      id = "run",
      session_id = "codex/session",
      agent = "codex",
      acp_session_id = "acp-session",
      cwd = "/tmp/project",
      load_session = true,
      claims = { "issue" },
    },
    park_service = function(_, callback)
      complete = callback
      return true
    end,
  }))
  local result = assert(run:park(function(ok)
    MiniTest.expect.equality(ok, true)
  end))
  MiniTest.expect.equality(result, true)
  MiniTest.expect.equality(run.status, "active")
  complete(true)
  MiniTest.expect.equality(run.status, "parked")
end

T["durable Park"]["keeps Run active when service persistence fails"] = function()
  local failure
  local run = assert(Workflow.new_run({
    park_record = {
      id = "run",
      session_id = "codex/session",
      agent = "codex",
      acp_session_id = "acp-session",
      cwd = "/tmp/project",
      load_session = true,
      claims = { "issue" },
    },
    park_service = function(_, callback)
      callback(false, "service unavailable")
      return true
    end,
  }))
  assert(run:park(function(ok, error_message)
    failure = { ok, error_message }
  end))
  MiniTest.expect.equality(run.status, "active")
  MiniTest.expect.equality(failure, { false, "service unavailable" })
end

T["durable Park"]["notifies the callback when Park is invoked on an already-Parked Run"] = function()
  -- Regression for louiselm-oaib.1: a second :LouiselmPark on a Session
  -- that is already cold-Parked took this early-return branch, and with
  -- no callback invocation the operator saw no notification at all --
  -- the command appeared to silently do nothing.
  local run = assert(Workflow.new_run())
  assert(run:accept_park())
  MiniTest.expect.equality(run.status, "parked")

  local result
  local started = assert(run:park(function(ok, error_message)
    result = { ok, error_message }
  end))
  MiniTest.expect.equality(started, true)
  MiniTest.expect.equality(result, { true, nil })
end

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
