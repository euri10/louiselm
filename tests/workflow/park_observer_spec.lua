local MiniTest = require("mini.test")
local ParkObserver = require("louiselm.workflow.park_observer")
local RunClient = require("louiselm.workflow.run_client")
local Workflow = require("louiselm.workflow")
---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim
local T = MiniTest.new_set()

local function fake_pipe()
  local pipe = { closing = false }
  function pipe:connect(_, callback)
    self.connect_callback = callback
  end
  function pipe:read_start(callback)
    self.read_callback = callback
  end
  function pipe:read_stop() end
  function pipe:is_closing()
    return self.closing
  end
  function pipe:close()
    self.closing = true
  end
  function pipe:write() end
  return pipe
end

local function parked_snapshot(revision)
  return nvim.json.encode({
    type = "snapshot",
    runs = {
      {
        id = "run",
        revision = revision,
        state = "parked",
        generated_work_ceiling = 1,
        generated_work_consumed = 1,
        generated_work_reserved = 0,
        pending_mutation_ids = {},
        triggering_mutation_id = "mutation",
        park_expires_at_ms = 60000,
      },
    },
  }) .. "\n"
end

T["parks live work and presents bounded state only after scheduling"] = function()
  local worker = { cancelled = 0 }
  function worker:inspect()
    return { status = "prompting" }
  end
  function worker:cancel()
    MiniTest.expect.equality(nvim.in_fast_event(), false)
    self.cancelled = self.cancelled + 1
    return true
  end
  function worker:dispose()
    return true
  end
  local run = assert(Workflow.new_run())
  assert(run:adopt_session(worker))
  local presented
  local observer = assert(ParkObserver.new({
    find_run = function(id)
      return id == "run" and run or nil
    end,
    on_park = function(value)
      MiniTest.expect.equality(nvim.in_fast_event(), false)
      presented = value
    end,
  }))
  local pipe = fake_pipe()
  assert(RunClient.connect("/tmp/run.sock", function(runs)
    assert(observer:observe(runs))
  end, {
    pipe_factory = function()
      return pipe
    end,
  }))
  pipe.connect_callback()
  local async
  async = nvim.uv.new_async(function()
    MiniTest.expect.equality(nvim.in_fast_event(), true)
    pipe.read_callback(nil, parked_snapshot(2))
    async:close()
  end)
  async:send()
  MiniTest.expect.equality(
    nvim.wait(1000, function()
      return presented ~= nil
    end),
    true
  )
  MiniTest.expect.equality(worker.cancelled, 1)
  MiniTest.expect.equality(presented, {
    id = "run",
    revision = 2,
    state = "parked",
    ceiling = 1,
    consumed = 1,
    reserved = 0,
    pending_mutation_ids = {},
    triggering_mutation_id = "mutation",
    expires_at_ms = 60000,
  })
  assert(observer:observe(nvim.json.decode(parked_snapshot(2)).runs))
  MiniTest.expect.equality(worker.cancelled, 1)
end

T["late scheduled snapshots are inert after observer disposal"] = function()
  local pipe = fake_pipe()
  local presented = 0
  local observer = assert(ParkObserver.new({
    find_run = function()
      return nil
    end,
    on_park = function()
      presented = presented + 1
    end,
  }))
  assert(RunClient.connect("/tmp/run.sock", function(runs)
    observer:observe(runs)
  end, {
    pipe_factory = function()
      return pipe
    end,
  }))
  pipe.connect_callback()
  pipe.read_callback(nil, parked_snapshot(1))
  assert(observer:dispose())
  nvim.wait(20)
  MiniTest.expect.equality(presented, 0)
end

return T
