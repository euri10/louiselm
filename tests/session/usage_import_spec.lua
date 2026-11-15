local MiniTest = require("mini.test")
local Import = require("louiselm.session.usage_import")

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

local function selection()
  local directory = nvim.fn.tempname()
  assert(nvim.fn.mkdir(directory, "p", 448) == 1)
  MiniTest.finally(function()
    nvim.fn.delete(directory, "rf")
  end)
  local session_path = directory .. "/session.jsonl"
  local connection_path = directory .. "/first-connection.jsonl"
  local resumed_path = directory .. "/resumed-connection.jsonl"
  local all = nvim.deepcopy(fixture.first)
  for _, record in ipairs(fixture.second) do
    all[#all + 1] = nvim.deepcopy(record)
  end
  write_records(session_path, all)
  write_records(connection_path, fixture.connection)
  write_records(resumed_path, fixture.resumed_connection)
  local proposal = { agent = "sample-agent", provider = "sample-provider" }
  return {
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
        last_line = #all,
        proposal = nvim.deepcopy(proposal),
      },
    },
  },
    all
end

local function preview(input)
  local called, result, failure, in_fast
  local cancel = Import.preview(input, function(report, err)
    called, result, failure, in_fast = true, report, err, nvim.in_fast_event()
  end)
  eq(called, nil)
  eq(type(cancel), "function")
  assert(
    nvim.wait(5000, function()
      return called
    end, 10),
    "preview callback did not arrive"
  )
  eq(in_fast, false)
  return result, failure
end

local T = MiniTest.new_set()

