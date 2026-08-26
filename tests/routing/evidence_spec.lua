local MiniTest = require("mini.test")
local Evidence = require("louiselm.routing.evidence")

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim
local temp_dir
local T = MiniTest.new_set({
  hooks = {
    pre_case = function()
      temp_dir = nvim.fn.tempname()
      assert(nvim.fn.mkdir(temp_dir, "p") == 1)
    end,
    post_case = function()
      nvim.fn.delete(temp_dir, "rf")
      temp_dir = nil
    end,
  },
})

local function store_path()
  return nvim.fs.joinpath(temp_dir, "routing-evidence.json")
end

local function new_store()
  return assert(Evidence.new(store_path()))
end

local function target(overrides)
  local value = { phase = "design", agent = "alpha", model = "sonnet" }
  for key, override in pairs(overrides or {}) do
    value[key] = override
  end
  return value
end

---@return louiselm.routing.RoutingEvidence
local function record_for(store, phase, agent, model)
  local records = assert(store:evidence())
  for _, record in ipairs(records) do
    if record.phase == phase and record.agent == agent and record.model == model then
      return record
    end
  end
  error("no evidence for phase=" .. tostring(phase) .. " agent=" .. agent .. " model=" .. tostring(model))
end

T["observe"] = MiniTest.new_set()

T["observe"]["counts a completed turn as a reliable sample"] = function()
  local store = new_store()

  assert(store:observe(target(), "completed"))

  local record = record_for(store, "design", "alpha", "sonnet")
  MiniTest.expect.equality(record.samples, 1)
  MiniTest.expect.equality(record.reliability, 1)
end

T["observe"]["counts a failed turn against reliability"] = function()
  local store = new_store()

  assert(store:observe(target(), "completed"))
  assert(store:observe(target(), "failed"))

  local record = record_for(store, "design", "alpha", "sonnet")
  MiniTest.expect.equality(record.samples, 2)
  MiniTest.expect.equality(record.reliability, 0.5)
end

T["observe"]["does not count a cancelled turn against the agent"] = function()
  local store = new_store()

  assert(store:observe(target(), "completed"))
  assert(store:observe(target(), "cancelled"))

  local record = record_for(store, "design", "alpha", "sonnet")
  MiniTest.expect.equality(record.samples, 1)
  MiniTest.expect.equality(record.reliability, 1)
end

T["observe"]["also accumulates a global record across phases"] = function()
  local store = new_store()

  assert(store:observe(target({ phase = "design" }), "completed"))
  assert(store:observe(target({ phase = "qa" }), "completed"))

  MiniTest.expect.equality(record_for(store, nil, "alpha", "sonnet").samples, 2)
  MiniTest.expect.equality(record_for(store, "design", "alpha", "sonnet").samples, 1)
end

T["observe"]["keeps records for different models apart"] = function()
  local store = new_store()

  assert(store:observe(target({ model = "sonnet" }), "completed"))
  assert(store:observe(target({ model = "opus" }), "failed"))

  MiniTest.expect.equality(record_for(store, "design", "alpha", "sonnet").reliability, 1)
  MiniTest.expect.equality(record_for(store, "design", "alpha", "opus").reliability, 0)
end

T["observe"]["bounds how far one observation can move an established record"] = function()
  local store = new_store()
  for _ = 1, 200 do
    assert(store:observe(target(), "completed"))
  end
  local before = record_for(store, "design", "alpha", "sonnet").reliability

  assert(store:observe(target(), "failed"))
  local after = record_for(store, "design", "alpha", "sonnet").reliability

  MiniTest.expect.equality(before, 1)
  MiniTest.expect.equality(after < 1, true)
  MiniTest.expect.equality(after > 0.9, true)
end

T["observe"]["rejects an observation with no agent"] = function()
  local store = new_store()

  local ok, error_message = store:observe({ phase = "design" }, "completed")

  MiniTest.expect.equality(ok, false)
  MiniTest.expect.equality(error_message, "evidence requires an agent")
