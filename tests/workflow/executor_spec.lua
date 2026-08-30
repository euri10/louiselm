local MiniTest = require("mini.test")
local Workflow = require("louiselm.workflow")
---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local T = MiniTest.new_set()

local function loop_manifest()
  return {
    entry = {
      workflow = "reference",
      entry = true,
      ["generated-work"] = { max = 5 },
      ["park-expiry"] = "1h",
      outcomes = { { name = "review", to = "review" } },
    },
    review = {
      workflow = "reference",
      outcomes = {
        {
          name = "retry",
          to = "entry",
          ["back-edge"] = true,
          resolver = "agent",
          ["max-iterations"] = 2,
          ["on-exhausted"] = "accepted",
        },
        { name = "accepted", terminal = true },
      },
    },
  }
end

---Build a recording ledger double that answers the way the real one does.
---
---The production ledger is a `louiselm-capture` subprocess whose reply reaches
---Lua through a `vim.system` callback marshalled with `vim.schedule`. Replying
---synchronously here would exercise a context that never occurs in production,
---which AGENTS.md rules out as insufficient async coverage.
---@param overrides? table<string, unknown> Per-operation replies.
local function ledger(overrides)
  overrides = overrides or {}
  local calls = {}
  local held = {}
  local function reply(operation, callback, first, second)
    if overrides.hold == operation then
      held[#held + 1] = function()
        callback(first, second)
      end
      return
    end
    nvim.schedule(function()
      callback(first, second)
    end)
  end
  return {
    calls = calls,
    ---Release every ledger reply withheld by the `hold` override.
    flush = function()
      local pending = held
      held = {}
      for _, resume in ipairs(pending) do
        nvim.schedule(resume)
      end
    end,
    consume = function(mutation_id, kind, units, callback)
      calls[#calls + 1] = { operation = "consume", mutation_id = mutation_id, kind = kind, units = units }
      reply("consume", callback, overrides.consume or "consumed", overrides.consume_error)
    end,
    reserve = function(mutation_id, kind, units, callback)
      calls[#calls + 1] = { operation = "reserve", mutation_id = mutation_id, kind = kind, units = units }
      reply("reserve", callback, overrides.reserve or "reserved", overrides.reserve_error)
    end,
    confirm = function(mutation_id, output_id, callback)
      calls[#calls + 1] = { operation = "confirm", mutation_id = mutation_id, output_id = output_id }
      reply("confirm", callback, overrides.confirm ~= false, overrides.confirm_error)
    end,
    release = function(mutation_id, callback)
      calls[#calls + 1] = { operation = "release", mutation_id = mutation_id }
      reply("release", callback, overrides.release ~= false, overrides.release_error)
    end,
  }
end

---Start one executor operation and wait for its asynchronous completion.
---@param operation fun(callback: fun(first: unknown, second: string?)): boolean, string?
---@return boolean started
---@return unknown value
---@return string? error_message
local function await(operation)
  local fired, value, message = false, nil, nil
  local started, start_error = operation(function(first, second)
    fired, value, message = true, first, second
  end)
  if not started then
    return false, nil, start_error
  end
  nvim.wait(1000, function()
    return fired
  end)
  MiniTest.expect.equality(fired, true)
  return true, value, message
end

---@param executor table
---@param outcome string
---@param mutation_id? string
local function advance(executor, outcome, mutation_id)
  return await(function(callback)
    return executor:advance(outcome, mutation_id, callback)
  end)
end

T["transitions"] = MiniTest.new_set()

T["transitions"]["follows named outcomes and charges bounded automatic back-edges"] = function()
  local work = ledger()
  local executor = assert(Workflow.new_executor("reference", loop_manifest(), { ledger = work }))

  MiniTest.expect.equality(executor:inspect(), {
    status = "active",
    current_stage = "entry",
    iterations = {},
    history = {},
  })
  assert(advance(executor, "review"))
  assert(advance(executor, "retry", "mutation-1"))
  assert(advance(executor, "review"))
  assert(advance(executor, "retry", "mutation-2"))
  assert(advance(executor, "review"))
  local started, result = advance(executor, "retry", "mutation-3")
  assert(started)

  MiniTest.expect.equality(result, {
    from = "review",
    outcome = "accepted",
    terminal = true,
    exhausted = true,
    requested_outcome = "retry",
  })
  local snapshot = executor:inspect()
  MiniTest.expect.equality(snapshot.status, "completed")
  MiniTest.expect.equality(snapshot.current_stage, nil)
  MiniTest.expect.equality(snapshot.iterations, { ["review\0retry"] = 2 })
  MiniTest.expect.equality(snapshot.history[#snapshot.history], result)
  -- The third traversal routes to exhaustion without a third charge.
  MiniTest.expect.equality(work.calls, {
    { operation = "consume", mutation_id = "mutation-1", kind = "back_edge", units = 1 },
    { operation = "consume", mutation_id = "mutation-2", kind = "back_edge", units = 1 },
  })
end

T["transitions"]["completes only after the scheduling boundary"] = function()
  local executor = assert(Workflow.new_executor("reference", loop_manifest(), { ledger = ledger() }))

  local fired = false
  local started = executor:advance("review", nil, function()
    fired = true
  end)

  -- A transition that never reaches the ledger still settles asynchronously, so
  -- callers never have to ask which outcomes happen to be synchronous.
  assert(started)
  MiniTest.expect.equality(fired, false)
  nvim.wait(1000, function()
    return fired
  end)
  MiniTest.expect.equality(fired, true)
end

T["transitions"]["rejects unknown outcomes without advancing"] = function()
  local executor = assert(Workflow.new_executor("reference", loop_manifest(), { ledger = ledger() }))

  local started, _, error_message = advance(executor, "missing")

  MiniTest.expect.equality(started, false)
  MiniTest.expect.equality(error_message, "outcome 'missing' is not declared by stage 'entry'")
  MiniTest.expect.equality(executor:current_stage(), "entry")
end

T["transitions"]["does not advance after Park or disposal"] = function()
  local executor = assert(Workflow.new_executor("reference", loop_manifest(), { ledger = ledger() }))

  assert(executor:park())
  local parked_started, _, parked_error = advance(executor, "review")
  MiniTest.expect.equality(parked_started, false)
  MiniTest.expect.equality(parked_error, "workflow Run is parked")

  assert(executor:resume())
  assert(await(function(callback)
    return executor:dispose(callback)
  end))
  local disposed_started, _, disposed_error = advance(executor, "review")
  MiniTest.expect.equality(disposed_started, false)
  MiniTest.expect.equality(disposed_error, "workflow Run is disposed")
end

T["transitions"]["keeps state unchanged while a back-edge charge is pending"] = function()
  local work = ledger({ consume = "pending", consume_error = "ledger unavailable" })
  local executor = assert(Workflow.new_executor("reference", loop_manifest(), { ledger = work }))
  assert(advance(executor, "review"))

  local started, result, error_message = advance(executor, "retry", "mutation-1")

  assert(started)
  MiniTest.expect.equality(result, nil)
  MiniTest.expect.equality(error_message, "ledger unavailable")
  MiniTest.expect.equality(executor:inspect(), {
    status = "active",
    current_stage = "review",
    iterations = {},
    history = {
      { from = "entry", outcome = "review", to = "review", terminal = false },
    },
  })
end

T["transitions"]["refuses a second operation while a ledger call is in flight"] = function()
  local work = ledger({ hold = "consume" })
  local executor = assert(Workflow.new_executor("reference", loop_manifest(), { ledger = work }))
  assert(advance(executor, "review"))

  local fired = false
  assert(executor:advance("retry", "mutation-1", function()
    fired = true
  end))

  -- Without this guard two concurrent traversals would both pass validation and
  -- both charge, spending two units for one logical move.
  local started, _, error_message = advance(executor, "retry", "mutation-2")
  MiniTest.expect.equality(started, false)
  MiniTest.expect.equality(error_message, "a ledger operation is already in flight")

  work.flush()
  nvim.wait(1000, function()
    return fired
  end)
  MiniTest.expect.equality(#work.calls, 1)
  MiniTest.expect.equality(executor:inspect().iterations, { ["review\0retry"] = 1 })
end

T["transitions"]["disposal during an in-flight charge does not revive the workflow"] = function()
  local work = ledger({ hold = "consume" })
  local executor = assert(Workflow.new_executor("reference", loop_manifest(), { ledger = work }))
  assert(advance(executor, "review"))

  local late_result, late_error, fired = nil, nil, false
  assert(executor:advance("retry", "mutation-1", function(result, error_message)
    late_result, late_error, fired = result, error_message, true
  end))
  assert(await(function(callback)
    return executor:dispose(callback)
  end))

  work.flush()
  nvim.wait(1000, function()
    return fired
  end)
  MiniTest.expect.equality(late_result, nil)
  MiniTest.expect.equality(late_error, "workflow Run is disposed")
  local snapshot = executor:inspect()
  MiniTest.expect.equality(snapshot.status, "disposed")
  MiniTest.expect.equality(snapshot.current_stage, "review")
  MiniTest.expect.equality(snapshot.iterations, {})
end

T["transitions"]["cancellation prevents late outcomes"] = function()
  local executor = assert(Workflow.new_executor("reference", loop_manifest(), { ledger = ledger() }))

  assert(executor:cancel())
  local started, result, error_message = advance(executor, "review")

  MiniTest.expect.equality(started, false)
  MiniTest.expect.equality(result, nil)
  MiniTest.expect.equality(error_message, "workflow Run is cancelled")
  MiniTest.expect.equality(executor:inspect().status, "cancelled")
end

T["generators"] = MiniTest.new_set()

local function generating_manifest()
  local manifest = loop_manifest()
  manifest.review.generates = { max = 3 }
  return manifest
end

T["generators"]["reserve outputs and release unused capacity"] = function()
  local work = ledger()
  local executor = assert(Workflow.new_executor("reference", generating_manifest(), { ledger = work }))
  assert(advance(executor, "review"))

  assert(await(function(callback)
    return executor:begin_generator("generator-1", callback)
  end))
  assert(await(function(callback)
    return executor:record_generator_output("issue-1", callback)
  end))
  assert(await(function(callback)
    return executor:end_generator(callback)
  end))

  MiniTest.expect.equality(work.calls, {
    { operation = "reserve", mutation_id = "generator-1", kind = "skill_generator", units = 3 },
    { operation = "confirm", mutation_id = "generator-1", output_id = "issue-1" },
    { operation = "release", mutation_id = "generator-1" },
  })
  MiniTest.expect.equality(executor:inspect().generator, nil)
end

T["generators"]["a refused reservation leaves no Generator to charge against"] = function()
  local work = ledger({ reserve = "exhausted" })
  local executor = assert(Workflow.new_executor("reference", generating_manifest(), { ledger = work }))
  assert(advance(executor, "review"))

  local started, opened, error_message = await(function(callback)
    return executor:begin_generator("generator-1", callback)
  end)

  assert(started)
  MiniTest.expect.equality(opened, false)
  MiniTest.expect.equality(error_message, "workflow Run budget exhausted")
  MiniTest.expect.equality(executor:inspect().status, "parked")
  MiniTest.expect.equality(executor:inspect().generator, nil)

  -- No output can be confirmed against a reservation that was never granted.
  local output_started, _, output_error = await(function(callback)
    return executor:record_generator_output("issue-1", callback)
  end)
  MiniTest.expect.equality(output_started, false)
  MiniTest.expect.equality(output_error, "workflow Run is parked")
  MiniTest.expect.equality(#work.calls, 1)
end

T["generators"]["disposal releases an outstanding reservation"] = function()
  local work = ledger()
  local executor = assert(Workflow.new_executor("reference", generating_manifest(), { ledger = work }))
  assert(advance(executor, "review"))
  assert(await(function(callback)
    return executor:begin_generator("generator-1", callback)
  end))

  assert(await(function(callback)
    return executor:dispose(callback)
  end))

  MiniTest.expect.equality(work.calls[#work.calls], { operation = "release", mutation_id = "generator-1" })
  MiniTest.expect.equality(executor:inspect().status, "disposed")
end

T["generators"]["disposal is final even when the ledger refuses the release"] = function()
  local work = ledger({ release = false, release_error = "ledger unreachable" })
  local executor = assert(Workflow.new_executor("reference", generating_manifest(), { ledger = work }))
  assert(advance(executor, "review"))
  assert(await(function(callback)
    return executor:begin_generator("generator-1", callback)
  end))

  local started, disposed, error_message = await(function(callback)
    return executor:dispose(callback)
  end)

  -- The failure is reported, but an unreachable ledger cannot hold a workflow
  -- open; the service-side reaper owns reclaiming the stranded reservation.
  assert(started)
  MiniTest.expect.equality(disposed, false)
  MiniTest.expect.equality(error_message, "ledger unreachable")
  MiniTest.expect.equality(executor:inspect().status, "disposed")
end

return T
