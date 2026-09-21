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
  if enabled == nil then
    enabled = false
  end
  return {
    agent = agent or "codex",
    provider = provider or "OpenAI",
    options = { model = model or "astra", reasoning = "medium", enabled = enabled },
  }
end
-- Synthetic normalized recorder facts, not claimed ACP frames/order. Real wire
-- collection/order remains covered by recording_spec and option_recording_spec.
local function turn(id, filter, usage, costs, outcome, timestamp)
  local record = nvim.deepcopy(filter)
  record.id, record.acp_session_id, record.prepared_at = id, "session", timestamp or "2026-09-07T12:00:00Z"
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
local function explore(query)
  local done, result, failure
  writer:usage_query(query, function(rows, err)
    assert(not nvim.in_fast_event())
    done, result, failure = true, rows, err
  end)
  assert(not done, "query must be asynchronous")
  wait_for(function()
    return done
  end)
  return result, failure
end

T["explorer groups joint typed dimensions and UTC half-open time buckets"] = function()
  turn("before", cohort(), { total_tokens = 999 }, nil, nil, "2026-09-06T23:59:59Z")
  turn("first", cohort(), { total_tokens = 10 }, nil, nil, "2026-09-07T00:00:00Z")
  turn("second", cohort(), { input_tokens = 7 }, nil, "cancelled", "2026-09-07T00:59:59.999Z")
  turn("next", cohort(nil, "false"), { total_tokens = 20 }, nil, nil, "2026-09-07T01:00:00Z")
  turn("end", cohort(), { total_tokens = 999 }, nil, nil, "2026-09-08T00:00:00Z")
  flush()
  local query = {
    from = "2026-09-07T00:00:00Z",
    until_time = "2026-09-08T00:00:00Z",
    bucket = "hour",
    group_by = { "agent", "provider", "model", "option:enabled" },
    limit = 1,
  }
  local page = assert(explore(query))
  MiniTest.expect.equality(page.timezone, "UTC")
  MiniTest.expect.equality(page.total, 2)
  MiniTest.expect.equality(page.next_offset, 1)
  MiniTest.expect.equality(page.summary.turns, 3)
  MiniTest.expect.equality(page.summary.tokens.total_tokens, { samples = 2, average = 15, total = 30 })
  MiniTest.expect.equality(page.summary.tokens.input_tokens.samples, 1)
  MiniTest.expect.equality(page.rows[1].dimensions["option:enabled"], false)
  MiniTest.expect.equality(page.rows[1].bucket_start, "2026-09-07T00:00:00Z")
  MiniTest.expect.equality(page.rows[1].bucket_end, "2026-09-07T01:00:00Z")
  MiniTest.expect.equality(page.rows[1].summary.outcomes, { completed = 1, cancelled = 1 })
  query.offset = page.next_offset
  page = assert(explore(query))
  MiniTest.expect.equality(page.rows[1].dimensions["option:enabled"], "false")
  MiniTest.expect.equality(page.next_offset, nil)
  query.offset, query.filters = 0, { ["option:enabled"] = false, model = "astra" }
  MiniTest.expect.equality(assert(explore(query)).summary.turns, 2)
  query.bucket = "day"
  MiniTest.expect.equality(assert(explore(query)).rows[1].bucket_end, "2026-09-08T00:00:00Z")
end