T["captured order keeps resumed request IDs and typed options separate"] = function()
  local input = selection()
  local report, err = preview(input)
  eq(err, nil)
  eq(report.counts.candidates, 2)
  eq(report.attribution_confirmation, "unconfirmed")
  eq(report.sources[1].proposal.confirmation, "unconfirmed")
  eq(#report.batches, 2)
  eq(report.batches[1].source, 1)
  eq(report.batches[1].candidates, 1)
  eq(report.batches[1].proposal.confirmation, "unconfirmed")
  eq(report.counts.exclusions, 0)
  eq(#report.transitions, 2)
  eq(report.transitions[1].source, "response")
  eq(report.transitions[1].request, { id = 3, option = "mode", value = "bypassPermissions" })
  eq(report.transitions[2].previous_options.effort, "default")
  eq(report.transitions[2].options.effort, nil)
  local first, second = report.candidates[1], report.candidates[2]
  eq(first.connection_id, "first-connection")
  eq(first.request_id, 5)
  eq(first.options, { mode = "bypassPermissions", model = "haiku" })
  eq(first.usage, { input_tokens = 10, output_tokens = 3, total_tokens = 13 })
  eq(first.cost_baseline, nil)
  eq(first.costs[1].cost, { amount = 0.01, currency = "USD" })
  eq(first.outcome, "completed")
  eq(first.evidence.binding, { source = 1, file = "connection", line = 2 })
  eq(first.evidence.options, { source = 1, file = "session", line = 5 })
  eq(report.transitions[1].request_evidence, { source = 1, file = "session", line = 2 })
  eq(second.connection_id, "resumed-connection")
  eq(second.request_id, 3)
  eq(second.options, { mode = "default", model = "haiku" })
  eq(second.usage, { input_tokens = 11, output_tokens = 4, total_tokens = 15 })
  eq(second.proposal, { agent = "sample-agent", provider = "sample-provider", confirmation = "unconfirmed" })
  eq(type(first.evidence.dispatch.line), "number")
  eq(report.digest:match("^sha256:[0-9a-f]+$") ~= nil, true)
  local serialized = nvim.json.encode(report)
  eq(serialized:find("PRIVATE", 1, true), nil)
  local again = assert(preview(input))
  eq(again.digest, report.digest)
end

T["missing completion remains absent and invalid starting options exclude a turn"] = function()
  local input, records = selection()
  records[5].payload.result.configOptions = nil
  table.remove(records, #fixture.first + 6)
  write_records(input.sources[1].session_path, records)
  input.sources[2].last_line = #records
  local report = assert(preview(input))
  eq(#report.candidates, 1)
  eq(report.candidates[1].connection_id, "resumed-connection")
  eq(report.candidates[1].outcome, nil)
  eq(report.candidates[1].usage, nil)
  eq(report.exclusions[1].reason, "invalid_options")
end

T["a completed response can have no usage telemetry"] = function()
  local input, records = selection()
  records[9].payload.result.usage = nil
  write_records(input.sources[1].session_path, records)
  local report = assert(preview(input))
  eq(report.candidates[1].outcome, "completed")
  eq(report.candidates[1].usage, nil)
  eq(report.candidates[1].peer_response, true)
end

T["missing and unsupported sources are visible without reading other histories"] = function()
  local input = selection()
  input.sources[1].connection_path = nvim.fs.dirname(input.sources[1].connection_path)
    .. "/missing/first-connection.jsonl"
  input.sources[3] = { kind = "native", path = "/not-read/native.jsonl" }
  local report = assert(preview(input))
  eq(report.sources[1].status, "missing")
  eq(report.sources[3].status, "unsupported")
  eq(#report.candidates, 1)
end

T["file byte limits and mismatched bindings produce visible exclusions"] = function()
  local input = selection()
  input.max_bytes = 64
  local limited = assert(preview(input))
  eq(limited.sources[1].status, "byte_limit")
  eq(#limited.candidates, 0)

  input.max_bytes = 100000
  local connection = nvim.deepcopy(fixture.connection)
  connection[2].payload.sessionId = "another-session"
  write_records(input.sources[1].connection_path, connection)
  local mismatched = assert(preview(input))
  eq(mismatched.sources[1].status, "excluded")
  eq(mismatched.exclusions[1].reason, "invalid_binding")
  eq(#mismatched.candidates, 1)
end

T["non-finite telemetry is excluded without contaminating the report"] = function()
  local input, records = selection()
  records[7].payload.params.update.cost = { amount = 987654321, currency = "USD" }
  write_records(input.sources[1].session_path, records)
  local raw = nvim.fn.readfile(input.sources[1].session_path)
  raw[7] = raw[7]:gsub("987654321", "NaN")
  assert(nvim.fn.writefile(raw, input.sources[1].session_path) == 0)
  local report = assert(preview(input))
  eq(report.exclusions[1].reason, "invalid_telemetry")
  eq(report.candidates[1].telemetry_gap, true)
  eq(#report.candidates[1].costs, 1)
  eq(pcall(nvim.json.encode, report), true)
end

T["an oversized JSONL record is refused with a bounded reason"] = function()
  local input, records = selection()
  records[6].payload.params.prompt[1].text = string.rep("p", 262145)
  write_records(input.sources[1].session_path, records)
  input.max_bytes = 1000000
  local report = assert(preview(input))
  eq(report.sources[1].status, "line_bytes")
  eq(report.exclusions[1].reason, "line_bytes")
  eq(#report.candidates, 0)
end

T["cost resets, clearings and option changes are retained without estimating a price"] = function()
  local input, records = selection()
  local function update(at, value)
    return {
      timestamp = at,
      direction = "agent_to_client",
      kind = "frame",
      session_id = fixture.session,
      payload = { jsonrpc = "2.0", method = "session/update", params = { sessionId = fixture.session, update = value } },
    }
  end
  local changed = nvim.deepcopy(records[5].payload.result.configOptions)
  changed[#changed + 1] = { id = "enabled", name = "Enabled", type = "boolean", currentValue = false }
  table.insert(
    records,
    6,
    update("2026-08-31T17:01:02.000Z", {
      sessionUpdate = "usage_update",
      used = 1,
      size = 100,
      cost = { amount = 1, currency = "USD" },
    })
  )
  table.insert(
    records,
    8,
    update("2026-08-31T17:01:03.900Z", { sessionUpdate = "config_option_update", configOptions = changed })
  )
  table.insert(
    records,
    10,
    update("2026-08-31T17:01:04.542Z", {
      sessionUpdate = "usage_update",
      used = 10,
      size = 100,
      cost = nvim.NIL,
    })
  )
  table.insert(
    records,
    11,
    update("2026-08-31T17:01:04.543Z", {
      sessionUpdate = "usage_update",
      used = 10,
      size = 100,
      cost = { amount = 0.5, currency = "USD" },
    })
  )
  table.insert(
    records,
    12,
    update("2026-08-31T17:01:04.544Z", {
      sessionUpdate = "usage_update",
      used = 10,
      size = 100,
      cost = { amount = 0.7, currency = "EUR" },
    })
  )
  input.sources[1].last_line = #fixture.first + 5
  input.sources[2].first_line = input.sources[1].last_line + 1
  input.sources[2].last_line = #records
  write_records(input.sources[1].session_path, records)
  local report = assert(preview(input))
  local turn = report.candidates[1]
  eq(turn.cost_baseline, { amount = 1, currency = "USD" })
  eq(turn.costs[1].cost, nvim.NIL)
  eq(turn.costs[2].cost, { amount = 0.5, currency = "USD" })
  eq(turn.costs[3].cost, { amount = 0.7, currency = "EUR" })
  eq(turn.mixed, true)
  eq(report.transitions[3].source, "notification")
  eq(report.transitions[3].request, nil)
  eq(report.transitions[3].options.enabled, false)
end

T["selection end does not invent a later completion and file changes change its digest"] = function()
  local input = selection()
  input.until_time = "2026-08-31T17:01:08.200Z"
  local report = assert(preview(input))
  eq(#report.candidates, 1)
  eq(report.candidates[1].outcome, nil)
  eq(report.candidates[1].usage, nil)
  eq(#report.candidates[1].costs, 0)
  assert(nvim.fn.writefile({ "{}" }, input.sources[1].session_path, "a") == 0)
  local changed = assert(preview(input))
  eq(changed.digest ~= report.digest, true)
end

T["malformed stream ordering excludes its source rather than borrowing another request"] = function()
  local input, records = selection()
  records[4].timestamp = "2026-08-31T17:00:50.000Z"
  write_records(input.sources[1].session_path, records)
  local report = assert(preview(input))
  eq(report.sources[1].status, "excluded")
  eq(report.exclusions[1].reason, "unordered_session")
  eq(#report.candidates, 1)
  eq(report.candidates[1].connection_id, "resumed-connection")
end

T["a duplicate in-flight request excludes the ambiguous connection segment"] = function()
  local input, records = selection()
  local duplicate = nvim.deepcopy(records[2])
  duplicate.timestamp = "2026-08-31T17:00:56.877Z"
  table.insert(records, 3, duplicate)
  input.sources[1].last_line = input.sources[1].last_line + 1
  input.sources[2].first_line = input.sources[2].first_line + 1
  input.sources[2].last_line = #records
  write_records(input.sources[1].session_path, records)
  local report = assert(preview(input))
  eq(report.sources[1].status, "excluded")
  eq(report.exclusions[1].reason, "duplicate_request")
  eq(#report.candidates, 1)
end

T["resumed streams require an observed exit between selected connection segments"] = function()
  local input, records = selection()
  records[#fixture.first].kind = "trace-event"
  write_records(input.sources[1].session_path, records)
  local report = assert(preview(input))
  eq(report.sources[1].status, "excluded")
  eq(report.sources[2].status, "excluded")
  eq(report.exclusions[1].reason, "unverified_connection_boundary")
  eq(#report.candidates, 0)
end

T["a resumed segment cannot hide its missing preceding connection boundary"] = function()
  local input, records = selection()
  records[#fixture.first].kind = "trace-event"
  write_records(input.sources[1].session_path, records)
  input.sources = { input.sources[2] }
  local report = assert(preview(input))
  eq(report.sources[1].status, "excluded")
  eq(report.exclusions[1].reason, "unverified_connection_boundary")
  eq(#report.candidates, 0)
end

T["invalid selections fail asynchronously and cancellation suppresses delivery"] = function()
  local input = selection()
  input.max_bytes = 8388609
  local result, err = preview(input)
  eq(result, nil)
  eq(err.code, "invalid")
  input.max_bytes = 100000
  local called = false
  local cancel = Import.preview(input, function()
    called = true
  end)
  cancel()
  nvim.wait(100, function()
    return called
  end, 10)
  eq(called, false)
end

T["the selection is frozen before asynchronous reads"] = function()
  local input = selection()
  local done, report
  Import.preview(input, function(value)
    report, done = value, true
  end)
  input.sources[1].proposal.agent = "later-mutation"
  assert(
    nvim.wait(5000, function()
      return done
    end, 10),
    "preview callback did not arrive"
  )
  eq(report.candidates[1].proposal.agent, "sample-agent")
end

T["connection identities come from their explicitly selected filenames"] = function()
  local input = selection()
  input.sources[2].connection_path = input.sources[1].connection_path
  local report, err = preview(input)
  eq(report, nil)
  eq(err.code, "invalid")
end

T["a full load notification can establish options before a sparse load response"] = function()
  local input, records = selection()
  local loaded = nvim.deepcopy(records[#fixture.first + 3].payload.result.configOptions)
  records[#fixture.first + 2].payload.params.update = { sessionUpdate = "config_option_update", configOptions = loaded }
  records[#fixture.first + 3].payload.result.configOptions = nil
  write_records(input.sources[1].session_path, records)
  local report = assert(preview(input))
  eq(report.counts.candidates, 2)
  eq(report.candidates[2].options, { mode = "default", model = "haiku" })
end

T["explicit command emits the same sanitized preview"] = function()
  local input = selection()
  local expected = assert(preview(input))
  local done, result
  nvim.system(
    { nvim.fn.getcwd() .. "/scripts/preview-acp-usage", nvim.json.encode(input) },
    { text = true },
    function(value)
      nvim.schedule(function()
        result, done = value, true
      end)
    end
  )
  assert(
    nvim.wait(5000, function()
      return done
    end, 10),
    "preview command did not exit"
  )
  eq(result.code, 0)
  local report = nvim.json.decode(result.stdout)
  eq(report.digest, expected.digest)
  eq(report.counts.candidates, 2)
  eq(result.stdout:find("PRIVATE", 1, true), nil)
end

return T
