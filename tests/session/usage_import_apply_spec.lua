local MiniTest = require("mini.test")
local Import = require("louiselm.session.usage_import")
local Recording = require("louiselm.session.recording")

---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local fixture = dofile(nvim.fn.getcwd() .. "/tests/fixtures/usage_import.lua")
local eq = MiniTest.expect.equality

local function write_records(path, records)
  local lines = {}
  for _, record in ipairs(records) do
    lines[#lines + 1] = nvim.json.encode(record)
  end
  assert(nvim.fn.writefile(lines, path) == 0)
end

local function setup()
  local directory = nvim.fn.tempname()
  assert(nvim.fn.mkdir(directory, "p", 448) == 1)
  MiniTest.finally(function()
    nvim.fn.delete(directory, "rf")
  end)
  local session_path = directory .. "/session.jsonl"
  local connection_path = directory .. "/first-connection.jsonl"
  local resumed_path = directory .. "/resumed-connection.jsonl"
  local records = nvim.deepcopy(fixture.first)
  for _, record in ipairs(fixture.second) do
    records[#records + 1] = nvim.deepcopy(record)
  end
  write_records(session_path, records)
  write_records(connection_path, fixture.connection)
  write_records(resumed_path, fixture.resumed_connection)
  local proposal = { agent = "sample-agent", provider = "sample-provider" }
  local selection = {
    from = "2026-08-31T17:00:00Z",
    until_time = "2026-08-31T18:00:00Z",
    max_files = 4,
    max_bytes = 100000,
    sources = {
      {
        kind = "acp",
        session_id = fixture.session,
        connection_id = "first-connection",
        connection_path = connection_path,
        session_path = session_path,
        first_line = 1,
        last_line = #fixture.first,
        proposal = proposal,
      },
      {
        kind = "acp",
        session_id = fixture.session,
        connection_id = "resumed-connection",
        connection_path = resumed_path,
        session_path = session_path,
        first_line = #fixture.first + 1,
        last_line = #records,
        proposal = nvim.deepcopy(proposal),
      },
    },
  }
  local store = assert(Recording.new(directory .. "/usage", function() end))
  return selection, store, records
end

local function await(start)
  local called, value, failure
  start(function(result, err)
    assert(not nvim.in_fast_event())
    called, value, failure = true, result, err
  end)
  eq(called, nil)
  assert(
    nvim.wait(7000, function()
      return called
    end, 10),
    "import callback did not arrive"
  )
  return value, failure
end

local function preview(selection)
  return await(function(callback)
    Import.preview(selection, callback)
  end)
end

local function apply(selection, store, declaration)
  return await(function(callback)
    Import.apply(store, selection, declaration, callback)
  end)
end

local function declaration(report)
  local bindings = {}
  for index, source in ipairs(report.selection.sources) do
    if source.kind == "acp" then
      bindings[tostring(index)] = nvim.deepcopy(source.proposal)
    end
  end
  return {
    digest = report.digest,
    bindings = bindings,
    confirmer = "human reviewer",
    confirmed_at = "2026-09-01T10:00:00Z",
    reference = "review-123",
  }
end

local function query(store, input)
  return await(function(callback)
    store:usage_query(input, callback)
  end)
end

local function append(store, record)
  local done, failure
  store:append(record, function(err)
    assert(not nvim.in_fast_event())
    done, failure = true, err
  end)
  assert(nvim.wait(7000, function()
    return done
  end, 10))
  return failure
end

local function sql(store, statement)
  local result = nvim.system({ "sqlite3", "-json", store.path, statement }, { text = true }):wait()
  assert(result.code == 0, result.stderr)
  return result.stdout == "" and {} or nvim.json.decode(result.stdout)
end

local T = MiniTest.new_set()

T["confirmed preview is atomic, idempotent and visible to shared readers"] = function()
  local selection, store = setup()
  local reviewed = assert(preview(selection))
  local confirmation = declaration(reviewed)
  local result, err = apply(selection, store, confirmation)
  eq(err, nil)
  eq(result.counts.admitted, 2)
  eq(result.counts.existing, 0)
  assert(result.batch_id:match("^sha256:"))
  local again = assert(apply(selection, store, confirmation))
  eq(again.batch_id, result.batch_id)
  eq(again.counts.admitted, 0)
  eq(again.counts.existing, 2)
  local page = assert(query(store, { view = "turns" }))
  eq(page.summary.turns, 2)
  eq(page.rows[1].attribution.source, "human")
  eq(page.rows[1].attribution.confirmer, confirmation.confirmer)
  eq(page.rows[1].attribution.reference, confirmation.reference)
  eq(page.rows[1].attribution.batch_id, result.batch_id)
  eq(assert(query(store, { filters = { attribution = "machine" } })).summary.turns, 0)
  local timeline = assert(query(store, { view = "events", filters = { attribution = "human" } }))
  local option_events = 0
  for _, event in ipairs(timeline.rows) do
    if event.kind == "options" then
      option_events = option_events + 1
      eq(event.data.attribution.source, "human")
      eq(event.data.attribution.batch_id, result.batch_id)
    end
  end
  eq(option_events, 2)
  eq(assert(query(store, { view = "events", filters = { attribution = "machine" } })).total, 0)
  local invalid, invalid_error = query(store, { filters = { attribution = "unconfirmed" } })
  eq(invalid, nil)
  eq(invalid_error.code, "invalid")
  local summaries = assert(await(function(callback)
    store:usage_summaries(
      { { agent = "sample-agent", provider = "sample-provider", options = page.rows[1].options } },
      callback
    )
  end))
  eq(summaries[1].turns, 1)
end

T["concurrent retries serialize to one batch"] = function()
  local selection, store = setup()
  local confirmation = declaration(assert(preview(selection)))
  local reports, failures = {}, {}
  for index = 1, 2 do
    Import.apply(store, selection, confirmation, function(report, err)
      assert(not nvim.in_fast_event())
      reports[index], failures[index] = report, err or false
    end)
  end
  assert(nvim.wait(7000, function()
    return failures[1] ~= nil and failures[2] ~= nil
  end, 10))
  eq(failures, { false, false })
  eq(reports[1].batch_id, reports[2].batch_id)
  eq(reports[1].counts.admitted + reports[2].counts.admitted, 2)
  eq(reports[1].counts.existing + reports[2].counts.existing, 2)
  eq(assert(query(store, {})).summary.turns, 2)
end

T["renaming evidence cannot duplicate an ambiguous dispatch"] = function()
  local selection, store = setup()
  assert(apply(selection, store, declaration(assert(preview(selection)))))
  local source = selection.sources[1]
  source.connection_id = "renamed-connection"
  source.connection_path = nvim.fs.dirname(source.connection_path) .. "/renamed-connection.jsonl"
  write_records(source.connection_path, fixture.connection)
  selection.sources = { source }
  local report = assert(apply(selection, store, declaration(assert(preview(selection)))))
  eq(report.counts.admitted, 0)
  eq(report.counts.excluded, 3)
  eq(report.exclusions[1].reason, "ambiguous_overlap")
  eq(assert(query(store, {})).summary.turns, 2)
end

T["reused request IDs remain distinct when reviewed connection boundaries prove it"] = function()
  local selection, store, records = setup()
  records[#fixture.first + 4].payload.id = 5
  records[#fixture.first + 6].payload.id = 5
  write_records(selection.sources[1].session_path, records)
  local resumed = nvim.deepcopy(selection.sources[2])
  selection.sources = { selection.sources[1] }
  eq(assert(apply(selection, store, declaration(assert(preview(selection))))).counts.admitted, 1)
  selection.sources = { resumed }
  eq(assert(apply(selection, store, declaration(assert(preview(selection))))).counts.admitted, 1)
  eq(assert(query(store, {})).summary.turns, 2)
end

T["a later declaration cannot assign a Session to a different Agent"] = function()
  local selection, store = setup()
  local resumed = nvim.deepcopy(selection.sources[2])
  selection.sources = { selection.sources[1] }
  assert(apply(selection, store, declaration(assert(preview(selection)))))
  resumed.proposal.agent = "different-agent"
  selection.sources = { resumed }
  local result = assert(apply(selection, store, declaration(assert(preview(selection)))))
  eq(result.counts.admitted, 0)
  eq(result.exclusions[1].reason, "conflicting_agent")
  eq(assert(query(store, {})).summary.turns, 1)
end

T["changed evidence and expanded selections never reach the store"] = function()
  local selection, store, records = setup()
  local confirmation = declaration(assert(preview(selection)))
  records[6].payload.params.prompt[1].text = "PRIVATE changed after review"
  write_records(selection.sources[1].session_path, records)
  local result, err = apply(selection, store, confirmation)
  eq(result, nil)
  eq(err.code, "changed")
  eq(nvim.uv.fs_stat(store.path), nil)
  local fresh = declaration(assert(preview(selection)))
  local mismatch = nvim.deepcopy(fresh)
  mismatch.bindings["1"].provider = "another-provider"
  result, err = apply(selection, store, mismatch)
  eq(result, nil)
  eq(err.code, "invalid")
  eq(nvim.uv.fs_stat(store.path), nil)
  selection.until_time = "2026-08-31T19:00:00Z"
  result, err = apply(selection, store, fresh)
  eq(result, nil)
  eq(err.code, "changed")
  eq(nvim.uv.fs_stat(store.path), nil)
  fresh.reference = ""
  result, err = apply(selection, store, fresh)
  eq(result, nil)
  eq(err.code, "invalid")
end

T["overlapping imports recognize existing evidence and refuse conflicting attribution"] = function()
  local selection, store = setup()
  local first = assert(apply(selection, store, declaration(assert(preview(selection)))))
  eq(first.counts.admitted, 2)
  selection.sources = { selection.sources[1] }
  local overlap = assert(apply(selection, store, declaration(assert(preview(selection)))))
  eq(overlap.counts.existing, 1)
  eq(overlap.counts.admitted, 0)
  selection.sources[1].proposal.provider = "different-provider"
  local conflict = assert(apply(selection, store, declaration(assert(preview(selection)))))
  eq(conflict.counts.excluded, 1)
  eq(conflict.exclusions[1].reason, "conflicting_import")
  local page = assert(query(store, { view = "turns" }))
  eq(page.summary.turns, 2)
  eq(page.rows[1].provider, "sample-provider")
  eq(page.rows[1].attribution.batch_id, first.batch_id)
end

T["live and pending turns conservatively exclude overlap without changing history"] = function()
  local selection, store = setup()
  eq(
    append(store, {
      id = "live-turn",
      agent = "sample-agent",
      provider = "live-provider",
      acp_session_id = fixture.session,
      prepared_at = "2026-08-31T17:01:03Z",
      options = { mode = "live" },
    }),
    nil
  )
  eq(
    append(store, {
      turn_id = "live-turn",
      sequence = 1,
      kind = "dispatch",
      observed_at = "2026-08-31T17:01:03Z",
      data = { request_id = 5, transcript_turn = 1 },
    }),
    nil
  )
  local report = assert(apply(selection, store, declaration(assert(preview(selection)))))
  eq(report.counts.admitted, 1)
  eq(report.counts.excluded, 3)
  eq(report.exclusions[1].reason, "live_overlap")
  local live = assert(query(store, { view = "turns", filters = { attribution = "machine" } }))
  eq(live.summary.turns, 1)
  eq(live.rows[1].id, "live-turn")
  eq(live.rows[1].provider, "live-provider")
  eq(live.rows[1].attribution.source, "machine")
  eq(assert(query(store, { filters = { attribution = "human" } })).summary.turns, 1)

  local other_selection, other_store = setup()
  eq(
    append(other_store, {
      id = "pending-live",
      agent = "sample-agent",
      provider = "live-provider",
      acp_session_id = fixture.session,
      prepared_at = "2026-08-31T17:01:03Z",
      options = {},
    }),
    nil
  )
  local pending = assert(apply(other_selection, other_store, declaration(assert(preview(other_selection)))))
  eq(pending.counts.admitted, 0)
  eq(pending.counts.excluded, 4)
  eq(pending.exclusions[1].reason, "live_overlap")
end

T["queued local live facts settle before import overlap is decided"] = function()
  local selection, store = setup()
  local confirmation = declaration(assert(preview(selection)))
  store:append({
    id = "queued-live",
    agent = "sample-agent",
    provider = "sample-provider",
    acp_session_id = fixture.session,
    prepared_at = "2026-08-31T17:01:03Z",
    options = {},
  })
  store:append({
    turn_id = "queued-live",
    sequence = 1,
    kind = "dispatch",
    observed_at = "2026-08-31T17:01:03Z",
    data = { request_id = 5 },
  })
  local report = assert(apply(selection, store, confirmation))
  eq(report.counts.admitted, 1)
  eq(report.exclusions[1].reason, "live_overlap")
  eq(assert(query(store, {})).summary.turns, 2)
end

T["evidence is checked again after waiting for queued recording"] = function()
  local selection, store, records = setup()
  eq(
    append(store, {
      id = "before",
      agent = "other",
      provider = "other",
      acp_session_id = "other",
      prepared_at = "2026-08-31T16:00:00Z",
      options = {},
    }),
    nil
  )
  local held = false
  local locker = nvim.system({ "sqlite3", store.path }, {
    stdin = true,
    stdout = function(_, data)
      if data and data:find("held", 1, true) then
        held = true
      end
    end,
  })
  MiniTest.finally(function()
    locker:kill(9)
    locker:wait()
  end)
  locker:write("BEGIN IMMEDIATE; SELECT 'held';\n")
  assert(nvim.wait(3000, function()
    return held
  end, 10))
  local confirmation = declaration(assert(preview(selection)))
  store:append({
    id = "queued",
    agent = "other",
    provider = "other",
    acp_session_id = "other",
    prepared_at = "2026-08-31T16:00:01Z",
    options = {},
  })
  local delayed = nvim.defer_fn(function()
    records[6].payload.params.prompt[1].text = "PRIVATE changed while waiting"
    write_records(selection.sources[1].session_path, records)
    locker:write("COMMIT;\n.quit\n")
  end, 150)
  MiniTest.finally(function()
    if not delayed:is_closing() then
      delayed:stop()
      delayed:close()
    end
  end)
  local report, err = apply(selection, store, confirmation)
  eq(report, nil)
  eq(err.code, "changed")
  eq(sql(store, "SELECT count(*) AS n FROM import_turns")[1].n, 0)
end

T["an option change in an unselected turn is not misrecorded between turns"] = function()
  local selection, store, records = setup()
  local changed = nvim.deepcopy(records[5].payload.result.configOptions)
  changed[#changed + 1] = { id = "enabled", name = "Enabled", type = "boolean", currentValue = false }
  table.insert(records, 7, {
    timestamp = "2026-08-31T17:01:04.000Z",
    direction = "agent_to_client",
    kind = "frame",
    session_id = fixture.session,
    payload = {
      jsonrpc = "2.0",
      method = "session/update",
      params = {
        sessionId = fixture.session,
        update = { sessionUpdate = "config_option_update", configOptions = changed },
      },
    },
  })
  table.insert(records, 11, {
    timestamp = "2026-08-31T17:01:10.000Z",
    direction = "client_to_agent",
    kind = "frame",
    session_id = fixture.session,
    payload = {
      jsonrpc = "2.0",
      id = 6,
      method = "session/prompt",
      params = {
        sessionId = fixture.session,
        prompt = { { text = "PRIVATE" } },
      },
    },
  })
  table.insert(records, 12, {
    timestamp = "2026-08-31T17:01:11.000Z",
    direction = "agent_to_client",
    kind = "frame",
    session_id = fixture.session,
    payload = { jsonrpc = "2.0", id = 6, result = { stopReason = "end_turn" } },
  })
  selection.sources[1].last_line = #fixture.first + 3
  selection.sources = { selection.sources[1] }
  selection.from = "2026-08-31T17:01:04Z"
  write_records(selection.sources[1].session_path, records)
  local reviewed = assert(preview(selection))
  eq(reviewed.transitions[1].during_unselected_turn, true)
  local first = assert(apply(selection, store, declaration(reviewed)))
  eq(first.counts.admitted, 1)
  eq(first.exclusions[1].reason, "ambiguous_option_transition")
  selection.from = "2026-08-31T17:00:00Z"
  local expanded = assert(apply(selection, store, declaration(assert(preview(selection)))))
  eq(expanded.counts.admitted, 1)
  eq(expanded.counts.existing, 1)
  local turns = assert(query(store, { view = "turns" }))
  eq(turns.rows[1].mixed, true)
  local events = assert(query(store, { view = "events", turn_id = turns.rows[1].id }))
  local changed_during_turn = false
  for _, event in ipairs(events.rows) do
    changed_during_turn = changed_during_turn or event.kind == "options"
  end
  eq(changed_during_turn, true)
end

T["sparse telemetry and typed mixed turns retain their coverage"] = function()
  local selection, store, records = setup()
  records[9].payload.result.usage = nil
  table.remove(records, #fixture.first + 6)
  local changed = nvim.deepcopy(records[5].payload.result.configOptions)
  changed[#changed + 1] = { id = "enabled", name = "Enabled", type = "boolean", currentValue = false }
  table.insert(records, 7, {
    timestamp = "2026-08-31T17:01:04.000Z",
    direction = "agent_to_client",
    kind = "frame",
    session_id = fixture.session,
    payload = {
      jsonrpc = "2.0",
      method = "session/update",
      params = {
        sessionId = fixture.session,
        update = { sessionUpdate = "config_option_update", configOptions = changed },
      },
    },
  })
  selection.sources[1].last_line = #fixture.first + 1
  selection.sources[2].first_line = #fixture.first + 2
  selection.sources[2].last_line = #records
  write_records(selection.sources[1].session_path, records)
  local report = assert(apply(selection, store, declaration(assert(preview(selection)))))
  eq(report.counts.admitted, 2)
  local all = assert(query(store, { view = "turns" }))
  eq(all.summary.turns, 2)
  eq(all.summary.outcomes, { completed = 1, unobserved = 1 })
  eq(all.summary.tokens, {})
  eq(all.rows[1].mixed, true)
  local fixed = assert(query(store, { filters = { provider = "sample-provider" } }))
  eq(fixed.summary.turns, 1)
  eq(fixed.excluded_mixed, 1)
  local events = assert(query(store, { view = "events", turn_id = all.rows[1].id }))
  local found
  for _, event in ipairs(events.rows) do
    if event.kind == "options" then
      found = event.data.options.enabled
    end
  end
  eq(found, false)
  local summaries = assert(await(function(callback)
    store:usage_summaries(
      { { agent = "sample-agent", provider = "sample-provider", options = all.rows[1].options } },
      callback
    )
  end))
  eq(summaries[1].turns, 0)
end

T["currency changes and malformed cost coverage never become a complete delta"] = function()
  local selection, store, records = setup()
  local function cost(at, amount, currency)
    return {
      timestamp = at,
      direction = "agent_to_client",
      kind = "frame",
      session_id = fixture.session,
      payload = {
        jsonrpc = "2.0",
        method = "session/update",
        params = {
          sessionId = fixture.session,
          update = {
            sessionUpdate = "usage_update",
            used = 1,
            size = 100,
            cost = { amount = amount, currency = currency },
          },
        },
      },
    }
  end
  table.insert(records, 6, cost("2026-08-31T17:01:02.000Z", 1, "USD"))
  table.insert(records, 8, cost("2026-08-31T17:01:04.000Z", 2, "EUR"))
  selection.sources[1].last_line = #fixture.first + 2
  selection.sources[2].first_line = #fixture.first + 3
  selection.sources[2].last_line = #records
  write_records(selection.sources[1].session_path, records)
  assert(apply(selection, store, declaration(assert(preview(selection)))))
  eq(assert(query(store, {})).summary.costs, {})
end

T["a telemetry gap suppresses an otherwise monotonic cost delta"] = function()
  local selection, store, records = setup()
  table.insert(records, 6, {
    timestamp = "2026-08-31T17:01:02.000Z",
    direction = "agent_to_client",
    kind = "frame",
    session_id = fixture.session,
    payload = {
      jsonrpc = "2.0",
      method = "session/update",
      params = {
        sessionId = fixture.session,
        update = { sessionUpdate = "usage_update", used = 1, size = 100, cost = { amount = 0.001, currency = "USD" } },
      },
    },
  })
  records[8].payload.params.update.cost = { amount = 987654321, currency = "USD" }
  selection.sources[1].last_line = #fixture.first + 1
  selection.sources[2].first_line = #fixture.first + 2
  selection.sources[2].last_line = #records
  write_records(selection.sources[1].session_path, records)
  local lines = nvim.fn.readfile(selection.sources[1].session_path)
  lines[8] = lines[8]:gsub("987654321", "NaN")
  assert(nvim.fn.writefile(lines, selection.sources[1].session_path) == 0)
  local reviewed = assert(preview(selection))
  eq(reviewed.candidates[1].telemetry_gap, true)
  assert(apply(selection, store, declaration(reviewed)))
  eq(assert(query(store, {})).summary.costs, {})
end

T["SQL failure rolls back the batch and a fresh retry succeeds"] = function()
  local selection, store = setup()
  eq(
    append(store, {
      id = "existing",
      agent = "other",
      provider = "other",
      acp_session_id = "other",
      prepared_at = "2026-08-31T16:00:00Z",
      options = {},
    }),
    nil
  )
  sql(
    store,
    [[CREATE TRIGGER reject_second_import BEFORE INSERT ON turns
    WHEN NEW.id LIKE 'import:%' AND EXISTS(SELECT 1 FROM turns WHERE id LIKE 'import:%')
    BEGIN SELECT RAISE(ABORT, 'synthetic failure'); END;]]
  )
  local reviewed = assert(preview(selection))
  local result, err = apply(selection, store, declaration(reviewed))
  eq(result, nil)
  eq(err.code, "storage")
  eq(sql(store, "SELECT count(*) AS n FROM import_turns")[1].n, 0)
  eq(sql(store, "SELECT count(*) AS n FROM import_batches")[1].n, 0)
  eq(sql(store, "SELECT count(*) AS n FROM turns")[1].n, 1)
  sql(store, "DROP TRIGGER reject_second_import")
  result = assert(apply(selection, store, declaration(assert(preview(selection)))))
  eq(result.counts.admitted, 2)
end

T["broken import provenance fails closed instead of appearing machine observed"] = function()
  local selection, store = setup()
  local reviewed = assert(preview(selection))
  local confirmation = declaration(reviewed)
  assert(apply(selection, store, confirmation))
  sql(store, "DELETE FROM import_batches")
  local page, err = query(store, { filters = { attribution = "machine" } })
  eq(page, nil)
  eq(err.code, "corrupt")
  local report, apply_error = apply(selection, store, confirmation)
  eq(report, nil)
  eq(apply_error.code, "corrupt")
end

T["live recorder cannot manufacture a reserved import identity"] = function()
  local _, store = setup()
  local err = append(store, {
    id = "import:sha256:forged",
    agent = "agent",
    provider = "service",
    acp_session_id = "session",
    prepared_at = "2026-08-31T16:00:00Z",
    options = {},
  })
  eq(err.code, "invalid")
  eq(nvim.uv.fs_stat(store.path), nil)
end

T["live recorder cannot append new observations to an imported turn"] = function()
  local selection, store = setup()
  assert(apply(selection, store, declaration(assert(preview(selection)))))
  local page = assert(query(store, { view = "turns" }))
  local err = append(store, {
    turn_id = page.rows[1].id,
    sequence = 50,
    kind = "cost",
    observed_at = "2026-08-31T17:01:09Z",
    data = { cost = { amount = 999, currency = "USD" } },
  })
  eq(err.code, "invalid")
  eq(sql(store, "SELECT count(*) AS n FROM turn_events WHERE sequence=50")[1].n, 0)
end

T["apply freezes caller data and cancellation before writing suppresses delivery"] = function()
  local selection, store = setup()
  local confirmation = declaration(assert(preview(selection)))
  local called = false
  local cancel = Import.apply(store, selection, confirmation, function()
    called = true
  end)
  cancel()
  nvim.wait(100, function()
    return called
  end, 5)
  eq(called, false)
  eq(nvim.uv.fs_stat(store.path), nil)
  local done, result, failure
  Import.apply(store, selection, confirmation, function(report, err)
    done, result, failure = true, report, err
  end)
  selection.sources[1].proposal.provider = "changed after invocation"
  confirmation.confirmer = "changed after invocation"
  assert(nvim.wait(7000, function()
    return done
  end, 10))
  eq(failure, nil)
  eq(result.counts.admitted, 2)
  local rows = assert(query(store, { view = "turns" })).rows
  eq(rows[1].provider, "sample-provider")
  eq(rows[1].attribution.confirmer, "human reviewer")
end

T["explicit apply command consumes reviewed JSON and a chosen private store"] = function()
  local selection, store = setup()
  local confirmation = declaration(assert(preview(selection)))
  local process = nvim
    .system({
      nvim.fn.getcwd() .. "/scripts/apply-acp-usage",
      store.directory,
      nvim.json.encode(selection),
      nvim.json.encode(confirmation),
    }, { text = true })
    :wait(10000)
  eq(process.code, 0)
  local report = nvim.json.decode(process.stdout)
  eq(report.counts.admitted, 2)
  eq(assert(query(store, {})).summary.turns, 2)
end

T["a failed transaction requires fresh evidence on retry and never stores payloads"] = function()
  local selection, store, records = setup()
  eq(
    append(store, {
      id = "unrelated",
      agent = "other",
      provider = "other",
      acp_session_id = "other",
      prepared_at = "2026-08-31T16:00:00Z",
      options = {},
    }),
    nil
  )
  local held = false
  local locker = nvim.system({ "sqlite3", store.path }, {
    stdin = true,
    stdout = function(_, data)
      if data and data:find("held", 1, true) then
        held = true
      end
    end,
  })
  MiniTest.finally(function()
    locker:kill(9)
    locker:wait()
  end)
  locker:write("BEGIN IMMEDIATE; SELECT 'held';\n")
  assert(
    nvim.wait(3000, function()
      return held
    end, 10),
    "SQLite lock not acquired"
  )
  local confirmation = declaration(assert(preview(selection)))
  local result, err = apply(selection, store, confirmation)
  eq(result, nil)
  eq(err.code, "locked")
  locker:write("COMMIT;\n.quit\n")
  eq(locker:wait(3000).code, 0)
  eq(sql(store, "SELECT count(*) AS n FROM import_turns")[1].n, 0)
  records[6].payload.params.prompt[1].text = "PRIVATE changed before retry"
  write_records(selection.sources[1].session_path, records)
  result, err = apply(selection, store, confirmation)
  eq(result, nil)
  eq(err.code, "changed")
  local retried = assert(apply(selection, store, declaration(assert(preview(selection)))))
  eq(retried.counts.admitted, 2)
  local contents = nvim.json.encode(sql(
    store,
    [[
    SELECT (SELECT json_group_array(json_object('selection',selection,'sources',sources,'declaration',declaration)) FROM import_batches) AS batches,
      (SELECT json_group_array(evidence) FROM import_turns) AS evidence,
      (SELECT json_group_array(data) FROM turn_events) AS events;
  ]]
  ))
  eq(contents:find("PRIVATE", 1, true), nil)
  local dump = nvim.system({ "sqlite3", store.path, ".dump" }, { text = true }):wait()
  eq(dump.code, 0)
  eq(dump.stdout:find("PRIVATE", 1, true), nil)
end

T["the importer upgrades older stores and keeps the older writer guard closed"] = function()
  local selection, store = setup()
  eq(
    append(store, {
      id = "older",
      agent = "old",
      provider = "old",
      acp_session_id = "other",
      prepared_at = "2026-08-31T16:00:00Z",
      options = {},
    }),
    nil
  )
  sql(store, "DROP TABLE import_options; DROP TABLE import_turns; DROP TABLE import_batches; PRAGMA user_version=5;")
  eq(assert(query(store, {})).summary.turns, 0)
  eq(sql(store, "PRAGMA user_version")[1].user_version, 5)
  assert(apply(selection, store, declaration(assert(preview(selection)))))
  eq(sql(store, "PRAGMA user_version")[1].user_version, 6)
  local older_guard = nvim
    .system({
      "sqlite3",
      store.path,
      [[
    CREATE TEMP TABLE old_guard(ok INTEGER NOT NULL CHECK(ok=1));
    INSERT INTO old_guard SELECT user_version IN (0,1,2,3,4,5) FROM pragma_user_version;
  ]],
    }, { text = true })
    :wait()
  assert(older_guard.code ~= 0 and older_guard.stderr:find("CHECK constraint", 1, true))
end

return T