T["explorer retains mixed totals and paged facts but excludes fixed comparisons"] = function()
  turn("fixed", cohort(), { total_tokens = 10 }, { { amount = 1, currency = "USD" }, { amount = 3, currency = "USD" } })
  turn("mixed", cohort(), { total_tokens = 30 }, { { amount = 1, currency = "EUR" }, { amount = 4, currency = "EUR" } })
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
    source = "response",
    request = { id = 7, option = "model", value = "terra" },
    turn_id = "mixed",
  })
  writer:append({
    kind = "options",
    id = "between",
    observer_id = "observer",
    sequence = 2,
    agent = "codex",
    acp_session_id = "session",
    observed_at = "2026-09-07T12:00:02Z",
    previous_options = cohort("terra").options,
    options = cohort().options,
    source = "notification",
  })
  flush()
  local page = assert(explore({}))
  MiniTest.expect.equality(page.summary.turns, 2)
  MiniTest.expect.equality(page.mixed_turns, 1)
  MiniTest.expect.equality(page.summary.costs, {
    { currency = "EUR", samples = 1, average = 3, total = 3 },
    { currency = "USD", samples = 1, average = 2, total = 2 },
  })
  page = assert(explore({ group_by = { "model" } }))
  MiniTest.expect.equality(page.excluded_mixed, 1)
  MiniTest.expect.equality(page.summary.turns, 1)
  page = assert(explore({ view = "turns", filters = { model = "astra" }, mixed = "only", limit = 1 }))
  MiniTest.expect.equality(page.rows[1].id, "mixed")
  MiniTest.expect.equality(page.rows[1].options.enabled, false)
  MiniTest.expect.equality(page.rows[1].mixed, true)
  MiniTest.expect.equality(page.rows[1].summary.tokens.total_tokens.total, 30)
  page = assert(explore({ view = "events", turn_id = "mixed", limit = 2 }))
  MiniTest.expect.equality(page.total, 4)
  MiniTest.expect.equality(page.next_offset, 2)
  local rest = assert(explore({ view = "events", turn_id = "mixed", limit = 2, offset = 2 }))
  MiniTest.expect.equality(rest.rows[2].data.request, { id = 7, option = "model", value = "terra" })
  MiniTest.expect.equality(rest.rows[2].data.source, "response")
  page = assert(explore({ view = "events", filters = { agent = "codex", session = "session" } }))
  MiniTest.expect.equality(page.rows[#page.rows].data.source, "notification")
  MiniTest.expect.equality(page.rows[#page.rows].turn_id, nil)
end

T["explorer discovers all typed dimensions without interpolating their names"] = function()
  local filter = cohort("'); DROP TABLE turns; --\n.shell false")
  filter.options['strange".option'] = true
  turn("quoted", filter, nil)
  flush()
  local page =
    assert(explore({ filters = { ['option:strange".option'] = true }, group_by = { 'option:strange".option' } }))
  MiniTest.expect.equality(page.summary.turns, 1)
  page = assert(explore({ view = "dimensions", limit = 100 }))
  local found = {}
  for _, row in ipairs(page.rows) do
    found[row.dimension] = true
  end
  for _, key in ipairs({ "agent", "provider", "model", "option:enabled", "option:reasoning", 'option:strange".option' }) do
    MiniTest.expect.equality(found[key], true)
  end
end

T["explorer validates closed query shapes and reports empty or corrupt stores"] = function()
  local page = assert(explore({}))
  MiniTest.expect.equality(page.rows, {})
  MiniTest.expect.equality(page.summary.turns, 0)
  MiniTest.expect.equality(nvim.uv.fs_stat(directory), nil)
  for _, bad in ipairs({
    { view = "raw" },
    { limit = 0 },
    { limit = 101 },
    { offset = -1 },
    { extra = true },
    { from = "2026-02-30T00:00:00Z" },
    { from = "2026-09-08T00:00:00Z", until_time = "2026-09-07T00:00:00Z" },
    { filters = { agent = false } },
    { filters = { ["option:enabled"] = 0 } },
    { group_by = { "anything" } },
    { group_by = { "agent", "agent" } },
    { bucket = "week" },
    { mixed = "guess" },
  }) do
    local rows, err = explore(bad)
    MiniTest.expect.equality(rows, nil)
    MiniTest.expect.equality(err.code, "invalid")
  end
  turn("bad", cohort(), { total_tokens = 1 })
  flush()
  local result = nvim
    .system({
      "sqlite3",
      writer.path,
      [[UPDATE turn_events SET data='{"outcome":"completed","peer_response":true,"usage":{"total_tokens":"12"}}' WHERE kind='outcome';]],
    })
    :wait()
  assert(result.code == 0)
  local rows, err = explore({})
  MiniTest.expect.equality(rows, nil)
  MiniTest.expect.equality(err.code, "corrupt")
end
T["explorer cancellation suppresses delivery and absent dimensions stay distinct"] = function()
  local delivered = false
  local cancel = writer:usage_query({}, function()
    delivered = true
  end)
  cancel()
  nvim.wait(20, function()
    return false
  end, 1)
  MiniTest.expect.equality(delivered, false)
  MiniTest.expect.equality(nvim.uv.fs_stat(directory), nil)
  local filter = cohort()
  filter.options.enabled = nil
  turn("missing", filter, nil)
  turn("present", cohort(), nil)
  flush()
  local page = assert(explore({ filters = { ["option:enabled"] = nvim.NIL }, group_by = { "option:enabled" } }))
  MiniTest.expect.equality(page.summary.turns, 1)
  MiniTest.expect.equality(page.rows[1].dimensions, {})
  local result = nvim.system({ "sqlite3", writer.path, [[UPDATE turns SET options='{"enabled":5}';]] }):wait()
  assert(result.code == 0)
  local rows, err = explore({ view = "dimensions" })
  MiniTest.expect.equality(rows, nil)
  MiniTest.expect.equality(err.code, "corrupt")
end
return T
