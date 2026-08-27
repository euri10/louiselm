local MiniTest = require("mini.test")
local Workflow = require("louiselm.workflow")

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

local function ledger()
  local calls = {}
  return {
    calls = calls,
    consume = function(mutation_id, kind, units)
      calls[#calls + 1] = { operation = "consume", mutation_id = mutation_id, kind = kind, units = units }
      return "consumed"
    end,
    reserve = function(mutation_id, kind, units)
      calls[#calls + 1] = { operation = "reserve", mutation_id = mutation_id, kind = kind, units = units }
      return "reserved"
    end,
    confirm = function(mutation_id, output_id)
      calls[#calls + 1] = { operation = "confirm", mutation_id = mutation_id, output_id = output_id }
      return true
    end,
    release = function(mutation_id)
      calls[#calls + 1] = { operation = "release", mutation_id = mutation_id }
      return true
    end,
  }
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
  assert(executor:advance("review"))
  assert(executor:advance("retry", "mutation-1"))
  assert(executor:advance("review"))
  assert(executor:advance("retry", "mutation-2"))
  assert(executor:advance("review"))
  local result = assert(executor:advance("retry", "mutation-3"))

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
  MiniTest.expect.equality(work.calls, {
    { operation = "consume", mutation_id = "mutation-1", kind = "back_edge", units = 1 },
    { operation = "consume", mutation_id = "mutation-2", kind = "back_edge", units = 1 },
  })
end

T["transitions"]["rejects unknown outcomes without advancing"] = function()
  local executor = assert(Workflow.new_executor("reference", loop_manifest(), { ledger = ledger() }))

  local result, error_message = executor:advance("missing")

  MiniTest.expect.equality(result, nil)
  MiniTest.expect.equality(error_message, "outcome 'missing' is not declared by stage 'entry'")
  MiniTest.expect.equality(executor:current_stage(), "entry")
end

T["transitions"]["does not advance after Park or disposal"] = function()
  local executor = assert(Workflow.new_executor("reference", loop_manifest(), { ledger = ledger() }))

  assert(executor:park())
  local parked_result, parked_error = executor:advance("review")
  MiniTest.expect.equality(parked_result, nil)
  MiniTest.expect.equality(parked_error, "workflow Run is parked")

  assert(executor:resume())
  assert(executor:dispose())
  local disposed_result, disposed_error = executor:advance("review")
  MiniTest.expect.equality(disposed_result, nil)
  MiniTest.expect.equality(disposed_error, "workflow Run is disposed")
end

T["transitions"]["keeps state unchanged while a back-edge charge is pending"] = function()
  local work = ledger()
  function work.consume()
    return "pending", "ledger unavailable"
  end
  local executor = assert(Workflow.new_executor("reference", loop_manifest(), { ledger = work }))
  assert(executor:advance("review"))

  local result, error_message = executor:advance("retry", "mutation-1")

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

T["transitions"]["cancellation prevents late outcomes"] = function()
  local executor = assert(Workflow.new_executor("reference", loop_manifest(), { ledger = ledger() }))

  assert(executor:cancel())
  local result, error_message = executor:advance("review")

  MiniTest.expect.equality(result, nil)
  MiniTest.expect.equality(error_message, "workflow Run is cancelled")
  MiniTest.expect.equality(executor:inspect().status, "cancelled")
end

T["generators"] = MiniTest.new_set()

T["generators"]["reserve outputs and release unused capacity"] = function()
  local manifest = loop_manifest()
  manifest.review.generates = { max = 3 }
  local work = ledger()
  local executor = assert(Workflow.new_executor("reference", manifest, { ledger = work }))
  assert(executor:advance("review"))

  assert(executor:begin_generator("generator-1"))
  assert(executor:record_generator_output("issue-1"))
  assert(executor:end_generator())

  MiniTest.expect.equality(work.calls, {
    { operation = "reserve", mutation_id = "generator-1", kind = "skill_generator", units = 3 },
    { operation = "confirm", mutation_id = "generator-1", output_id = "issue-1" },
    { operation = "release", mutation_id = "generator-1" },
  })
end

return T
