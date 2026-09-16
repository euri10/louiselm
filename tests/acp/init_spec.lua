local MiniTest = require("mini.test")
local Acp = require("louiselm.acp")
local Protocol = require("louiselm.acp.protocol")

local T = MiniTest.new_set()

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

---@param system function
local function set_system(system)
  rawset(nvim, "system", system)
end

T["connect"] = MiniTest.new_set()

T["connect"]["correlates responses and builds ACP requests"] = function()
  local calls = {}
  local fake_handle = {
    is_closing = function()
      return false
    end,
    write = function(_, data)
      calls[#calls + 1] = data
    end,
  }
  local original_system = nvim.system
  set_system(function(_, options)
    calls.stdout = options.stdout
    return fake_handle
  end)

  local client, err = Acp.connect({ provider = "test-service", command = "agent", args = {} })
  set_system(original_system)
  MiniTest.expect.equality(err, nil)
  assert(client ~= nil)

  local result
  local request_error
  local request_id = client:initialize(nil, function(value, failure)
    result = value
    request_error = failure
  end)
  MiniTest.expect.equality(request_id, 1)
  MiniTest.expect.equality(assert(Protocol.decode(calls[1]:sub(1, -2))), {
    id = 1,
    method = "initialize",
    params = {
      clientCapabilities = {
        fs = { readTextFile = false, writeTextFile = false },
        _meta = {
          ["io.github.euri10.louiselm.sessionActivity"] = { version = 1 },
          jetbrains = {
            air = { version = 1, capabilities = { "sessionFailure" } },
          },
        },
        session = { configOptions = { boolean = {} } },
        terminal = false,
      },
      clientInfo = { name = "louiselm.nvim", version = nvim.fn.readfile("VERSION")[1] },
      protocolVersion = 1,
    },
    jsonrpc = "2.0",
  })

  calls.stdout(
    nil,
    '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":true}}}\n'
  )
  MiniTest.expect.equality(result, { protocolVersion = 1, agentCapabilities = { loadSession = true } })
  MiniTest.expect.equality(request_error, nil)

  local session_id
  client:new_session({ cwd = "/tmp/project", mcpServers = {} }, function(value)
    session_id = value.sessionId
  end)
  MiniTest.expect.equality(assert(Protocol.decode(calls[2]:sub(1, -2))), {
    id = 2,
    jsonrpc = "2.0",
    method = "session/new",
    params = { cwd = "/tmp/project", mcpServers = {} },
  })
  calls.stdout(nil, '{"jsonrpc":"2.0","id":2,"result":{"sessionId":"session-1"}}\n')
  MiniTest.expect.equality(session_id, "session-1")

  client:set_config_option({ sessionId = "session-1", configId = "model", value = "large" })
  MiniTest.expect.equality(assert(Protocol.decode(calls[3]:sub(1, -2))), {
    id = 3,
    jsonrpc = "2.0",
    method = "session/set_config_option",
    params = { sessionId = "session-1", configId = "model", value = "large" },
  })
end

T["connect"]["ignores an unsolicited response after a completed request"] = function()
  local calls = {}
  local errors = {}
  local fake_handle = {
    is_closing = function()
      return false
    end,
    write = function(_, data)
      calls[#calls + 1] = data
    end,
  }
  local original_system = nvim.system
  set_system(function(_, options)
    calls.stdout = options.stdout
    return fake_handle
  end)

  local client = assert(Acp.connect({ provider = "test-service", command = "agent", args = {} }, {
    on_error = function(message)
      errors[#errors + 1] = message
    end,
  }))
  assert(client:initialize())
  set_system(original_system)
  calls.stdout(nil, '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentCapabilities":{}}}\n')

  local completed = false
  assert(client:prompt({ sessionId = "grok-session", prompt = {} }, function()
    completed = true
  end))
  calls.stdout(nil, '{"jsonrpc":"2.0","id":2,"result":{"stopReason":"end_turn"}}\n')

  -- Captured immediately before Grok Session 01a05644-1efe-7d11-92f5-482a60f7f624
  -- entered Error: proxy/sessions/<id>/log.jsonl emitted this unsolicited response.
  calls.stdout(nil, '{"jsonrpc":"2.0","id":"skills-reload","result":{"result":{"reloaded":1}}}\n')

  MiniTest.expect.equality(completed, true)
  MiniTest.expect.equality(errors, {})
end

T["connect"]["rejects loading when the agent lacks loadSession capability"] = function()
  local calls = {}
  local fake_handle = {
    is_closing = function()
      return false
    end,
    write = function()
      calls.writes = (calls.writes or 0) + 1
    end,
  }
  local original_system = nvim.system
  set_system(function(_, options)
    calls.stdout = options.stdout
    return fake_handle
  end)

  local client = assert(Acp.connect({ provider = "test-service", command = "agent", args = {} }))
  assert(client:initialize())
  set_system(original_system)
  calls.stdout(nil, '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentCapabilities":{}}}\n')

  local request_id, request_error = client:load_session({ sessionId = "prior" })
  MiniTest.expect.equality(request_id, nil)
  MiniTest.expect.equality(request_error, "ACP agent does not support session/load")
  MiniTest.expect.equality(calls.writes, 1)
end

T["connect"]["lists sessions only when the agent advertises support"] = function()
  local calls = {}
  local fake_handle = {
    is_closing = function()
      return false
    end,
    write = function(_, data)
      calls[#calls + 1] = data
    end,
  }
  local original_system = nvim.system
  set_system(function(_, options)
    calls.stdout = options.stdout
    return fake_handle
  end)

  local client = assert(Acp.connect({ provider = "test-service", command = "agent", args = {} }))
  assert(client:initialize())
  set_system(original_system)
  calls.stdout(
    nil,
    '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentCapabilities":{"sessionCapabilities":{"list":{}}}}}\n'
  )

  local listed
  assert(client:list_sessions({ cwd = "/tmp/project" }, function(result)
    listed = result
  end))
  MiniTest.expect.equality(assert(Protocol.decode(calls[2]:sub(1, -2))), {
    id = 2,
    jsonrpc = "2.0",
    method = "session/list",
    params = { cwd = "/tmp/project" },
  })
  calls.stdout(nil, '{"jsonrpc":"2.0","id":2,"result":{"sessions":[]}}\n')
  MiniTest.expect.equality(listed, { sessions = {} })

  local listed_without_params
  assert(client:list_sessions(nil, function(result)
    listed_without_params = result
  end))
  MiniTest.expect.equality(calls[3]:find('"params":{}', 1, true) ~= nil, true)
  MiniTest.expect.equality(calls[3]:find('"params":[]', 1, true), nil)
  calls.stdout(nil, '{"jsonrpc":"2.0","id":3,"result":{"sessions":[]}}\n')
  MiniTest.expect.equality(listed_without_params, { sessions = {} })

  client.agent_capabilities = {}
  local request_id, request_error = client:list_sessions({})
  MiniTest.expect.equality(request_id, nil)
  MiniTest.expect.equality(request_error, "ACP agent does not support session/list")
end

T["connect"]["passes agent requests to the callback"] = function()
  local received
  local response
  local fake_handle = {
    is_closing = function()
      return false
    end,
    write = function(_, data)
      response = data
    end,
  }
  local original_system = nvim.system
  set_system(function(_, options)
    options.stdout(nil, '{"jsonrpc":"2.0","id":9,"method":"session/request_permission","params":{"sessionId":"s"}}\n')
    return fake_handle
  end)

  local client = assert(Acp.connect({ provider = "test-service", command = "agent", args = {} }, {
    on_request = function(request)
      received = request
    end,
  }))
  set_system(original_system)

  MiniTest.expect.equality(received.method, "session/request_permission")
  local sent, send_error = client:respond(9, { outcome = { outcome = "cancelled" } })
  MiniTest.expect.equality(sent, true)
  MiniTest.expect.equality(send_error, nil)
  MiniTest.expect.equality(assert(Protocol.decode(response:sub(1, -2))), {
    id = 9,
    jsonrpc = "2.0",
    result = { outcome = { outcome = "cancelled" } },
  })
end

T["connect"]["defers agent callbacks from fast events"] = function()
  local calls = {}
  local scheduled
  local received
  local fake_handle = {
    is_closing = function()
      return false
    end,
    write = function(_, data)
      calls.response = data
    end,
  }
  local original_system = nvim.system
  local original_in_fast_event = nvim.in_fast_event
  local original_schedule = nvim.schedule
  set_system(function(_, options)
    calls.stdout = options.stdout
    return fake_handle
  end)
  nvim.in_fast_event = function()
    return true
  end
  rawset(nvim, "schedule", function(callback)
    scheduled = callback
  end)

  local client = assert(Acp.connect({ provider = "test-service", command = "agent", args = {} }, {
    on_request = function(request)
      received = request
    end,
  }))

  calls.stdout(nil, '{"jsonrpc":"2.0","id":9,"method":"session/request_permission","params":{"sessionId":"s"}}\n')
  MiniTest.expect.equality(received, nil)
  MiniTest.expect.equality(type(scheduled), "function")

  set_system(original_system)
  nvim.in_fast_event = original_in_fast_event
  rawset(nvim, "schedule", original_schedule)
  scheduled()
  MiniTest.expect.equality(received.method, "session/request_permission")
  client:close()
end

return T
