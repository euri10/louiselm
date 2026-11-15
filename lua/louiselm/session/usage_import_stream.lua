---@diagnostic disable-next-line: undefined-global -- Neovim supplies JSON and scheduling.
local nvim = vim
local Protocol = require("louiselm.acp.protocol")
local Validation = require("louiselm.session.validation")

local M = {}
local MAX_REASONS = 256
local MAX_LINES = 50000
local MAX_LINE_BYTES = 256 * 1024
local MAX_CANDIDATES, MAX_TRANSITIONS, MAX_COSTS = 1000, 1000, 128

local function lines(bytes)
  if bytes == "" then
    return nil, "empty_file"
  end
  if bytes:sub(-1) ~= "\n" then
    return nil, "incomplete_file"
  end
  local result = {}
  for line in bytes:gmatch("([^\n]*)\n") do
    if #line > MAX_LINE_BYTES then
      return nil, "line_bytes"
    end
    result[#result + 1] = line
    if #result > MAX_LINES then
      return nil, "line_limit"
    end
  end
  return result
end

local function decode(line)
  local ok, value = pcall(nvim.json.decode, line)
  if not ok or type(value) ~= "table" or not Validation.timestamp_key(value.timestamp) then
    return nil
  end
  return value
end

local function id_key(id)
  if type(id) == "string" and #id > 0 and #id <= 256 and not id:find("[%z\1-\31\127]") then
    return "s:" .. id
  end
  if type(id) == "number" and id % 1 == 0 then
    return "n:" .. tostring(id)
  end
end

local function finite(value)
  return value == value and value ~= math.huge and value ~= -math.huge
end

local function bounded_string(value, limit)
  return type(value) == "string" and #value > 0 and #value <= limit and not value:find("[%z\1-\31\127]")
end

local function options(value)
  if type(value) ~= "table" or not nvim.islist(value) or #value > 64 then
    return nil
  end
  for _, item in ipairs(value) do
    if type(item) ~= "table" or (item.type ~= "select" and item.type ~= "boolean") then
      return nil
    end
  end
  local parsed = Validation.config_options(value)
  if not parsed then
    return nil
  end
  local result = nvim.empty_dict()
  local model, models
  models = 0
  for _, item in ipairs(parsed) do
    if
      not bounded_string(item.id, 128)
      or type(item.current_value) == "string" and not bounded_string(item.current_value, 256)
    then
      return nil
    end
    result[item.id] = item.current_value
    if item.category == "model" then
      models, model = models + 1, item.current_value
    end
  end
  if models > 1 then
    return nil
  end
  return result, model
end

local function evidence(index, file, line)
  return { source = index, file = file, line = line }
end

local function source_file(file, path)
  return { path = path, status = file.status, sha256 = file.sha256, bytes = file.bytes and #file.bytes or nil }
end

local function add_exclusion(target, index, reason, line, file)
  target.exclusion_count = target.exclusion_count + 1
  if #target.exclusions < MAX_REASONS then
    target.exclusions[#target.exclusions + 1] = {
      source = index,
      reason = reason,
      evidence = line and evidence(index, file or "session", line) or nil,
    }
  end
end

local function in_range(timestamp, from, until_time)
  local key = Validation.timestamp_key(timestamp)
  return key and key >= from and key < until_time
end

local function prepare_connection(records, source, finished, cancelled)
  local cursor, last, binding, startup = 1, nil, nil, nil
  local function next_batch()
    if cancelled() then
      return
    end
    local stop = math.min(cursor + 99, #records)
    while cursor <= stop do
      local line = cursor
      local entry = decode(records[cursor])
      cursor = cursor + 1
      if not entry then
        finished(nil, "malformed_connection", line)
        return
      end
      local at = Validation.timestamp_key(entry.timestamp)
      if last and at < last then
        finished(nil, "unordered_connection", line)
        return
      end
      last = at
      if entry.kind == "session-bound" then
        if
          binding
          or entry.direction ~= "internal"
          or entry.session_id ~= source.session_id
          or type(entry.payload) ~= "table"
          or entry.payload.sessionId ~= source.session_id
        then
          finished(nil, "invalid_binding", line)
          return
        end
        binding = { at = at, line = line }
      elseif
        entry.kind == "frame"
        and entry.direction == "client_to_agent"
        and type(entry.payload) == "table"
        and entry.payload.method == "session/new"
      then
        if binding or startup or not Protocol.validate(entry.payload) or not id_key(entry.payload.id) then
          finished(nil, "invalid_start", line)
          return
        end
        startup = { id = entry.payload.id, method = "session/new", line = line }
      end
    end
    if cursor <= #records then
      nvim.schedule(next_batch)
    elseif not binding then
      finished(nil, "missing_binding")
    else
      finished({ binding = binding, startup = startup })
    end
  end
  next_batch()
end

local function process_session(records, source, index, connection, selection, limits, finished, cancelled)
  if source.last_line > #records then
    finished(nil, "line_range")
    return
  end
  if source.first_line > 1 then
    local previous = decode(records[source.first_line - 1])
    if
      not previous
      or previous.kind ~= "exit"
      or previous.direction ~= "internal"
      or previous.session_id ~= source.session_id
      or connection.binding.at <= Validation.timestamp_key(previous.timestamp)
    then
      finished(nil, "unverified_connection_boundary", source.first_line - 1)
      return
    end
  end
  local from = Validation.timestamp_key(selection.from)
  local until_time = Validation.timestamp_key(selection.until_time)
  local state = {
    cursor = source.first_line,
    last = nil,
    started = false,
    ready = false,
    ended = false,
    pending = {},
    options = nil,
    option_evidence = nil,
    model = nil,
    cost = nil,
    cost_evidence = nil,
    active = nil,
    candidates = {},
    transitions = {},
    exclusions = {},
    exclusion_count = 0,
  }
  if connection.startup then
    state.pending[id_key(connection.startup.id)] = connection.startup
  end

  local function exclude(reason, line)
    add_exclusion(state, index, reason, line)
  end

  local function accept_options(raw, entry, line, request, request_evidence)
    local next_options, model = options(raw)
    if not next_options then
      if state.active and not state.active.outside_selection then
        state.active.mixed, state.active.options_uncertain = true, true
      end
      state.options, state.model, state.option_evidence = nil, nil, nil
      exclude("invalid_options", line)
      return
    end
    if state.ready and state.options and not nvim.deep_equal(state.options, next_options) then
      if in_range(entry.timestamp, from, until_time) and #state.transitions < limits.transitions then
        state.transitions[#state.transitions + 1] = {
          session_id = source.session_id,
          connection_id = source.connection_id,
          observed_at = entry.timestamp,
          previous_options = state.options,
          options = next_options,
          source = request and "response" or "notification",
          request = request,
          request_evidence = request_evidence,
          evidence = evidence(index, "session", line),
          during_unselected_turn = state.active and state.active.outside_selection or nil,
        }
      elseif in_range(entry.timestamp, from, until_time) and not state.transition_limit then
        state.transition_limit = true
        exclude("transition_limit", line)
      end
      if state.active then
        state.active.mixed = true
      end
    end
    state.options, state.model, state.option_evidence = next_options, model, evidence(index, "session", line)
  end

  local function consume(entry, line)
    if entry.session_id ~= source.session_id then
      return "session_mismatch"
    end
    local at = Validation.timestamp_key(entry.timestamp)
    if state.last and at < state.last then
      return "unordered_session"
    end
    state.last = at
    if state.ended then
      return "multiple_connections"
    end
    if entry.kind == "exit" then
      if entry.direction ~= "internal" then
        return "malformed_exit"
      end
      state.ended = true
      return
    end
    if entry.kind ~= "frame" then
      return
    end
    if entry.direction ~= "client_to_agent" and entry.direction ~= "agent_to_client" then
      return "malformed_direction"
    end
    local payload = entry.payload
    if not Protocol.validate(payload) then
      return "malformed_frame"
    end
    local params = type(payload.params) == "table" and payload.params or nil
    if params and params.sessionId ~= nil and params.sessionId ~= source.session_id then
      return "session_mismatch"
    end
    if
      state.started
      and entry.direction == "client_to_agent"
      and (payload.method == "session/load" or payload.method == "session/new")
    then
      return "multiple_connections"
    end
    if not state.started then
      local is_load = entry.direction == "client_to_agent"
        and payload.method == "session/load"
        and params
        and params.sessionId == source.session_id
        and id_key(payload.id)
      local is_new_response = entry.direction == "agent_to_client"
        and payload.method == nil
        and connection.startup
        and id_key(payload.id) == id_key(connection.startup.id)
        and type(payload.result) == "table"
        and payload.result.sessionId == source.session_id
      if not is_load and not is_new_response then
        return "missing_start"
      end
      if connection.binding.at < at then
        return "invalid_binding_order"
      end
      state.started = true
    end
    if at >= until_time then
      return
    end
    if entry.direction == "client_to_agent" and payload.method then
      local key = id_key(payload.id)
      if key then
        if state.pending[key] then
          return "duplicate_request"
        end
        local request = { id = payload.id, method = payload.method, line = line }
        if payload.method == "session/set_config_option" then
          if
            not params
            or not bounded_string(params.configId, 128)
            or (type(params.value) ~= "boolean" and not bounded_string(params.value, 256))
          then
            return "malformed_option_request"
          end
          request.option, request.value = params.configId, params.value
        elseif payload.method == "session/prompt" then
          if not params or params.sessionId ~= source.session_id or not state.ready or state.active then
            return "unordered_prompt"
          end
          if connection.binding.at > at then
            return "invalid_binding_order"
          end
          if not state.options then
            if in_range(entry.timestamp, from, until_time) then
              exclude("missing_options", line)
            end
            state.active = { outside_selection = true }
          elseif in_range(entry.timestamp, from, until_time) and #state.candidates >= limits.candidates then
            exclude("candidate_limit", line)
            state.active = { outside_selection = true }
          elseif in_range(entry.timestamp, from, until_time) then
            local candidate = {
              session_id = source.session_id,
              connection_id = source.connection_id,
              request_id = payload.id,
              dispatched_at = entry.timestamp,
              options = state.options,
              model = state.model,
              cost_baseline = state.cost,
              costs = {},
              mixed = false,
              proposal = {
                agent = source.proposal.agent,
                provider = source.proposal.provider,
                confirmation = "unconfirmed",
              },
              evidence = {
                dispatch = evidence(index, "session", line),
                options = state.option_evidence,
                binding = evidence(index, "connection", connection.binding.line),
                previous_exit = source.first_line > 1 and evidence(index, "session", source.first_line - 1) or nil,
                cost_baseline = state.cost and state.cost_evidence or nil,
              },
            }
            state.candidates[#state.candidates + 1] = candidate
            state.active = candidate
          else
            state.active = { outside_selection = true }
          end
        end
        state.pending[key] = request
      elseif payload.id ~= nil then
        return "invalid_request_id"
      end
    elseif entry.direction == "agent_to_client" and payload.method == "session/update" then
      local update = params and params.update
      if not params or params.sessionId ~= source.session_id or type(update) ~= "table" then
        return "malformed_update"
      end
      if update.sessionUpdate == "config_option_update" then
        accept_options(update.configOptions, entry, line)
      elseif update.sessionUpdate == "usage_update" then
        local context, cost, present = Validation.usage_update(update)
        if context and (not finite(update.used) or not finite(update.size) or cost and not finite(cost.amount)) then
          context, cost, present = nil, nil, false
        end
        if not context then
          state.cost, state.cost_evidence = nil, nil
          if state.active and not state.active.outside_selection then
            state.active.telemetry_gap = true
          end
          if in_range(entry.timestamp, from, until_time) then
            exclude("invalid_telemetry", line)
          end
        elseif present then
          state.cost, state.cost_evidence = cost, cost and evidence(index, "session", line) or nil
          if state.active and not state.active.outside_selection then
            if #state.active.costs < MAX_COSTS then
              state.active.costs[#state.active.costs + 1] = {
                observed_at = entry.timestamp,
                cost = cost or nvim.NIL,
                evidence = evidence(index, "session", line),
              }
            elseif not state.active.costs_truncated then
              state.active.telemetry_gap, state.active.costs_truncated = true, true
              exclude("cost_limit", line)
            end
          end
        end
      elseif
        update.sessionUpdate == "session_info_update"
        and state.active
        and Validation.codex_system_error(update._meta)
      then
        state.active.failed = true
      end
    elseif entry.direction == "agent_to_client" and payload.method == nil then
      local key = id_key(payload.id)
      local request = key and state.pending[key]
      if request then
        state.pending[key] = nil
        if request.method == "session/new" or request.method == "session/load" then
          if
            payload.error
            or type(payload.result) ~= "table"
            or payload.result.sessionId ~= nil and payload.result.sessionId ~= source.session_id
          then
            return "invalid_start_response"
          end
          if request.method ~= "session/load" or payload.result.configOptions ~= nil or not state.options then
            accept_options(payload.result.configOptions, entry, line)
          end
          state.ready = true
        elseif request.method == "session/set_config_option" then
          if not payload.error then
            if type(payload.result) ~= "table" then
              state.options, state.model, state.option_evidence = nil, nil, nil
              exclude("invalid_options", line)
            else
              accept_options(
                payload.result.configOptions,
                entry,
                line,
                { id = request.id, option = request.option, value = request.value },
                evidence(index, "session", request.line)
              )
            end
          end
        elseif request.method == "session/prompt" then
          local active = state.active
          state.active = nil
          if active and not active.outside_selection then
            active.peer_response = true
            active.evidence.response = evidence(index, "session", line)
            if payload.error then
              active.outcome = "failed"
              active.outcome_at = entry.timestamp
              active.evidence.outcome = evidence(index, "session", line)
            elseif type(payload.result) ~= "table" then
              exclude("invalid_completion", line)
            else
              local reasons =
                { end_turn = true, max_tokens = true, max_turn_requests = true, refusal = true, cancelled = true }
              local usage, usage_ok = Validation.turn_usage(payload.result.usage)
              local failure, failure_error = Validation.session_failure(payload.result._meta)
              if not reasons[payload.result.stopReason] or not usage_ok or failure_error then
                exclude("invalid_completion", line)
              else
                active.outcome = (active.failed or (failure and failure.severity == "error")) and "failed"
                  or (payload.result.stopReason == "cancelled" and "cancelled" or "completed")
                active.stop_reason = payload.result.stopReason
                active.usage = usage
                active.outcome_at = entry.timestamp
                active.evidence.outcome = evidence(index, "session", line)
              end
            end
            active.failed = nil
          end
        end
      elseif type(payload.result) == "table" and payload.result.configOptions ~= nil then
        state.options, state.model, state.option_evidence = nil, nil, nil
        exclude("unmatched_options", line)
      end
    end
  end

  local function next_batch()
    if cancelled() then
      return
    end
    local stop = math.min(state.cursor + 99, source.last_line)
    while state.cursor <= stop do
      local line = state.cursor
      local entry = decode(records[line])
      state.cursor = state.cursor + 1
      if not entry then
        finished(nil, "malformed_session", line)
        return
      end
      local reason = consume(entry, line)
      if reason then
        finished(nil, reason, line)
        return
      end
    end
    if state.cursor <= source.last_line then
      nvim.schedule(next_batch)
    elseif not state.started then
      finished(nil, "missing_start")
    else
      finished(state)
    end
  end
  next_batch()
end

---Build the payload-free preview in scheduled batches from already bounded reads.
---@param selection louiselm.session.ImportSelection
---@param files table<string, table>
---@param callback fun(report: louiselm.session.ImportReport)
---@param cancelled fun(): boolean
function M.report(selection, files, callback, cancelled)
  local result = {
    selection = selection,
    attribution_confirmation = "unconfirmed",
    digest = "",
    sources = {},
    batches = {},
    candidates = {},
    transitions = {},
    exclusions = {},
    exclusion_count = 0,
    counts = { candidates = 0, exclusions = 0, transitions = 0 },
  }
  local decoded_lines, grouped, bad_boundaries, boundary_checks, prepared = {}, {}, {}, {}, {}
  for source_index, source in ipairs(selection.sources) do
    if source.kind == "acp" then
      grouped[source.session_path] = grouped[source.session_path] or {}
      grouped[source.session_path][#grouped[source.session_path] + 1] = { index = source_index, source = source }
    end
  end
  for filename, group in pairs(grouped) do
    if #group > 1 and files[filename].status == "ok" then
      local records = lines(files[filename].bytes)
      decoded_lines[filename] = records
      table.sort(group, function(a, b)
        return a.source.first_line < b.source.first_line
      end)
      if records then
        for position = 2, #group do
          local previous, current = group[position - 1], group[position]
          local ended = records[previous.source.last_line] and decode(records[previous.source.last_line])
          if
            previous.source.last_line + 1 ~= current.source.first_line
            or not ended
            or ended.kind ~= "exit"
            or ended.direction ~= "internal"
            or ended.session_id ~= previous.source.session_id
          then
            bad_boundaries[previous.index], bad_boundaries[current.index] = true, true
          else
            boundary_checks[#boundary_checks + 1] = { previous = previous, current = current, ended = ended.timestamp }
          end
        end
      end
    end
  end
  local index = 0
  local function next_source()
    if cancelled() then
      return
    end
    index = index + 1
    local source = selection.sources[index]
    if not source then
      result.counts =
        { candidates = #result.candidates, exclusions = result.exclusion_count, transitions = #result.transitions }
      result.exclusion_count = nil
      callback(result)
      return
    end
    local current = index
    if source.kind == "native" then
      result.sources[current] = { kind = "native", path = source.path, status = "unsupported" }
      add_exclusion(result, current, "unsupported_source")
      nvim.schedule(next_source)
      return
    end
    local connection, session = files[source.connection_path], files[source.session_path]
    local summary = {
      kind = "acp",
      session_id = source.session_id,
      connection_id = source.connection_id,
      connection = source_file(connection, source.connection_path),
      session = source_file(session, source.session_path),
      proposal = { agent = source.proposal.agent, provider = source.proposal.provider, confirmation = "unconfirmed" },
      first_line = source.first_line,
      last_line = source.last_line,
      status = "ok",
    }
    local batch = {
      source = current,
      session_id = source.session_id,
      connection_id = source.connection_id,
      from = selection.from,
      until_time = selection.until_time,
      proposal = summary.proposal,
      status = "ok",
      candidates = 0,
      transitions = 0,
      exclusions = 0,
    }
    result.batches[#result.batches + 1] = batch
    result.sources[current] = summary
    if connection.status ~= "ok" or session.status ~= "ok" then
      summary.status = connection.status ~= "ok" and connection.status or session.status
      batch.status, batch.exclusions = summary.status, 1
      add_exclusion(result, current, summary.status)
      nvim.schedule(next_source)
      return
    end
    if bad_boundaries[current] then
      summary.status = "excluded"
      batch.status, batch.exclusions = summary.status, 1
      add_exclusion(result, current, "unverified_connection_boundary")
      nvim.schedule(next_source)
      return
    end
    local connection_lines, connection_error = decoded_lines[source.connection_path], nil
    if not connection_lines then
      connection_lines, connection_error = lines(connection.bytes)
    end
    local session_lines, session_error = decoded_lines[source.session_path], nil
    if not session_lines then
      session_lines, session_error = lines(session.bytes)
    end
    decoded_lines[source.connection_path], decoded_lines[source.session_path] = connection_lines, session_lines
    if not connection_lines or not session_lines then
      summary.status = connection_error or session_error
      batch.status, batch.exclusions = summary.status, 1
      add_exclusion(result, current, summary.status)
      nvim.schedule(next_source)
      return
    end
    local function reject(reason, line, file)
      summary.status = "excluded"
      batch.status, batch.exclusions = summary.status, 1
      add_exclusion(result, current, reason, line, file)
      nvim.schedule(next_source)
    end
    local function on_connection(parsed, reason, line)
      if not parsed then
        reject(reason, line, "connection")
        return
      end
      batch.binding = evidence(current, "connection", parsed.binding.line)
      local limits =
        { candidates = MAX_CANDIDATES - #result.candidates, transitions = MAX_TRANSITIONS - #result.transitions }
      process_session(
        session_lines,
        source,
        current,
        parsed,
        selection,
        limits,
        function(state, session_reason, session_line)
          if not state then
            reject(session_reason, session_line, "session")
            return
          end
          batch.candidates, batch.transitions, batch.exclusions =
            #state.candidates, #state.transitions, state.exclusion_count
          for _, candidate in ipairs(state.candidates) do
            result.candidates[#result.candidates + 1] = candidate
          end
          for _, transition in ipairs(state.transitions) do
            result.transitions[#result.transitions + 1] = transition
          end
          result.exclusion_count = result.exclusion_count + state.exclusion_count
          for _, exclusion in ipairs(state.exclusions) do
            if #result.exclusions < MAX_REASONS then
              result.exclusions[#result.exclusions + 1] = exclusion
            end
          end
          nvim.schedule(next_source)
        end,
        cancelled
      )
    end
    local saved = prepared[current]
    if saved then
      on_connection(saved.parsed, saved.reason, saved.line)
    else
      prepare_connection(connection_lines, source, on_connection, cancelled)
    end
  end
  local boundary_index = 0
  local function next_boundary()
    if cancelled() then
      return
    end
    boundary_index = boundary_index + 1
    local check = boundary_checks[boundary_index]
    if not check then
      nvim.schedule(next_source)
      return
    end
    local source = check.current.source
    local file = files[source.connection_path]
    local connection_lines = file.status == "ok" and lines(file.bytes) or nil
    if not connection_lines then
      nvim.schedule(next_boundary)
      return
    end
    decoded_lines[source.connection_path] = connection_lines
    prepare_connection(connection_lines, source, function(parsed, reason, line)
      prepared[check.current.index] = { parsed = parsed, reason = reason, line = line }
      if parsed and parsed.binding.at <= Validation.timestamp_key(check.ended) then
        bad_boundaries[check.previous.index], bad_boundaries[check.current.index] = true, true
      end
      nvim.schedule(next_boundary)
    end, cancelled)
  end
  nvim.schedule(next_boundary)
end

return M
