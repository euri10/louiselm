local MiniTest = require("mini.test")
local AttentionClient = require("louiselm.workflow.attention_client")

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim
local T = MiniTest.new_set()

local function fake_pipe()
  local pipe = { closing = false, writes = {} }
  function pipe:connect(_, callback)
    self.connect_callback = callback
  end
  function pipe:read_start(callback)
    self.read_callback = callback
  end
  function pipe:read_stop() end
  function pipe:write(value)
    self.writes[#self.writes + 1] = value
  end
  function pipe:is_closing()
    return self.closing
  end
  function pipe:close()
    self.closing = true
  end
  return pipe
end

local function snapshot(generation)
  return { generation = generation, items = {} }
end

T["schedules snapshots and rereads after generation invalidation"] = function()
  local pipe = fake_pipe()
  local snapshots = {}
  local client = assert(AttentionClient.connect("/tmp/attention.sock", function(value)
    MiniTest.expect.equality(nvim.in_fast_event(), false)
    snapshots[#snapshots + 1] = value
  end, {
    operator_capability = "operator-secret",
    pipe_factory = function()
      return pipe
    end,
  }))
  pipe.connect_callback()

  local async
  async = nvim.uv.new_async(function()
    MiniTest.expect.equality(nvim.in_fast_event(), true)
    pipe.read_callback(nil, nvim.json.encode({ type = "snapshot", snapshot = snapshot(1) }) .. "\n")
    pipe.read_callback(nil, nvim.json.encode({ type = "attention_changed", generation = 2 }) .. "\n")
    async:close()
  end)
  async:send()
  MiniTest.expect.equality(
    nvim.wait(1000, function()
      return #pipe.writes == 1
    end),
    true
  )
  MiniTest.expect.equality(#snapshots, 1)
  MiniTest.expect.equality(pipe.writes, { '{"type":"snapshot"}\n' })
  assert(client:dispose())
end

T["correlates mutation results and ignores late callbacks after disposal"] = function()
  local pipe = fake_pipe()
  local result
  local client = assert(AttentionClient.connect("/tmp/attention.sock", function() end, {
    operator_capability = "operator-secret",
    pipe_factory = function()
      return pipe
    end,
  }))
  pipe.connect_callback()
  pipe.read_callback(nil, nvim.json.encode({ type = "snapshot", snapshot = snapshot(0) }) .. "\n")
  nvim.wait(20)
  assert(client:upsert({ kind = "turn_ready" }, function(value, error_message)
    result = { value, error_message }
  end))
  local request = nvim.json.decode(pipe.writes[1])
  MiniTest.expect.equality(request.type, "upsert")
  MiniTest.expect.equality(request.request_id, "1")
  MiniTest.expect.equality(request.capability, "operator-secret")
  pipe.read_callback(nil, nvim.json.encode({
    type = "mutation_result",
    request_id = "1",
    snapshot = snapshot(1),
  }) .. "\n")
  MiniTest.expect.equality(
    nvim.wait(1000, function()
      return result ~= nil
    end),
    true
  )
  MiniTest.expect.equality(result[1].generation, 1)
  assert(client:dispose())
  pipe.read_callback(nil, nvim.json.encode({ type = "snapshot", snapshot = snapshot(2) }) .. "\n")
  nvim.wait(20)
end

T["clears all conditions for a Session"] = function()
  local pipe = fake_pipe()
  local client = assert(AttentionClient.connect("/tmp/attention.sock", function() end, {
    operator_capability = "operator-secret",
    pipe_factory = function()
      return pipe
    end,
  }))
  pipe.connect_callback()
  pipe.read_callback(nil, nvim.json.encode({ type = "snapshot", snapshot = snapshot(0) }) .. "\n")
  nvim.wait(20)
  assert(client:clear_session("codex/session-1", function() end))
  local request = nvim.json.decode(pipe.writes[1])
  MiniTest.expect.equality(request, {
    type = "clear_session",
    request_id = "1",
    session_id = "codex/session-1",
    capability = "operator-secret",
  })
  assert(client:dispose())
end

return T
