---@diagnostic disable-next-line: undefined-global -- Neovim supplies JSON and hashing.
local nvim = vim
local M = {}

-- Source indexes are local to a preview; file and line identify the same
-- evidence even when another reviewed selection orders sources differently.
local function stable_reference(ref)
  return { file = ref.file, line = ref.line }
end

---@class louiselm.session.ImportSqlEntry
---@field candidate louiselm.session.ImportCandidate
---@field key string
---@field turn_id string
---@field fingerprint string
---@field source louiselm.session.ImportSource
---@field session_digest string

---Encode a revalidated preview for one atomic recorder transaction.
---Call from a coroutine: large inputs yield while encoding. Returns nil for
---inconsistent normalized facts; the caller owns I/O and sanitized errors.
---@param report louiselm.session.ImportReport
---@param declaration louiselm.session.ImportDeclaration
---@param sql_text fun(value: string?): string
---@param json fun(value: unknown): string
---@param record_sql fun(record: table, condition: string): string?
---@param write_header fun(): string[]
---@return string[]? sql
---@return louiselm.session.ImportSqlEntry[]? entries
---@return string? batch_id
---@return table<string, louiselm.session.ImportEvidence>? transitions
function M.build(report, declaration, sql_text, json, record_sql, write_header)
  local function import_hash(value)
    return "sha256:" .. nvim.fn.sha256(json(value))
  end

  local work = 0
  local function checkpoint()
    work = work + 1
    if work >= 64 then
      work = 0
      coroutine.yield()
    end
  end
  local batch_id = import_hash({ digest = report.digest, declaration = declaration })
  local sql = write_header()
  sql[#sql + 1] = "INSERT INTO recording_guard SELECT NOT EXISTS(SELECT 1 FROM pragma_foreign_key_check);"
  sql[#sql + 1] = "INSERT INTO import_batches(id,digest,declaration,selection,sources) VALUES("
    .. table.concat({
      sql_text(batch_id),
      sql_text(report.digest),
      sql_text(json(declaration)),
      sql_text(json(report.selection)),
      sql_text(json(report.sources)),
    }, ",")
    .. ") ON CONFLICT(id) DO NOTHING;"
  sql[#sql + 1] = [[CREATE TEMP TABLE incoming(
 key TEXT,turn_id TEXT,fingerprint TEXT,agent TEXT,provider TEXT,session TEXT,
 request TEXT,dispatched_at TEXT,source INTEGER,connection_id TEXT,session_digest TEXT,
 first_line INTEGER,last_line INTEGER,dispatch_line INTEGER
);]]
  local entries = {} ---@type louiselm.session.ImportSqlEntry[]
  for _, candidate in ipairs(report.candidates) do
    checkpoint()
    local key = import_hash({ candidate.session_id, candidate.connection_id, candidate.evidence.dispatch.line })
    local turn_id = "import:" .. key
    local evidence, costs = {}, {}
    for key, ref in pairs(candidate.evidence) do
      evidence[key] = stable_reference(ref)
    end
    for _, cost in ipairs(candidate.costs) do
      costs[#costs + 1] = {
        observed_at = cost.observed_at,
        cost = cost.cost,
        evidence = stable_reference(cost.evidence),
      }
      checkpoint()
    end
    local facts = {
      session_id = candidate.session_id,
      connection_id = candidate.connection_id,
      request_id = candidate.request_id,
      dispatched_at = candidate.dispatched_at,
      options = candidate.options,
      model = candidate.model,
      cost_baseline = candidate.cost_baseline,
      costs = costs,
      mixed = candidate.mixed,
      options_uncertain = candidate.options_uncertain,
      telemetry_gap = candidate.telemetry_gap,
      costs_truncated = candidate.costs_truncated,
      peer_response = candidate.peer_response,
      outcome = candidate.outcome,
      outcome_at = candidate.outcome_at,
      stop_reason = candidate.stop_reason,
      usage = candidate.usage,
      evidence = evidence,
    }
    local fingerprint = import_hash(facts)
    local source_index = candidate.evidence.dispatch.source
    local source = report.selection.sources[source_index]
    local source_result = report.sources[source_index]
    if
      not source
      or source.kind ~= "acp"
      or not source.first_line
      or not source.last_line
      or not source_result
      or not source_result.session
      or not source_result.session.sha256
    then
      return nil
    end
    local fields = {
      sql_text(key),
      sql_text(turn_id),
      sql_text(fingerprint),
      sql_text(candidate.proposal.agent),
      sql_text(candidate.proposal.provider),
      sql_text(candidate.session_id),
      sql_text(json(candidate.request_id)),
      sql_text(candidate.dispatched_at),
      tostring(source_index),
      sql_text(candidate.connection_id),
      sql_text(source_result.session.sha256),
      tostring(source.first_line),
      tostring(source.last_line),
      tostring(candidate.evidence.dispatch.line),
    }
    sql[#sql + 1] = "INSERT INTO incoming VALUES(" .. table.concat(fields, ",") .. ");"
    entries[#entries + 1] = {
      candidate = candidate,
      key = key,
      turn_id = turn_id,
      fingerprint = fingerprint,
      source = source,
      session_digest = source_result.session.sha256,
    }
  end
  sql[#sql + 1] = [[
CREATE TEMP TABLE import_decisions AS
SELECT c.*,CASE
 WHEN EXISTS(SELECT 1 FROM import_turns i WHERE i.evidence_key=c.key) THEN
  CASE WHEN EXISTS(SELECT 1 FROM import_turns i JOIN turns t ON t.id=i.turn_id
   WHERE i.evidence_key=c.key AND i.fingerprint=c.fingerprint
   AND t.agent=c.agent AND t.provider=c.provider) THEN 'existing' ELSE 'conflicting_import' END
 WHEN EXISTS(SELECT 1 FROM turns t LEFT JOIN import_turns i ON i.turn_id=t.id
  WHERE i.turn_id IS NULL AND t.acp_session_id=c.session AND (
   NOT EXISTS(SELECT 1 FROM turn_events e WHERE e.turn_id=t.id AND e.kind IN ('dispatch','outcome')) OR
   EXISTS(SELECT 1 FROM turn_events e WHERE e.turn_id=t.id AND e.kind='dispatch'
    AND json_type(e.data,'$.request_id')=json_type(c.request)
    AND json_extract(e.data,'$.request_id') IS json_extract(c.request,'$')))) THEN 'live_overlap'
 WHEN EXISTS(SELECT 1 FROM turns t WHERE t.acp_session_id=c.session AND t.agent<>c.agent)
  THEN 'conflicting_agent'
 WHEN EXISTS(SELECT 1 FROM turns t JOIN import_turns i ON i.turn_id=t.id
  JOIN turn_events e ON e.turn_id=t.id AND e.kind='dispatch'
  WHERE t.acp_session_id=c.session
   AND json_type(e.data,'$.request_id')=json_type(c.request)
   AND json_extract(e.data,'$.request_id') IS json_extract(c.request,'$')
   AND NOT (i.session_digest=c.session_digest AND (
    (i.connection_id=c.connection_id AND i.dispatch_line<>c.dispatch_line) OR
    (i.connection_id<>c.connection_id AND (i.last_line<c.first_line OR c.last_line<i.first_line)))))
  THEN 'ambiguous_overlap'
 ELSE 'admitted' END AS decision FROM incoming c;
CREATE UNIQUE INDEX import_decisions_key ON import_decisions(key);
CREATE INDEX import_decisions_source ON import_decisions(source,decision);
]]
  for _, entry in ipairs(entries) do
    local candidate = entry.candidate
    local check = "EXISTS(SELECT 1 FROM import_decisions WHERE key="
      .. sql_text(entry.key)
      .. " AND decision IN ('admitted','existing'))"
    local records = {
      {
        id = entry.turn_id,
        agent = candidate.proposal.agent,
        provider = candidate.proposal.provider,
        acp_session_id = candidate.session_id,
        prepared_at = candidate.dispatched_at,
        options = candidate.options,
        model = candidate.model,
        cost_baseline = candidate.cost_baseline,
      },
      {
        turn_id = entry.turn_id,
        sequence = 1,
        kind = "dispatch",
        observed_at = candidate.dispatched_at,
        data = { request_id = candidate.request_id },
      },
    }
    for _, cost in ipairs(candidate.costs) do
      checkpoint()
      records[#records + 1] = {
        turn_id = entry.turn_id,
        sequence = #records,
        kind = "cost",
        observed_at = cost.observed_at,
        data = { cost = cost.cost },
      }
    end
    if candidate.outcome then
      records[#records + 1] = {
        turn_id = entry.turn_id,
        sequence = #records,
        kind = "outcome",
        observed_at = candidate.outcome_at,
        data = { outcome = candidate.outcome, peer_response = candidate.peer_response == true, usage = candidate.usage },
      }
    end
    for _, record in ipairs(records) do
      checkpoint()
      local statement = record_sql(record, check)
      if not statement then
        return nil
      end
      sql[#sql + 1] = statement
    end
    local source = entry.source
    sql[#sql + 1] = "INSERT INTO import_turns(turn_id,batch_id,evidence_key,fingerprint,evidence,connection_id,session_digest,first_line,last_line,dispatch_line,mixed,telemetry_gap) SELECT "
      .. table.concat({
        sql_text(entry.turn_id),
        sql_text(batch_id),
        sql_text(entry.key),
        sql_text(entry.fingerprint),
        sql_text(json(candidate.evidence)),
        sql_text(candidate.connection_id),
        sql_text(entry.session_digest),
        tostring(source.first_line),
        tostring(source.last_line),
        tostring(candidate.evidence.dispatch.line),
        candidate.mixed and "1" or "0",
        candidate.telemetry_gap and "1" or "0",
      }, ",")
      .. " WHERE EXISTS(SELECT 1 FROM import_decisions WHERE key="
      .. sql_text(entry.key)
      .. " AND decision='admitted');"
  end
  local transition_entries = {}
  sql[#sql + 1] = "CREATE TEMP TABLE expected_options(event_id TEXT);"
  for _, transition in ipairs(report.transitions) do
    checkpoint()
    local transition_id = "import-option:"
      .. import_hash({ transition.session_id, transition.connection_id, transition.evidence.line })
    transition_entries[transition_id] = transition.evidence
    sql[#sql + 1] = "INSERT INTO expected_options VALUES(" .. sql_text(transition_id) .. ");"
    local linked
    for _, entry in ipairs(entries) do
      checkpoint()
      local candidate = entry.candidate
      local ref = transition.evidence
      local dispatch = candidate.evidence.dispatch
      local response = candidate.evidence.response
      if ref.source == dispatch.source and ref.line > dispatch.line and (not response or ref.line < response.line) then
        linked = true
        local event_id = transition_id
        local condition = "EXISTS(SELECT 1 FROM import_decisions WHERE key="
          .. sql_text(entry.key)
          .. " AND decision IN ('admitted','existing'))"
        local statement = record_sql({
          kind = "options",
          id = event_id,
          observer_id = "import-stream:" .. import_hash({ transition.session_id, transition.connection_id }),
          sequence = ref.line,
          agent = candidate.proposal.agent,
          acp_session_id = transition.session_id,
          observed_at = transition.observed_at,
          previous_options = transition.previous_options,
          options = transition.options,
          source = transition.source,
          request = transition.request,
          turn_id = entry.turn_id,
        }, condition)
        if not statement then
          return nil
        end
        sql[#sql + 1] = statement
        sql[#sql + 1] = "INSERT INTO import_options(event_id,batch_id,evidence) SELECT "
          .. sql_text(event_id)
          .. ","
          .. sql_text(batch_id)
          .. ","
          .. sql_text(json({ observation = ref, request = transition.request_evidence }))
          .. " WHERE "
          .. condition
          .. " ON CONFLICT(event_id) DO NOTHING;"
      end
    end
    if not linked and not transition.during_unselected_turn then
      local ref = transition.evidence
      local source = report.selection.sources[ref.source]
      if not source or source.kind ~= "acp" or not source.proposal then
        return nil
      end
      local event_id = transition_id
      local condition = "EXISTS(SELECT 1 FROM import_decisions WHERE source="
        .. ref.source
        .. " AND decision='admitted') AND NOT EXISTS(SELECT 1 FROM import_decisions WHERE source="
        .. ref.source
        .. " AND decision NOT IN ('admitted','existing')) AND NOT EXISTS(SELECT 1 FROM turns t "
        .. "LEFT JOIN import_turns i ON i.turn_id=t.id WHERE i.turn_id IS NULL AND t.acp_session_id="
        .. sql_text(transition.session_id)
        .. ")"
      local statement = record_sql({
        kind = "options",
        id = event_id,
        observer_id = "import-stream:" .. import_hash({ transition.session_id, transition.connection_id }),
        sequence = ref.line,
        agent = source.proposal.agent,
        acp_session_id = transition.session_id,
        observed_at = transition.observed_at,
        previous_options = transition.previous_options,
        options = transition.options,
        source = transition.source,
        request = transition.request,
      }, condition)
      if not statement then
        return nil
      end
      sql[#sql + 1] = statement
      sql[#sql + 1] = "INSERT INTO import_options(event_id,batch_id,evidence) SELECT "
        .. sql_text(event_id)
        .. ","
        .. sql_text(batch_id)
        .. ","
        .. sql_text(json({ observation = ref, request = transition.request_evidence }))
        .. " WHERE "
        .. condition
        .. " ON CONFLICT(event_id) DO NOTHING;"
    end
  end
  sql[#sql + 1] = ".output stdout"
  sql[#sql + 1] = [[SELECT key,decision FROM import_decisions
UNION ALL SELECT e.event_id,'ambiguous_option_transition' FROM expected_options e
WHERE NOT EXISTS(SELECT 1 FROM import_options i WHERE i.event_id=e.event_id);]]
  sql[#sql + 1] = "COMMIT;"
  table.insert(sql, 1, ".output /dev/null")
  return sql, entries, batch_id, transition_entries
end

return M
