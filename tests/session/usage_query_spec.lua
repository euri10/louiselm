local MiniTest = require("mini.test")
local Recording = require("louiselm.session.recording")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local directory, writer
local T = MiniTest.new_set({
  hooks = {
    pre_case = function()
      directory = nvim.fn.tempname()
      writer = assert(Recording.new(directory, function() end))
    end,
    post_case = function()
      nvim.fn.delete(directory, "rf")
    end,
  },
})
local function wait_for(predicate)
  assert(nvim.wait(6000, predicate, 10), "usage query did not settle")
end
local function flush()
  local done
  writer:flush(function(err)
    assert(err == nil, err and err.message)
    done = true
  end)
  wait_for(function()
    return done
  end)
end
local function cohort(model, enabled, agent, provider)
  return {
    agent = agent or "codex",
    provider = provider or "OpenAI",
    options = { model = model or "astra", reasoning = "medium", enabled = enabled == nil and false or enabled },
  }
end
-- Synthetic normalized recorder facts, not claimed ACP frames/order. Real wire
-- collection/order remains covered by recording_spec and option_recording_spec.
local function turn(id, filter, usage, costs, outcome)
  local record = nvim.deepcopy(filter)
  record.id, record.acp_session_id, record.prepared_at = id, "session", "2026-09-07T12:00:00Z"
  record.model = record.options.model
  record.cost_baseline = costs and costs[1]
  writer:append(record)
  writer:append({
    turn_id = id,
    sequence = 1,
    kind = "dispatch",
    observed_at = record.prepared_at,
    data = { request_id = id },
  })
  for i = 2, #(costs or {}) do
    writer:append({
      turn_id = id,
      sequence = i,
      kind = "cost",
      observed_at = record.prepared_at,
      data = { cost = costs[i] },
    })
  end
  if outcome ~= "active" then
    writer:append({
      turn_id = id,
      sequence = math.max(2, #(costs or {}) + 1),
      kind = "outcome",
      observed_at = record.prepared_at,
      data = { outcome = outcome or "completed", peer_response = true, usage = usage },
    })
  end
end
local function summaries(filters)
  local done, result, failure
  writer:usage_summaries(filters, function(rows, err)
    assert(not nvim.in_fast_event())
    done, result, failure = true, rows, err
  end)
  wait_for(function()
    return done
  end)
  return result, failure
end
T["exact typed cohorts exclude other Agents Providers options and mixed turns"] = function()
  turn("old-model", cohort("terra"), { total_tokens = 900 })
  turn("other-agent", cohort(nil, nil, "copilot"), { total_tokens = 800 })
  turn("other-service", cohort(nil, nil, nil, "Copilot"), { total_tokens = 700 })
  turn("string-false", cohort(nil, "false"), { total_tokens = 600 })
  turn("boolean-true", cohort(nil, true), { total_tokens = 500 })
  local extra = cohort()
  extra.options.extra = "present"
  turn("extra-option", extra, { total_tokens = 400 })
  turn("mixed", cohort(), { total_tokens = 300 })
  writer:append({
    kind = "options",
    id = "change",
    observer_id = "observer",
    sequence = 1,
    agent = "codex",
    acp_session_id = "session",
    observed_at = "2026-09-07T12:00:01Z",
    previous_options = cohort().options,
    options = cohort("terra").options,
    source = "notification",
    turn_id = "mixed",
  })
  flush()
  local rows = assert(summaries({ cohort() }))
  MiniTest.expect.equality(rows[1], { turns = 0, tokens = {}, costs = {}, outcomes = {} })
  turn("matching", cohort(), { total_tokens = 120, input_tokens = 100, thought_tokens = 0 })
  turn("unmeasured", cohort(), nil, nil, "cancelled")
  turn("active", cohort(), nil, nil, "active")
  flush()
  rows = assert(summaries({ cohort(), cohort("terra"), cohort(nil, true) }))
  MiniTest.expect.equality(rows[1], {
    turns = 3,
    tokens = {
      total_tokens = { samples = 1, average = 120 },
      input_tokens = { samples = 1, average = 100 },
      thought_tokens = { samples = 1, average = 0 },
    },
    costs = {},
    outcomes = { completed = 1, cancelled = 1, unobserved = 1 },
  })
  MiniTest.expect.equality(rows[2].tokens.total_tokens.average, 900)
  MiniTest.expect.equality(rows[3].tokens.total_tokens.average, 500)
end
T["cost means require a complete monotonic sequence and separate currencies"] = function()
  local function cost(amount, currency)
    return { amount = amount, currency = currency or "USD" }
  end
  turn("usd", cohort(), nil, { cost(1), cost(2), cost(4) })
  turn("eur", cohort(), nil, { cost(2, "EUR"), cost(4, "EUR") })
  turn("reset", cohort(), nil, { cost(10), cost(1), cost(4) })
  turn("switch", cohort(), nil, { cost(1), cost(2, "EUR") })
  turn("clear", cohort(), nil, { cost(1), nvim.NIL, cost(4) })
  turn("missing-baseline", cohort(), nil, { nvim.NIL, cost(4) })
  turn("no-reading", cohort(), nil, { cost(1) })
  turn("active-cost", cohort(), nil, { cost(1), cost(2) }, "active")
  turn("zero", cohort(), nil, { cost(1), cost(1) })
  flush()
  local rows = assert(summaries({ cohort() }))
  MiniTest.expect.equality(rows[1].turns, 9)
  MiniTest.expect.equality(rows[1].costs, {
    { currency = "EUR", samples = 1, average = 2 },
    { currency = "USD", samples = 2, average = 1.5 },
  })
end
T["missing storage is empty and invalid filters fail asynchronously without creation"] = function()
  MiniTest.expect.equality(assert(summaries({ cohort() }))[1].turns, 0)
  MiniTest.expect.equality(nvim.uv.fs_stat(directory), nil)
  for _, bad in ipairs({
    { agent = "codex", provider = "OpenAI", options = { enabled = 0 } },
    { agent = "codex", provider = "", options = {} },
    { agent = "codex", provider = "OpenAI", options = {}, extra = true },
  }) do
    local rows, err = summaries({ bad })
    MiniTest.expect.equality(rows, nil)
    MiniTest.expect.equality(err.code, "invalid")
  end
end
T["hostile strings remain values and malformed measured fields fail closed"] = function()
  local filter = cohort("'); DROP TABLE turns; --\n.shell false")
  turn("quoted", filter, { output_tokens = 12 })
  flush()
  MiniTest.expect.equality(assert(summaries({ filter }))[1].tokens.output_tokens.average, 12)
  local result = nvim
    .system({
      "sqlite3",
      writer.path,
      [[UPDATE turn_events SET data='{"outcome":"completed","peer_response":true,"usage":{"total_tokens":"12"}}' WHERE kind='outcome';]],
    })
    :wait()
  assert(result.code == 0)
  local rows, err = summaries({ filter })
  MiniTest.expect.equality(rows, nil)
  MiniTest.expect.equality(err.code, "corrupt")
end
return T