end

T["observe"]["rejects an unknown outcome"] = function()
  local store = new_store()

  local ok, error_message = store:observe(target(), "exploded")

  MiniTest.expect.equality(ok, false)
  MiniTest.expect.equality(error_message, "outcome must be completed, failed, or cancelled")
end

T["observe"]["rejects a phase outside the canonical contract"] = function()
  local store = new_store()

  local ok, error_message = store:observe(target({ phase = "testing" }), "completed")

  MiniTest.expect.equality(ok, false)
  MiniTest.expect.equality(
    error_message,
    "unknown phase 'testing'; expected one of design, planning, implementation, review, qa, mechanical"
  )
end

T["feedback"] = MiniTest.new_set()

T["feedback"]["records good as full quality"] = function()
  local store = new_store()
  assert(store:observe(target(), "completed"))

  assert(store:feedback(target(), "good"))

  MiniTest.expect.equality(record_for(store, "design", "alpha", "sonnet").quality, 1)
end

T["feedback"]["records poor as no quality and keeps the context"] = function()
  local store = new_store()
  assert(store:observe(target(), "completed"))

  assert(store:feedback(target(), "poor", "invented an API that does not exist"))

  MiniTest.expect.equality(record_for(store, "design", "alpha", "sonnet").quality, 0)
  local records = assert(store:records())
  MiniTest.expect.equality(records[1].notes, { "invented an API that does not exist" })
end

T["feedback"]["refuses poor without context"] = function()
  local store = new_store()

  local ok, error_message = store:feedback(target(), "poor")

  MiniTest.expect.equality(ok, false)
  MiniTest.expect.equality(error_message, "poor feedback requires context")
end

T["feedback"]["refuses poor with only whitespace for context"] = function()
  local store = new_store()

  local ok, error_message = store:feedback(target(), "poor", "   ")

  MiniTest.expect.equality(ok, false)
  MiniTest.expect.equality(error_message, "poor feedback requires context")
end

T["feedback"]["treats skip as an answer that records no opinion"] = function()
  local store = new_store()
  assert(store:observe(target(), "completed"))
  assert(store:feedback(target(), "good"))

  assert(store:feedback(target(), "skip"))

  MiniTest.expect.equality(record_for(store, "design", "alpha", "sonnet").quality, 1)
end

T["feedback"]["averages repeated ratings"] = function()
  local store = new_store()
  assert(store:observe(target(), "completed"))

  assert(store:feedback(target(), "good"))
  assert(store:feedback(target(), "poor", "hallucinated a config key"))

  MiniTest.expect.equality(record_for(store, "design", "alpha", "sonnet").quality, 0.5)
end

