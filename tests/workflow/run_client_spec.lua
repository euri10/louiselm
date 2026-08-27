local MiniTest = require("mini.test")
local RunClient = require("louiselm.workflow.run_client")
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

T["schedules snapshots from a real fast-event boundary and rereads on newer hints"] = function()
  local pipe = fake_pipe()
  local snapshots = {}
  local client = assert(RunClient.connect("/tmp/run.sock", function(runs)
    MiniTest.expect.equality(nvim.in_fast_event(), false)
    snapshots[#snapshots + 1] = runs
  end, {
    pipe_factory = function()
      return pipe
    end,
  }))
  pipe.connect_callback()

  local async
  async = nvim.uv.new_async(function()
    MiniTest.expect.equality(nvim.in_fast_event(), true)
    pipe.read_callback(
      nil,
      '{"type":"snapshot","runs":[{"id":"run","revision":1,"state":"active","generated_work_ceiling":3,"generated_work_consumed":0,"generated_work_reserved":0,"pending_mutation_ids":[],"park_expires_at_ms":0}]}\n'
    )
    pipe.read_callback(nil, '{"type":"run_changed","id":"run","revision":2}\n')
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

T["ignores work queued before disposal"] = function()
  local pipe = fake_pipe()
  local snapshots = 0
  local client = assert(RunClient.connect("/tmp/run.sock", function()
    snapshots = snapshots + 1
  end, {
    pipe_factory = function()
      return pipe
    end,
  }))
  pipe.connect_callback()
  pipe.read_callback(
    nil,
    '{"type":"snapshot","runs":[{"id":"run","revision":1,"state":"active","generated_work_ceiling":3,"generated_work_consumed":0,"generated_work_reserved":0,"pending_mutation_ids":[],"park_expires_at_ms":0}]}\n'
  )
  assert(client:dispose())
  nvim.wait(20)
  MiniTest.expect.equality(snapshots, 0)
  MiniTest.expect.equality(pipe.closing, true)
end

T["closes a disconnected handle so a new client can reconcile"] = function()
  local pipe = fake_pipe()
  local error_message
  assert(RunClient.connect("/tmp/run.sock", function() end, {
    pipe_factory = function()
      return pipe
    end,
    on_error = function(message)
      error_message = message
    end,
  }))
  pipe.connect_callback()
  pipe.read_callback(nil, nil)
  MiniTest.expect.equality(
    nvim.wait(1000, function()
      return error_message ~= nil
    end),
    true
  )
  MiniTest.expect.equality(error_message, "Run socket disconnected")
  MiniTest.expect.equality(pipe.closing, true)
end

T["correlates operator mutations and invokes callbacks once"] = function()
  local pipe = fake_pipe()
  local result
  local client = assert(RunClient.connect("/tmp/run.sock", function() end, {
    pipe_factory = function()
      return pipe
    end,
    operator_capability = "operator-secret",
  }))
  pipe.connect_callback()
  assert(client:raise("run", 3, 5, function(run, error_message)
    result = { run, error_message }
  end))
  local request = nvim.json.decode(pipe.writes[1])
  MiniTest.expect.equality(request, {
    type = "raise",
    id = "run",
    expected_revision = 3,
    ceiling = 5,
    request_id = "1",
    capability = "operator-secret",
  })
  pipe.read_callback(nil, nvim.json.encode({
    type = "mutation_result",
    request_id = "1",
    run = {
      id = "run",
      revision = 4,
      state = "parked",
      generated_work_ceiling = 5,
      generated_work_consumed = 1,
      generated_work_reserved = 0,
      pending_mutation_ids = {},
      park_expires_at_ms = 10,
    },
  }) .. "\n")
  MiniTest.expect.equality(
    nvim.wait(1000, function()
      return result ~= nil
    end),
    true
  )
  MiniTest.expect.equality(result[1].revision, 4)
  pipe.read_callback(nil, nvim.json.encode({
    type = "mutation_error",
    request_id = "1",
    message = "duplicate",
  }) .. "\n")
  nvim.wait(20)
  MiniTest.expect.equality(result[2], nil)
end

T["reads a private operator capability asynchronously"] = function()
  local path = nvim.fn.tempname()
  local capability = "12345678-1234-4234-8234-123456789abc"
  nvim.fn.writefile({ capability }, path, "b")
  local observed
  assert(RunClient.read_operator_capability(path, function(value, error_message)
    MiniTest.expect.equality(nvim.in_fast_event(), false)
    observed = { value, error_message }
  end))
  MiniTest.expect.equality(
    nvim.wait(1000, function()
      return observed ~= nil
    end),
    true
  )
  MiniTest.expect.equality(observed, { capability, nil })
  nvim.fn.delete(path)
end

return T
