local MiniTest = require("mini.test")
local Acp = require("louiselm.acp")
local Chat = require("louiselm.ui.chat")
local Session = require("louiselm.session")

local T = MiniTest.new_set()

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim
local project_root = nvim.fn.getcwd()

local function mock_definition(mode, response, extra_env)
  local env = { LOUISELM_MOCK_MODE = mode }
  if response ~= nil then
    env.LOUISELM_MOCK_RESPONSE = response
  end
  for key, value in pairs(extra_env or {}) do
    env[key] = value
  end
  return {
    command = nvim.v.progpath,
    args = {
      "--headless",
      "--noplugin",
      "-u",
      project_root .. "/tests/mock/init.lua",
      "-c",
      "lua require('louiselm.dev.mock_agent').run()",
    },
    env = env,
  }
end

---@param predicate fun(): boolean
local function wait_for(predicate)
  MiniTest.expect.equality(nvim.wait(3000, predicate, 10), true)
end

T["mock agent"] = MiniTest.new_set()

T["mock agent"]["supports the ACP session flow and static responses"] = function()
  local updates = {}
  local client = assert(Acp.connect(mock_definition("static", "fixed response"), {
    on_notification = function(message)
      updates[#updates + 1] = message
    end,
  }))

  local initialized
  assert(client:initialize(nil, function(result, err)
    initialized = { result = result, error = err }
  end))
  wait_for(function()
    return initialized ~= nil
  end)
  MiniTest.expect.equality(initialized.error, nil)
  MiniTest.expect.equality(initialized.result.protocolVersion, 1)

  local created
  assert(client:new_session({ cwd = project_root, mcpServers = {} }, function(result, err)
    created = { result = result, error = err }
  end))
  wait_for(function()
    return created ~= nil
  end)
  MiniTest.expect.equality(created.error, nil)

  local session_id = created.result.sessionId
  local listed
  assert(client:list_sessions({ cwd = project_root }, function(result, err)
    listed = { result = result, error = err }
  end))
  wait_for(function()
    return listed ~= nil
  end)
  MiniTest.expect.equality(listed, {
    result = {
      sessions = { { sessionId = session_id, cwd = project_root, title = "Mock " .. session_id } },
    },
    error = nil,
  })

  local rejected
  assert(client:list_sessions(nvim.json.decode("[]"), function(result, err)
    rejected = { result = result, error = err }
  end))
  wait_for(function()
    return rejected ~= nil
  end)
  MiniTest.expect.equality(rejected.result, nil)
  MiniTest.expect.equality(rejected.error.code, -32602)

  local loaded
  assert(client:load_session({ sessionId = session_id }, function(result, err)
    loaded = { result = result, error = err }
  end))
  wait_for(function()
    return loaded ~= nil
  end)
  MiniTest.expect.equality(loaded, { result = { sessionId = session_id }, error = nil })

  local completed
  assert(client:prompt({ sessionId = session_id, prompt = { { type = "text", text = "hello" } } }, function(result, err)
    completed = { result = result, error = err }
  end))
  wait_for(function()
    return completed ~= nil
  end)
  MiniTest.expect.equality(completed, { result = { stopReason = "end_turn" }, error = nil })
  MiniTest.expect.equality(updates[1].params.update.content.text, "fixed response")
  MiniTest.expect.equality(updates[2].params.update.sessionUpdate, "turn_done")

  assert(client:close())
end

T["mock agent"]["completes a prompt after a permission response"] = function()
  local events = {}
  local ready
  local completed
  local api = assert(Session.new({ mock = mock_definition("permission") }))
  local session = assert(api:create_session("mock", {
    cwd = project_root,
    on_event = function(event)
      events[#events + 1] = event
      if event.type == "permission_requested" then
        local sent, err = event.respond({ outcome = { outcome = "selected", optionId = "allow-once" } })
        MiniTest.expect.equality({ sent = sent, error = err }, { sent = true, error = nil })
      end
    end,
  }, function(value, err)
    ready = { session = value, error = err }
  end))
  wait_for(function()
    return ready ~= nil
  end)
  MiniTest.expect.equality(ready.error, nil)

  assert(session:prompt("permission please", function(result, err)
    completed = { result = result, error = err }
  end))
  wait_for(function()
    return completed ~= nil
  end)
  MiniTest.expect.equality(completed, {
    result = { stopReason = "end_turn" },
    error = nil,
  })
  local meaningful = {}
  for _, event in ipairs(events) do
    if event.type ~= "state_changed" then
      meaningful[#meaningful + 1] = event.type
    end
  end
  MiniTest.expect.equality(meaningful, { "permission_requested", "chunk", "turn_done" })

  assert(api:dispose())
end

T["mock agent"]["advertises commands through session/update and delivers them on the session"] = function()
  local events = {}
  local changed
  local ready
  local api = assert(Session.new({
    mock = mock_definition("echo", nil, {
      LOUISELM_MOCK_AVAILABLE_COMMANDS = nvim.json.encode({
        { name = "grill-me", description = "Stress-test an idea" },
      }),
    }),
  }))
  local session = assert(api:create_session("mock", {
    cwd = project_root,
    on_event = function(event)
      events[#events + 1] = event
      if event.type == "commands_changed" then
        changed = event
      end
    end,
  }, function(value, err)
    ready = { session = value, error = err }
  end))
  wait_for(function()
    return ready ~= nil
  end)
  MiniTest.expect.equality(ready.error, nil)

  wait_for(function()
    return changed ~= nil
  end)
  MiniTest.expect.equality(session:inspect().commands, { { name = "grill-me", description = "Stress-test an idea" } })

  MiniTest.expect.equality(changed.data, {
    commands = { { name = "grill-me", description = "Stress-test an idea" } },
    diagnostics = {},
  })

  assert(api:dispose())
end

T["mock agent"]["defers and consumes a hidden catalog once across the process boundary"] = function()
  local api = assert(Session.new({ mock = mock_definition("echo") }, "inject"))
  local chat = assert(Chat.new(api, {
    agents = { "mock" },
    skill_catalog = "hidden catalog",
  }))
  local session = assert(chat:new_session("mock", { cwd = project_root }))
  local chunks = {}
  session:on(function(event)
    if event.type == "chunk" then
      chunks[#chunks + 1] = event.data.content.text
    end
  end)
  wait_for(function()
    return session:inspect().status == "ready"
  end)

  assert(chat:submit("/compact"))
  wait_for(function()
    return session:inspect().status == "ready" and #chunks == 1
  end)
  assert(chat:submit("hello"))
  wait_for(function()
    return session:inspect().status == "ready" and #chunks == 2
  end)
  assert(chat:submit("again"))
  wait_for(function()
    return session:inspect().status == "ready" and #chunks == 3
  end)

  MiniTest.expect.equality(chunks, { "/compact", "hidden catalog", "again" })
  chat:dispose()
end

T["mock agent"]["surfaces a simulated crash as a session error"] = function()
  local ready
  local completed
  local error_event
  local api = assert(Session.new({ mock = mock_definition("crash") }))
  local session = assert(api:create_session("mock", {
    cwd = project_root,
    on_event = function(event)
      if event.type == "error" then
        error_event = event
      end
    end,
  }, function(value, err)
    ready = { session = value, error = err }
  end))
  wait_for(function()
    return ready ~= nil
  end)
  MiniTest.expect.equality(ready.error, nil)

  assert(session:prompt("crash", function(result, err)
    completed = { result = result, error = err }
  end))
  wait_for(function()
    return completed ~= nil
  end)
  MiniTest.expect.equality(completed.result, nil)
  MiniTest.expect.equality(completed.error, "agent process exited with code 23")
  MiniTest.expect.equality(error_event.data.message, "agent process exited with code 23")
  MiniTest.expect.equality(session:inspect().status, "error")

  assert(api:dispose())
end

return T