T["feedback"]["keeps only the most recent context notes"] = function()
  local store = new_store()
  for index = 1, 8 do
    assert(store:feedback(target(), "poor", "failure number " .. index))
  end

  local records = assert(store:records())
  MiniTest.expect.equality(#records[1].notes, 5)
  MiniTest.expect.equality(records[1].notes[5], "failure number 8")
end

T["feedback"]["truncates an overlong note rather than storing it whole"] = function()
  local store = new_store()

  assert(store:feedback(target(), "poor", string.rep("x", 900)))

  local records = assert(store:records())
  MiniTest.expect.equality(#records[1].notes[1], 500)
end

T["feedback"]["rejects an unknown rating"] = function()
  local store = new_store()

  local ok, error_message = store:feedback(target(), "excellent")

  MiniTest.expect.equality(ok, false)
  MiniTest.expect.equality(error_message, "rating must be good, skip, or poor")
end

T["persistence"] = MiniTest.new_set()

T["persistence"]["survives a restart"] = function()
  local store = new_store()
  assert(store:observe(target(), "completed"))
  assert(store:feedback(target(), "good"))

  local reopened = assert(Evidence.new(store_path()))

  local record = record_for(reopened, "design", "alpha", "sonnet")
  MiniTest.expect.equality(record.samples, 1)
  MiniTest.expect.equality(record.quality, 1)
end

T["persistence"]["reads an absent file as no evidence"] = function()
  MiniTest.expect.equality(assert(new_store():evidence()), {})
end

T["persistence"]["reports a corrupt file instead of silently discarding it"] = function()
  assert(nvim.fn.writefile({ "not json at all" }, store_path()) == 0)
  local store = new_store()

  local records, error_message = store:evidence()

  MiniTest.expect.equality(records, nil)
  MiniTest.expect.equality(error_message, "routing evidence is not valid JSON")
end

T["persistence"]["refuses to write over a corrupt file"] = function()
  assert(nvim.fn.writefile({ "not json at all" }, store_path()) == 0)
  local store = new_store()

  local ok, error_message = store:observe(target(), "completed")

  MiniTest.expect.equality(ok, false)
  MiniTest.expect.equality(error_message, "routing evidence is not valid JSON")
  MiniTest.expect.equality(nvim.fn.readfile(store_path()), { "not json at all" })
end

T["persistence"]["rejects a file written by a newer version"] = function()
  assert(nvim.fn.writefile({ nvim.json.encode({ version = 2, records = {} }) }, store_path()) == 0)

  local records, error_message = new_store():evidence()

  MiniTest.expect.equality(records, nil)
  MiniTest.expect.equality(error_message, "routing evidence has invalid schema")
end

T["persistence"]["stores nothing beyond the declared operational fields"] = function()
  local store = new_store()
  assert(store:observe(target({ options = { effort = "high", think = true } }), "completed"))
  assert(store:feedback(target(), "poor", "wrong answer"))

  local stored = nvim.json.decode(table.concat(nvim.fn.readfile(store_path()), "\n"))
  local allowed = {
    phase = true,
    agent = true,
    model = true,
    options = true,
    samples = true,
    successes = true,
    quality_total = true,
    quality_samples = true,
    notes = true,
    updated_at = true,
  }
  for _, record in ipairs(stored.records) do
    for key in pairs(record) do
      MiniTest.expect.equality(allowed[key] ~= nil, true, "unexpected persisted key " .. key)
    end
  end
end

T["persistence"]["records the configuration and a timestamp alongside the counters"] = function()
  local store = new_store()

  assert(store:observe(target({ options = { effort = "high" } }), "completed"))

  local records = assert(store:records())
  MiniTest.expect.equality(records[1].options, { effort = "high" })
  MiniTest.expect.equality(type(records[1].updated_at), "number")
end

T["persistence"]["rejects options that are not a flat string-keyed map"] = function()
  local store = new_store()

  local ok, error_message = store:observe(target({ options = { effort = { "high" } } }), "completed")

  MiniTest.expect.equality(ok, false)
  MiniTest.expect.equality(error_message, "options must map option ids to strings or booleans")
end

T["evidence"] = MiniTest.new_set()

T["evidence"]["is shaped for the ranking boundary"] = function()
  local store = new_store()
  assert(store:observe(target(), "completed"))
  assert(store:feedback(target(), "good"))

  local record = record_for(store, "design", "alpha", "sonnet")

  MiniTest.expect.equality(record, {
    phase = "design",
    agent = "alpha",
    model = "sonnet",
    samples = 1,
    reliability = 1,
    quality = 1,
  })
end

T["evidence"]["omits quality when nothing has been rated"] = function()
  local store = new_store()
  assert(store:observe(target(), "completed"))

  MiniTest.expect.equality(record_for(store, "design", "alpha", "sonnet").quality, nil)
end

T["evidence"]["orders records deterministically"] = function()
  local store = new_store()
  assert(store:observe(target({ agent = "zulu" }), "completed"))
  assert(store:observe(target({ agent = "alpha" }), "completed"))

  local first = assert(store:evidence())
  local second = assert(Evidence.new(store_path()):evidence())

  MiniTest.expect.equality(first, second)
end

return T
