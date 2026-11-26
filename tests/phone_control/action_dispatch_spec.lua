local MiniTest = require("mini.test")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local Action = require("louiselm.phone_control.action")
local Journal = require("louiselm.phone_control.action_journal")
local Projection = require("louiselm.phone_control.projection")
local Session = require("louiselm.session")
local Chat = require("louiselm.ui.chat")

local T = MiniTest.new_set()

T["an action without a bound generation cannot reserve or authorize"] = function()
  local called = false
  local session = {}
  local action = Action.new({
    journal = {
      run = function()
        called = true
      end,
    },
    session = session,
    projection = {},
    current = function()
      return true
    end,
    authorize = function()
      called = true
    end,
  })
  MiniTest.finally(function()
    action:dispose()
  end)
  local text = "phone text"
  local result
  action:send(
    {
      action = 3,
      operation_id = string.rep("01", 16),
      expected_revision = 1,
      payload_hash = nvim.fn.sha256(string.char(1) .. text),
    },
    text,
    function(value)
      result = value
    end
  )
  assert(nvim.wait(5000, function()
    return result ~= nil
  end))
  MiniTest.expect.equality(result.code, "unavailable")
  MiniTest.expect.equality(called, false)
end

T["a stale Session is durably fenced before the refusal is returned"] = function()
  local calls, released, prompt_count = {}, 0, 0
  local journal = {}
  function journal:run(message, callback)
    calls[#calls + 1] = message
    nvim.schedule(function()
      callback({ v = 1, type = message.type == "begin" and "begun" or "recorded" })
    end)
  end
  local owner = { sessions = {} }
  local session = { owner = owner }
  function session:inspect()
    return { id = "s", status = "ready" }
  end
  function session:prompt()
    prompt_count = prompt_count + 1
  end
  owner.sessions.s = session
  local action = Action.new({
    journal = journal,
    generation = string.rep("09", 16),
    session = session,
    projection = {
      page = function()
        return nil
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
          release = function()
            released = released + 1
          end,
        })
      end)
    end,
  })
  local text = "phone text"
  local identity = {
    action = 3,
    operation_id = string.rep("01", 16),
    expected_revision = 1,
    payload_hash = nvim.fn.sha256(string.char(1) .. text),
  }
  local result
  action:send(identity, text, function(value)
    result = value
  end)
  assert(nvim.wait(5000, function()
    return result ~= nil
  end))
  MiniTest.expect.equality(result.state, "fenced_no_effect")
  MiniTest.expect.equality(result.code, "stale_subject")
  MiniTest.expect.equality(calls[1].v, 2)
  MiniTest.expect.equality(calls[1].generation, string.rep("09", 16))
  MiniTest.expect.equality(calls[2].outcome.fenced_no_effect.evidence, result.evidence)
  MiniTest.expect.equality(prompt_count, 0)
  MiniTest.expect.equality(released, 1)
  action:dispose()
end

T["a lost owner report never confirms or repeats an admitted prompt"] = function()
  local prompts, callbacks, released = 0, 0, 0
  local journal = {}
  function journal:run(message, callback)
    nvim.schedule(function()
      if message.type == "begin" then
        callback({ v = 1, type = "begun" })
        callback({ v = 1, type = "begun" })
      else
        callback({ v = 1, type = "error", code = "unavailable" })
      end
    end)
  end
  local owner = { sessions = {} }
  local session = { owner = owner }
  function session:on()
    return function() end
  end
  function session:inspect()
    return { id = "s", status = "ready" }
  end
  function session:prompt()
    prompts = prompts + 1
    return "turn-1"
  end
  owner.sessions.s = session
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
          release = function()
            released = released + 1
          end,
        })
      end)
    end,
  })
  local text = "phone text"
  local identity = {
    action = 3,
    operation_id = string.rep("01", 16),
    expected_revision = 1,
    payload_hash = nvim.fn.sha256(string.char(1) .. text),
  }
  local result
  action:send(identity, text, function(value)
    callbacks = callbacks + 1
    result = value
  end)
  assert(nvim.wait(5000, function()
    return result ~= nil
  end))
  MiniTest.expect.equality(result.code, "unavailable")
  MiniTest.expect.equality(prompts, 1)
  MiniTest.expect.equality(callbacks, 1)
  MiniTest.expect.equality(released, 1)
  action:dispose()
end

