local MiniTest = require("mini.test")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local Action = require("louiselm.phone_control.action")

local T = MiniTest.new_set()

T["terminal notification waits for the durable completion report"] = function()
  local Phases = require("louiselm.phone_control.action_phases")
  local pending, terminal = {}, false
  local journal = {}
  function journal:run(message, callback)
    pending[#pending + 1] = { message = message, callback = callback }
  end
  local session = {
    on = function()
      return function() end
    end,
  }
  local phases = Phases.new(session, journal, { operation_id = string.rep("01", 16) }, function()
    terminal = true
  end)
  MiniTest.finally(function()
    phases:dispose()
  end)
  phases:admitted("turn-1", function() end)
  phases:completed(true)
  assert(nvim.wait(1000, function()
    return #pending == 1
  end))
  MiniTest.expect.equality(terminal, false)
  pending[1].callback({ type = "recorded" })
  assert(nvim.wait(1000, function()
    return #pending == 2
  end))
  MiniTest.expect.equality(terminal, false)
  pending[2].callback({ type = "recorded" })
  assert(nvim.wait(1000, function()
    return terminal
  end))
end

T["an unsuccessful callback stays unknown after its durable report"] = function()
  local Phases = require("louiselm.phone_control.action_phases")
  local reports, terminal = {}, nil
  local journal = {}
  function journal:run(message, callback)
    reports[#reports + 1] = message.outcome
    nvim.schedule(function()
      callback({ type = "recorded" })
    end)
  end
  local session = {
    on = function()
      return function() end
    end,
  }
  local phases = Phases.new(session, journal, { operation_id = string.rep("01", 16) }, function(outcome)
    terminal = outcome
  end)
  MiniTest.finally(function()
    phases:dispose()
  end)
  phases:admitted("turn-1", function() end)
  phases:completed(false)
  assert(nvim.wait(1000, function()
    return terminal ~= nil
  end))
  MiniTest.expect.equality(terminal, "unknown")
  MiniTest.expect.equality(reports[2], "unknown")
end

T["owner phases remain distinct after the accepted reply"] = function()
  local reports, listeners, completion = {}, {}, nil
  local journal = {}
  function journal:run(message, callback)
    if message.type == "report" then
      reports[#reports + 1] = message
    end
    nvim.schedule(function()
      callback({ v = 1, type = message.type == "begin" and "begun" or "recorded" })
    end)
  end
  local owner = { sessions = {} }
  local session = { owner = owner }
  owner.sessions.s = session
  function session:inspect()
    return { id = "s", status = "ready" }
  end
  function session:on(listener)
    listeners[#listeners + 1] = listener
    return function()
      for index, current in ipairs(listeners) do
        if current == listener then
          table.remove(listeners, index)
          return
        end
      end
    end
  end
  function session:prompt_phone(_, _gate, callback)
    completion = callback
    return "turn-1"
  end
  local action = Action.new({
    journal = journal,
    generation = string.rep("09", 16),
    session = session,
    projection = {
      page = function()
        return {}
      end,
    },
    current = function()
      return true
    end,
    authorize = function(done)
      nvim.schedule(function()
        done({
          gate = {},
          release = function() end,
        })
      end)
    end,
  })
  MiniTest.finally(function()
    action:dispose()
  end)
  local text = "phone text"
  local identity = {
    action = 3,
    operation_id = string.rep("01", 16),
    expected_revision = 1,
    payload_hash = nvim.fn.sha256(string.char(1) .. text),
  }
  local reply
  action:send(identity, text, function(result)
    reply = result
  end)
  assert(nvim.wait(5000, function()
    return reply ~= nil
  end))
  MiniTest.expect.equality(reply.state, "accepted")
  MiniTest.expect.equality(#reports, 1)
  MiniTest.expect.equality(reports[1].outcome.accepted.evidence, reply.evidence)

  assert(completion ~= nil)
  local timer = assert(nvim.uv.new_timer())
  timer:start(0, 0, function()
    for _, listener in ipairs(listeners) do
      listener({ type = "prompt_dispatched", data = { turn_id = "different-turn", prompt = {} } })
      listener({ type = "prompt_dispatched", data = { turn_id = "turn-1", prompt = {} } })
    end
    completion({ stopReason = "end_turn" }, nil)
    timer:close()
  end)
  assert(nvim.wait(5000, function()
    return #reports == 3
  end))
  MiniTest.expect.equality(type(reports[2].outcome.dispatched.evidence), "string")
  MiniTest.expect.equality(type(reports[3].outcome.completed.evidence), "string")
  MiniTest.expect.equality(reports[2].identity, identity)
  MiniTest.expect.equality(reports[3].identity, identity)
  MiniTest.expect.equality(nvim.json.encode(reports):find(text, 1, true), nil)
end

T["phase writes remain ordered when dispatch and completion overtake storage"] = function()
  local writes, listeners, complete = {}, {}, nil
  local journal = {}
  function journal:run(message, callback)
    if message.type == "begin" then
      nvim.schedule(function()
        callback({ v = 1, type = "begun" })
      end)
    else
      writes[#writes + 1] = { message = message, callback = callback }
    end
  end
  local owner = { sessions = {} }
  local session = { owner = owner }
  owner.sessions.s = session
  function session:inspect()
    return { id = "s", status = "ready" }
  end
  function session:on(callback)
    listeners[#listeners + 1] = callback
    return function() end
  end
  function session:prompt_phone(_, _gate, callback)
    complete = callback
    return "turn-1"
  end
  local action = Action.new({
    journal = journal,
    generation = string.rep("09", 16),
    session = session,
    projection = {
      page = function()
        return {}
      end,
    },
    current = function()
      return true
    end,
    authorize = function(done)
      nvim.schedule(function()
        done({
          gate = {},
          release = function() end,
        })
      end)
    end,
  })
  MiniTest.finally(function()
    action:dispose()
  end)
  local text = "phone text"
  local reply
  action:send(
    {
      action = 3,
      operation_id = string.rep("02", 16),
      expected_revision = 1,
      payload_hash = nvim.fn.sha256(string.char(1) .. text),
    },
    text,
    function(result)
      reply = result
    end
  )
  assert(nvim.wait(5000, function()
    return #writes == 1
  end))
  listeners[1]({ type = "prompt_dispatched", data = { turn_id = "turn-1", prompt = {} } })
  assert(complete)({ stopReason = "end_turn" }, nil)
  MiniTest.expect.equality({ #writes, reply }, { 1, nil })
  nvim.schedule(function()
    writes[1].callback({ v = 1, type = "recorded" })
  end)
  assert(nvim.wait(5000, function()
    return #writes == 2
  end))
  MiniTest.expect.equality(reply.state, "accepted")
  MiniTest.expect.equality(type(writes[2].message.outcome.dispatched.evidence), "string")
  nvim.schedule(function()
    writes[2].callback({ v = 1, type = "recorded" })
  end)
  assert(nvim.wait(5000, function()
    return #writes == 3
  end))
  MiniTest.expect.equality(type(writes[3].message.outcome.completed.evidence), "string")
  nvim.schedule(function()
    writes[3].callback({ v = 1, type = "recorded" })
  end)
end

T["a failed phase report is retained for retry without confirming acceptance"] = function()
  local writes, available = {}, false
  local journal = {}
  function journal:run(message, callback)
    writes[#writes + 1] = message
    nvim.schedule(function()
      callback(
        message.type == "begin" and { v = 1, type = "begun" }
          or available and { v = 1, type = "recorded" }
          or { v = 1, type = "error", code = "unavailable" }
      )
    end)
  end
  local owner = { sessions = {} }
  local session = { owner = owner }
  owner.sessions.s = session
  function session:inspect()
    return { id = "s", status = "ready" }
  end
  function session:on()
    return function() end
  end
  function session:prompt_phone()
    return "turn-1"
  end
  local action = Action.new({
    journal = journal,
    generation = string.rep("09", 16),
    session = session,
    projection = {
      page = function()
        return {}
      end,
    },
    current = function()
      return true
    end,
    authorize = function(done)
      nvim.schedule(function()
        done({
          gate = {},
          release = function() end,
        })
      end)
    end,
  })
  MiniTest.finally(function()
    action:dispose()
  end)
  local text = "phone text"
  local replies = {}
  action:send(
    {
      action = 3,
      operation_id = string.rep("03", 16),
      expected_revision = 1,
      payload_hash = nvim.fn.sha256(string.char(1) .. text),
    },
    text,
    function(result)
      replies[#replies + 1] = result
    end
  )
  assert(nvim.wait(5000, function()
    return #replies == 1
  end))
  MiniTest.expect.equality(replies[1].code, "unavailable")
  available = true
  action:retry_phases()
  assert(nvim.wait(5000, function()
    return #writes == 3
  end))
  MiniTest.expect.equality(writes[2].outcome, writes[3].outcome)
  MiniTest.expect.equality(#replies, 1)
end

T["a failed turn retains unknown after the dispatched proof"] = function()
  local writes, listener = {}, nil
  local journal = {}
  function journal:run(message, callback)
    writes[#writes + 1] = message.outcome
    nvim.schedule(function()
      callback({ v = 1, type = "recorded" })
    end)
  end
  local session = {}
  function session:on(callback)
    listener = callback
    return function() end
  end
  local phases = require("louiselm.phone_control.action_phases").new(session, journal, {
    operation_id = string.rep("04", 16),
  })
  MiniTest.finally(function()
    phases:dispose()
  end)
  phases:admitted("turn-1", function() end)
  assert(listener)({ type = "prompt_dispatched", data = { turn_id = "turn-1" } })
  phases:completed(false)
  assert(nvim.wait(5000, function()
    return #writes == 3
  end))
  MiniTest.expect.equality(type(writes[1].accepted.evidence), "string")
  MiniTest.expect.equality(type(writes[2].dispatched.evidence), "string")
  MiniTest.expect.equality(writes[3], "unknown")
end

T["cancelled and malformed callbacks never prove completion"] = function()
  for _, result in ipairs({ { stopReason = "cancelled" }, {}, { stopReason = "invalid" } }) do
    local reports, complete = {}, nil
    local journal = {}
    function journal:run(message, callback)
      if message.type == "report" then
        reports[#reports + 1] = message.outcome
      end
      nvim.schedule(function()
        callback({ v = 1, type = message.type == "begin" and "begun" or "recorded" })
      end)
    end
    local owner = { sessions = {} }
    local session = { owner = owner }
    owner.sessions.s = session
    function session:inspect()
      return { id = "s", status = "ready" }
    end
    function session:on()
      return function() end
    end
    function session:prompt_phone(_, _, callback)
      complete = callback
      return "turn-1"
    end
    local action = Action.new({
      journal = journal,
      generation = string.rep("09", 16),
      session = session,
      projection = {
        page = function()
          return {}
        end,
      },
      current = function()
        return true
      end,
      authorize = function(done)
        nvim.schedule(function()
          done({ gate = {}, release = function() end })
        end)
      end,
    })
    MiniTest.finally(function()
      action:dispose()
    end)
    local text = "phone text"
    action:send({
      action = 3,
      operation_id = string.rep("05", 16),
      expected_revision = 1,
      payload_hash = nvim.fn.sha256(string.char(1) .. text),
    }, text, function() end)
    assert(nvim.wait(5000, function()
      return complete ~= nil
    end))
    assert(complete)(result, nil)
    assert(nvim.wait(5000, function()
      return #reports == 2
    end))
    MiniTest.expect.equality(reports[2], "unknown")
  end
end

return T
