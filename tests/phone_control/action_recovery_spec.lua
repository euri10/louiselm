local MiniTest = require("mini.test")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local Action = require("louiselm.phone_control.action")

local T = MiniTest.new_set()

T["detaching a begun unadmitted action persists a fence before recovery"] = function()
  local reports, authorize, prompts = {}, nil, 0
  local journal = {}
  function journal:run(message, callback)
    reports[#reports + 1] = message
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
  function session:prompt()
    prompts = prompts + 1
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
      authorize = done
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
  local old_reply
  action:send(identity, text, function(result)
    old_reply = result
  end)
  assert(nvim.wait(5000, function()
    return authorize ~= nil
  end))
  local recovery
  action:detach(function(result)
    recovery = result
  end)
  assert(nvim.wait(5000, function()
    return recovery ~= nil
  end))
  MiniTest.expect.equality(recovery.state, "fenced_no_effect")
  MiniTest.expect.equality(reports[2].outcome.fenced_no_effect.evidence, recovery.evidence)
  MiniTest.expect.equality(old_reply, nil)
  local released = false
  assert(authorize)({
    valid = function()
      return true
    end,
    release = function()
      released = true
    end,
  })
  MiniTest.expect.equality({ prompts, released }, { 0, true })
end

T["detaching during admission cannot fence an accepted action"] = function()
  local reports = {}
  ---@type { state?: string, evidence?: string, code?: string }?
  local recovery
  local journal = {}
  function journal:run(message, callback)
    reports[#reports + 1] = message
    nvim.schedule(function()
      callback({ v = 1, type = message.type == "begin" and "begun" or "recorded" })
    end)
  end
  local owner = { sessions = {} }
  local session = { owner = owner }
  owner.sessions.s = session
  function session:on()
    return function() end
  end
  function session:inspect()
    return { id = "s", status = "ready" }
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
          valid = function()
            return true
          end,
          release = function() end,
        })
      end)
    end,
  })
  function session:prompt()
    action:detach(function(result)
      recovery = result
    end)
    return "turn-1"
  end
  MiniTest.finally(function()
    action:dispose()
  end)
  local text = "phone text"
  local old_reply
  action:send(
    {
      action = 3,
      operation_id = string.rep("02", 16),
      expected_revision = 1,
      payload_hash = nvim.fn.sha256(string.char(1) .. text),
    },
    text,
    function(result)
      old_reply = result
    end
  )
  assert(nvim.wait(5000, function()
    return recovery ~= nil
  end))
  MiniTest.expect.equality(assert(recovery).state, "accepted")
  MiniTest.expect.equality(type(reports[2].outcome.accepted.evidence), "string")
  MiniTest.expect.equality(old_reply, nil)
end

T["a failed fence write cannot confirm safe recovery"] = function()
  local authorize, storage_available = nil, false
  local journal = {}
  function journal:run(message, callback)
    nvim.schedule(function()
      callback(
        message.type == "begin" and { v = 1, type = "begun" }
          or storage_available and { v = 1, type = "recorded" }
          or { v = 1, type = "error", code = "unavailable" }
      )
    end)
  end
  local session = {}
  local action = Action.new({
    journal = journal,
    generation = string.rep("09", 16),
    session = session,
    projection = {},
    current = function()
      return true
    end,
    authorize = function(done)
      authorize = done
    end,
  })
  MiniTest.finally(function()
    action:dispose()
  end)
  local text = "phone text"
  action:send(
    {
      action = 3,
      operation_id = string.rep("03", 16),
      expected_revision = 1,
      payload_hash = nvim.fn.sha256(string.char(1) .. text),
    },
    text,
    function()
      error("detached reply must not be delivered")
    end
  )
  assert(nvim.wait(5000, function()
    return authorize ~= nil
  end))
  ---@type { state?: string, evidence?: string, code?: string, v?: integer, type?: string }?
  local recovery
  action:detach(function(result)
    recovery = result
  end)
  assert(nvim.wait(5000, function()
    return recovery ~= nil
  end))
  MiniTest.expect.equality(recovery, { v = 1, type = "error", code = "unavailable" })
  storage_available = true
  recovery = nil
  action:detach(function(result)
    recovery = result
  end)
  assert(nvim.wait(5000, function()
    return recovery ~= nil
  end))
  MiniTest.expect.equality(assert(recovery).state, "fenced_no_effect")
end

T["detaching after admission but before its journal reply never fences"] = function()
  local writes = {}
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
  function session:on()
    return function() end
  end
  function session:prompt()
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
          valid = function()
            return true
          end,
          release = function() end,
        })
      end)
    end,
  })
  MiniTest.finally(function()
    action:dispose()
  end)
  local text = "phone text"
  action:send(
    {
      action = 3,
      operation_id = string.rep("04", 16),
      expected_revision = 1,
      payload_hash = nvim.fn.sha256(string.char(1) .. text),
    },
    text,
    function()
      error("detached reply must not be delivered")
    end
  )
  assert(nvim.wait(5000, function()
    return #writes == 1
  end))
  local recovery
  action:detach(function(result)
    recovery = result
  end)
  MiniTest.expect.equality(#writes, 1)
  nvim.schedule(function()
    writes[1].callback({ v = 1, type = "recorded" })
  end)
  assert(nvim.wait(5000, function()
    return recovery ~= nil
  end))
  MiniTest.expect.equality(recovery.state, "accepted")
end

return T