local function exercise_real_session(initialized)
  local binary = nvim.env.LOUISELM_TEST_CAPTURE
  if binary == nil or binary == "" then
    MiniTest.skip("set LOUISELM_TEST_CAPTURE to the built capture executable")
  end
  local root, directory = nvim.fn.getcwd(), nvim.fn.tempname()
  local previous = nvim.env.LOUISELM_CAPTURE_STATE_DIR
  nvim.env.LOUISELM_CAPTURE_STATE_DIR = directory
  local definition = {
    provider = "test-service",
    command = nvim.v.progpath,
    env = { LOUISELM_MOCK_MODE = "static" },
    args = {
      "--headless",
      "--noplugin",
      "-u",
      root .. "/tests/mock/init.lua",
      "-c",
      "lua require('louiselm.dev.mock_agent').run()",
    },
  }
  local api = assert(Session.new({ mock = definition }, nil, { usage_directory = directory .. "/usage" }))
  local journal = Journal.new({ service = { nvim.fn.fnamemodify(binary, ":p") } })
  local chat, action, unsubscribe
  MiniTest.finally(function()
    if action then
      action:dispose()
    end
    if unsubscribe then
      unsubscribe()
    end
    if chat then
      chat:dispose()
    end
    journal:dispose()
    assert(api:dispose())
    local finished = false
    api:flush_recording(function()
      finished = true
    end)
    assert(nvim.wait(6000, function()
      return finished
    end))
    nvim.env.LOUISELM_CAPTURE_STATE_DIR = previous
    nvim.fn.delete(directory, "rf")
  end)
  require("tests.fixtures.phone_control_journal")(nvim.fn.fnamemodify(binary, ":p"), directory)
  local session = assert(api:create_session("mock", { cwd = root }))
  assert(nvim.wait(6000, function()
    return session:inspect().status == "ready"
  end))
  chat = assert(Chat.new(api, { agents = { "mock" }, start_insert_on_switch = false }))
  assert(chat:attach(session))
  assert(chat:queue_context({ label = "private", text = "UNSENT CONTEXT" }))
  local view = chat.views[session:inspect().id]
  view.renderer:replace_prompt("UNSENT DRAFT")
  local projection = Projection.new(function()
    return session:inspect().status
  end)
  unsubscribe = session:on(function(event)
    projection:record(event)
  end)
  local authorizations, releases = 0, 0
  action = Action.new({
    journal = journal,
    generation = string.rep("09", 16),
    session = session,
    projection = projection,
    current = function()
      return true
    end,
    authorize = function(done)
      authorizations = authorizations + 1
      nvim.schedule(function()
        done({
          valid = function()
            return true
          end,
          release = function()
            releases = releases + 1
          end,
        })
      end)
    end,
  })
  local text = "Reply from phone"
  local identity = {
    operation_id = string.rep("01", 16),
    receiver_identity = string.rep("02", 32),
    controller_id = string.rep("03", 16),
    subject_id = string.rep("04", 16),
    action = 3,
    payload_hash = nvim.fn.sha256(string.char(1) .. text),
    grant_id = string.rep("06", 16),
    grant_revision = 1,
    period_id = string.rep("07", 16),
    expected_revision = projection.revision,
    project = assert(nvim.uv.fs_realpath(root)),
    agent = "mock",
    recorded_at_ms = 1000,
  }
  local function send()
    local result
    action:send(identity, text, function(value)
      result = value
    end)
    assert(nvim.wait(6000, function()
      return result ~= nil
    end))
    return result
  end
  local initial_turn = session:inspect().current_turn
  local first = send()
  if not initialized then
    local unavailable = { v = 1, type = "error", code = "unavailable" }
    MiniTest.expect.equality(first, unavailable)
    MiniTest.expect.equality(send(), unavailable)
    MiniTest.expect.equality(session:inspect().current_turn, initial_turn)
    MiniTest.expect.equality(session:inspect().status, "ready")
    MiniTest.expect.equality({ authorizations, releases }, { 0, 0 })
    MiniTest.expect.equality(view.renderer:prompt_text(), "UNSENT DRAFT")
    local contents = table.concat(nvim.api.nvim_buf_get_lines(view.renderer.buffer, 0, -1, false), "\n")
    MiniTest.expect.equality(contents:find(text, 1, true), nil)
    MiniTest.expect.equality(contents:find("UNSENT CONTEXT", 1, true), nil)
    return
  end
  MiniTest.expect.equality(first.state, "accepted")
  assert(nvim.wait(6000, function()
    return session:inspect().status == "ready"
  end))
  local phases
  local function inspect_phases()
    journal:run({ v = 1, type = "inspect_phases", identity = identity }, function(result)
      if result.type == "action_phases" and result.completed_evidence ~= "" then
        phases = result
      else
        nvim.defer_fn(inspect_phases, 20)
      end
    end)
  end
  inspect_phases()
  assert(nvim.wait(6000, function()
    return phases ~= nil
  end))
  MiniTest.expect.equality(phases.accepted_evidence, first.evidence)
  MiniTest.expect.equality(#phases.dispatched_evidence, 64)
  MiniTest.expect.equality(#phases.completed_evidence, 64)
  MiniTest.expect.equality(session:inspect().current_turn, 1)
  MiniTest.expect.equality(view.renderer:prompt_text(), "UNSENT DRAFT")
  local lines = nvim.api.nvim_buf_get_lines(view.renderer.buffer, 0, -1, false)
  MiniTest.expect.equality(table.concat(lines, "\n"):find(text, 1, true) ~= nil, true)
  MiniTest.expect.equality(table.concat(lines, "\n"):find("UNSENT CONTEXT", 1, true), nil)
  MiniTest.expect.equality(send().type, "existing")
  MiniTest.expect.equality(session:inspect().current_turn, 1)
  MiniTest.expect.equality(releases, 1)
end

T["an uninitialized owner leaves the real Session and desktop draft untouched"] = function()
  exercise_real_session(false)
end

T["one explicit phone action enters the real Session without consuming the desktop draft"] = function()
  MiniTest.skip("restore after receiver-bound initialization: louiselm-qbr.12.4.3.2")
  exercise_real_session(true)
end

return T
