---@diagnostic disable-next-line: undefined-global -- Neovim owns process and filesystem effects.
local nvim = vim

---@alias louiselm.session.RecordingErrorCode "invalid"|"unavailable"|"permissions"|"locked"|"corrupt"|"conflict"|"storage"|"attribution"
---@class louiselm.session.RecordingError
---@field code louiselm.session.RecordingErrorCode
---@field message string Sanitized, never includes SQL or recorded values.

---@class louiselm.session.PreparedTurn: louiselm.session.TurnIdentity
---@field id string Random durable turn identifier, independent of editor and replay ordinals.
---@field acp_session_id string Agent-side conversation identity.
---@field prepared_at string UTC timestamp.
---@field cost_baseline? louiselm.session.Cost Last observed cumulative cost before admission.

---@class louiselm.session.TurnObservation
---@field turn_id string Prepared turn identifier.
---@field sequence integer Monotonic within this turn; starts at 1.
---@field kind "dispatch"|"cost"|"cancel_requested"|"outcome"
---@field observed_at string UTC timestamp.
---@field data table Normalized metadata only; see docs/turn-recording.md.

---@class louiselm.session.OptionRequest
---@field id string|number ACP request answered by this response.
---@field option string Explicitly requested option ID; other changes are not attributed to this request.
---@field value string|boolean Explicitly requested typed value.

---@class louiselm.session.OptionTransition
---@field kind "options"
---@field id string Stable event identity, retained across write retries.
---@field observer_id string Random identity of this live Session observation stream.
---@field sequence integer Observation order within that stream, independent of timestamp ties.
---@field agent string Configured Agent.
---@field acp_session_id string Agent-side Session identity.
---@field observed_at string UTC observation timestamp.
---@field previous_options table<string, string|boolean> Values before this confirmed replacement.
---@field options table<string, string|boolean> Values after this confirmed replacement.
---@field source "notification"|"response" Observed ACP source, never inferred intent.
---@field request? louiselm.session.OptionRequest Present only for the matched response.
---@field turn_id? string Active attempt affected by this transition; its starting tuple stays immutable.

---@alias louiselm.session.RecordingCallback fun(error?: louiselm.session.RecordingError)
---@class louiselm.session.ReplayUsage
---@field id string Durable turn ID; the ordinal is only a presentation association.
---@field turn integer Observed transcript position of the dispatched prompt.
---@field usage? louiselm.session.TurnUsage Reported completion usage, absent for unmeasured/unobserved outcomes.
---@alias louiselm.session.UsageHistoryCallback fun(records: louiselm.session.ReplayUsage[]?, error?: louiselm.session.RecordingError)
---@class louiselm.session.RecordingWrite
---@field sql string Encoded immutable transaction fragment.
---@field error? louiselm.session.RecordingError Invalid metadata fences the queue; it cannot become a successful admission barrier.
---@field callback? louiselm.session.RecordingCallback Cleared after its first acknowledgement, including failure.

---@class louiselm.session.RecordingStore
---@field directory string Private storage directory.
---@field path string SQLite database path.
---@field queue louiselm.session.RecordingWrite[] Unacknowledged writes, retained on failure.
---@field busy boolean Whether a bounded write is scheduled/running.
---@field error? louiselm.session.RecordingError Last failed write.
---@field changed fun(error: louiselm.session.RecordingError?, pending: boolean) Main-loop observer.
---@field append fun(self: louiselm.session.RecordingStore, record: louiselm.session.PreparedTurn|louiselm.session.TurnObservation|louiselm.session.OptionTransition, callback?: louiselm.session.RecordingCallback)
---@field flush fun(self: louiselm.session.RecordingStore, callback: louiselm.session.RecordingCallback)
---@field usage_history fun(self: louiselm.session.RecordingStore, agent: string, acp_session_id: string, callback: louiselm.session.UsageHistoryCallback)
---@field usage_summaries fun(self: louiselm.session.RecordingStore, cohorts: louiselm.session.UsageCohort[], callback: louiselm.session.UsageSummariesCallback)

---@class louiselm.session.UsageCohort
---@field agent string Exact configured Agent.
---@field provider string Resolved service for this candidate.
---@field options table<string, string|boolean> Complete typed tuple; no widening or legacy fallback.
---@class louiselm.session.UsageMetric
---@field samples integer Turns reporting this measurement.
---@field average number Mean over measured turns only.
---@class louiselm.session.UsageCurrency: louiselm.session.UsageMetric
---@field currency string Reported currency; never combined across currencies.
---@class louiselm.session.CohortSummary
---@field turns integer Dispatched turns, including unmeasured and unobserved completions.
---@field outcomes table<string, integer> Counts by observed outcome; unobserved means no terminal observation.
---@field tokens table<string, louiselm.session.UsageMetric> Reported token fields, absent when unmeasured.
---@field costs louiselm.session.UsageCurrency[] Complete reported deltas in currency order.
---@alias louiselm.session.UsageSummariesCallback fun(summaries: louiselm.session.CohortSummary[]?, error?: louiselm.session.RecordingError)

local M = {}
local Store = {}
Store.__index = Store

local SCHEMA = [[
CREATE TABLE IF NOT EXISTS turns (
  id TEXT PRIMARY KEY NOT NULL,
  agent TEXT NOT NULL, provider TEXT NOT NULL, acp_session_id TEXT NOT NULL,
  prepared_at TEXT NOT NULL, options TEXT NOT NULL, model TEXT, cost_baseline TEXT
) STRICT;
CREATE INDEX IF NOT EXISTS turns_session ON turns(agent, acp_session_id, prepared_at);
CREATE TABLE IF NOT EXISTS turn_events (
  turn_id TEXT NOT NULL REFERENCES turns(id), sequence INTEGER NOT NULL CHECK(sequence > 0),
  kind TEXT NOT NULL CHECK(kind IN ('dispatch','cost','cancel_requested','outcome')),
  observed_at TEXT NOT NULL, data TEXT NOT NULL,
  PRIMARY KEY(turn_id, sequence)
) STRICT;
CREATE UNIQUE INDEX IF NOT EXISTS turn_terminal ON turn_events(turn_id) WHERE kind = 'outcome';
CREATE TABLE IF NOT EXISTS option_events (
  id TEXT PRIMARY KEY NOT NULL,
  observer_id TEXT NOT NULL, sequence INTEGER NOT NULL CHECK(sequence > 0),
  agent TEXT NOT NULL, acp_session_id TEXT NOT NULL, observed_at TEXT NOT NULL,
  previous_options TEXT NOT NULL, options TEXT NOT NULL,
  source TEXT NOT NULL CHECK(source IN ('notification','response')), request TEXT,
  turn_id TEXT REFERENCES turns(id),
  UNIQUE(observer_id, sequence)
) STRICT;
CREATE INDEX IF NOT EXISTS option_events_turn ON option_events(turn_id);
]]

---@param code louiselm.session.RecordingErrorCode
---@return louiselm.session.RecordingError
local function failure(code)
  local messages = {
    invalid = "invalid turn recording metadata",
    unavailable = "turn recording requires sqlite3 >= 3.38 with JSON support on PATH",
    permissions = "turn recording needs an owned private directory (0700) and regular database (0600)",
    locked = "turn recording database is locked; retry after the writer finishes",
    corrupt = "turn recording database is corrupt or has an unsupported schema",
    conflict = "turn recording conflicts with an existing immutable fact",
    storage = "turn recording failed; check storage access and free space, then retry",
  }
  return { code = code, message = messages[code] }
end

-- Values never enter SQL grammar or CLI dot commands, even with quotes/NUL/newlines.
---@param value string?
---@return string
local function sql_text(value)
  if value == nil then
    return "NULL"
  end
  return "CAST(X'"
    .. value:gsub(".", function(byte)
      return string.format("%02x", byte:byte())
    end)
    .. "' AS TEXT)"
end

---@param value unknown
---@return string
local function json(value)
  if type(value) ~= "table" then
    return nvim.json.encode(value)
  end
  local keys = {}
  for key in pairs(value) do
    if type(key) ~= "string" then
      error("record metadata must be an object")
    end
    keys[#keys + 1] = key
  end
  table.sort(keys)
  local fields = {}
  for _, key in ipairs(keys) do
    fields[#fields + 1] = nvim.json.encode(key) .. ":" .. json(value[key])
  end
  return "{" .. table.concat(fields, ",") .. "}"
end

local function nonempty(value)
  return type(value) == "string" and value ~= ""
end

local function finite(value)
  return type(value) == "number" and value >= 0 and value < math.huge
end

---@param value unknown
---@return boolean
local function tuple_valid(value)
  if type(value) ~= "table" then
    return false
  end
  for key, item in pairs(value) do
    if not nonempty(key) or (type(item) ~= "string" and type(item) ~= "boolean") then
      return false
    end
  end
  return true
end

local function cost_valid(value)
  return value == nil
    or value == nvim.NIL
    or (
      type(value) == "table"
      and finite(value.amount)
      and type(value.currency) == "string"
      and value.currency:match("^[A-Z][A-Z][A-Z]$") ~= nil
      and nvim.tbl_count(value) == 2
    )
end

local TOKEN_FIELDS = {
  total_tokens = true,
  input_tokens = true,
  output_tokens = true,
  thought_tokens = true,
  cached_read_tokens = true,
  cached_write_tokens = true,
}

---@param record louiselm.session.PreparedTurn|louiselm.session.TurnObservation|louiselm.session.OptionTransition
---@return string?
local function record_sql(record)
  if type(record) ~= "table" then
    return nil
  end
  local columns, values, table_name, key, equal
  if record.kind == "options" then
    if
      not nonempty(record.id)
      or not nonempty(record.observer_id)
      or not finite(record.sequence)
      or record.sequence < 1
      or record.sequence % 1 ~= 0
      or not nonempty(record.agent)
      or not nonempty(record.acp_session_id)
      or not nonempty(record.observed_at)
      or not tuple_valid(record.previous_options)
      or not tuple_valid(record.options)
      or (record.turn_id ~= nil and not nonempty(record.turn_id))
    then
      return nil
    end
    local request = record.request
    if record.source == "response" then
      if
        type(request) ~= "table"
        or not nonempty(request.option)
        or (type(request.value) ~= "string" and type(request.value) ~= "boolean")
        or not (nonempty(request.id) or (finite(request.id) and request.id % 1 == 0))
      then
        return nil
      end
      request = { id = request.id, option = request.option, value = request.value }
    elseif record.source ~= "notification" or request ~= nil then
      return nil
    end
    columns = {
      "id",
      "observer_id",
      "sequence",
      "agent",
      "acp_session_id",
      "observed_at",
      "previous_options",
      "options",
      "source",
      "request",
      "turn_id",
    }
    values = {
      sql_text(record.id),
      sql_text(record.observer_id),
      tostring(record.sequence),
      sql_text(record.agent),
      sql_text(record.acp_session_id),
      sql_text(record.observed_at),
      sql_text(json(record.previous_options)),
      sql_text(json(record.options)),
      sql_text(record.source),
      sql_text(request and json(request) or nil),
      sql_text(record.turn_id),
    }
    table_name, key = "option_events", "id"
  elseif record.id ~= nil then
    if
      not nonempty(record.id)
      or not nonempty(record.agent)
      or not nonempty(record.provider)
      or not nonempty(record.acp_session_id)
      or not nonempty(record.prepared_at)
      or not tuple_valid(record.options)
      or not cost_valid(record.cost_baseline)
      or (record.model ~= nil and type(record.model) ~= "string" and type(record.model) ~= "boolean")
    then
      return nil
    end
    columns = { "id", "agent", "provider", "acp_session_id", "prepared_at", "options", "model", "cost_baseline" }
    values = {
      sql_text(record.id),
      sql_text(record.agent),
      sql_text(record.provider),
      sql_text(record.acp_session_id),
      sql_text(record.prepared_at),
      sql_text(json(record.options)),
      sql_text(record.model ~= nil and json(record.model) or nil),
      sql_text(record.cost_baseline ~= nil and json(record.cost_baseline) or nil),
    }
    table_name, key = "turns", "id"
  else
    if
      not nonempty(record.turn_id)
      or not finite(record.sequence)
      or record.sequence < 1
      or record.sequence % 1 ~= 0
      or not nonempty(record.observed_at)
      or type(record.data) ~= "table"
    then
      return nil
    end
    local data = record.data
    if record.kind == "dispatch" then
      if type(data.request_id) ~= "string" and type(data.request_id) ~= "number" then
        return nil
      end
      if
        data.transcript_turn ~= nil
        and (not finite(data.transcript_turn) or data.transcript_turn < 1 or data.transcript_turn % 1 ~= 0)
      then
        return nil
      end
      data = { request_id = data.request_id, transcript_turn = data.transcript_turn }
    elseif record.kind == "cost" then
      if data.cost == nil or not cost_valid(data.cost) then
        return nil
      end
      data = { cost = data.cost }
    elseif record.kind == "cancel_requested" then
      data = {}
    elseif record.kind == "outcome" then
      local outcomes = { completed = true, cancelled = true, failed = true, disposed = true, not_sent = true }
      if not outcomes[data.outcome] or type(data.peer_response) ~= "boolean" then
        return nil
      end
      local usage
      if data.usage ~= nil then
        if type(data.usage) ~= "table" then
          return nil
        end
        usage = {}
        for field, amount in pairs(data.usage) do
          if not TOKEN_FIELDS[field] or not finite(amount) or amount % 1 ~= 0 then
            return nil
          end
          usage[field] = amount
        end
      end
      data = { outcome = data.outcome, peer_response = data.peer_response, usage = usage }
    else
      return nil
    end
    columns = { "turn_id", "sequence", "kind", "observed_at", "data" }
    values = {
      sql_text(record.turn_id),
      tostring(record.sequence),
      sql_text(record.kind),
      sql_text(record.observed_at),
      sql_text(json(data)),
    }
    table_name, key = "turn_events", "turn_id,sequence"
  end
  equal = {}
  for _, column in ipairs(columns) do
    equal[#equal + 1] = table_name .. "." .. column .. " IS excluded." .. column
  end
  -- A conflicting retry deliberately violates NOT NULL, rolling back the batch.
  local checked = columns[#columns]
  if table_name == "turns" or table_name == "option_events" then
    checked = "options"
  end
  return "INSERT INTO "
    .. table_name
    .. "("
    .. table.concat(columns, ",")
    .. ") VALUES("
    .. table.concat(values, ",")
    .. ") ON CONFLICT("
    .. key
    .. ") DO UPDATE SET "
    .. checked
    .. " = CASE WHEN "
    .. table.concat(equal, " AND ")
    .. " THEN excluded."
    .. checked
    .. " ELSE NULL END;"
end

---@param self louiselm.session.RecordingStore
---@param create boolean Whether this is a writer preparing a new store.
---@return louiselm.session.RecordingError?
local function private_store(self, create)
  if create then
    local ok, result = pcall(nvim.fn.mkdir, self.directory, "p", 448)
    if not ok or (result == 0 and nvim.uv.fs_lstat(self.directory) == nil) then
      return failure("permissions")
    end
  end
  local stat = nvim.uv.fs_lstat(self.directory)
  local uid = nvim.uv.getuid()
  if stat == nil or stat.type ~= "directory" or stat.uid ~= uid or stat.mode % 512 ~= 448 then
    return failure("permissions")
  end
  if create then
    local fd, _, code = nvim.uv.fs_open(self.path, "wx", 384)
    if fd ~= nil then
      if not nvim.uv.fs_close(fd) then
        return failure("storage")
      end
    elseif code ~= "EEXIST" then
      return failure("storage")
    end
  end
  stat = nvim.uv.fs_lstat(self.path)
  if stat == nil or stat.type ~= "file" or stat.uid ~= uid or stat.nlink ~= 1 or stat.mode % 512 ~= 384 then
    return failure("permissions")
  end
  for _, suffix in ipairs({ "-journal", "-wal", "-shm" }) do
    local sidecar, _, stat_code = nvim.uv.fs_lstat(self.path .. suffix)
    if sidecar ~= nil then
      if sidecar.type ~= "file" or sidecar.uid ~= uid or sidecar.nlink ~= 1 or sidecar.mode % 512 ~= 384 then
        return failure("permissions")
      end
    elseif stat_code ~= "ENOENT" then
      return failure("storage")
    end
  end
  return nil
end

---@param stderr string
---@return louiselm.session.RecordingError
local function sqlite_error(stderr)
  if stderr:find("locked", 1, true) then
    return failure("locked")
  end
  if stderr:find("NOT NULL constraint", 1, true) or stderr:find("UNIQUE constraint", 1, true) then
    return failure("conflict")
  end
  if
    stderr:find("not a database", 1, true)
    or stderr:find("malformed", 1, true)
    or stderr:find("CHECK constraint", 1, true)
  then
    return failure("corrupt")
  end
  if stderr:find("no such function", 1, true) or stderr:find("unknown option", 1, true) then
    return failure("unavailable")
  end
  return failure("storage")
end

---@param self louiselm.session.RecordingStore
local function drain(self)
  if self.busy or #self.queue == 0 then
    return
  end
  self.busy = true
  nvim.schedule(function()
    self.changed(self.error, true)
    local count = #self.queue
    local function finished(err)
      self.busy = false
      self.error = err
      local callbacks = {}
      for index = 1, err ~= nil and #self.queue or count do
        local item = self.queue[index]
        if item.callback then
          callbacks[#callbacks + 1] = item.callback
          item.callback = nil
        end
      end
      if err == nil then
        self.queue = nvim.list_slice(self.queue, count + 1)
      end
      self.changed(err, #self.queue > 0)
      for _, callback in ipairs(callbacks) do
        callback(err)
      end
      if err == nil then
        drain(self)
      end
    end
    for index = 1, count do
      if self.queue[index].error ~= nil then
        finished(self.queue[index].error)
        return
      end
    end
    if nvim.fn.executable("sqlite3") ~= 1 then
      finished(failure("unavailable"))
      return
    end
    local private_error = private_store(self, true)
    if private_error then
      finished(private_error)
      return
    end
    local sql = {
      ".timeout 1000",
      "PRAGMA journal_mode=DELETE;",
      "PRAGMA synchronous=EXTRA;",
      "PRAGMA foreign_keys=ON;",
      "PRAGMA trusted_schema=OFF;",
      "BEGIN IMMEDIATE;",
      "CREATE TEMP TABLE recording_guard(ok INTEGER NOT NULL CHECK(ok = 1));",
      "INSERT INTO recording_guard SELECT (CAST(sqlite_version() AS INTEGER) > 3 OR"
        .. " (CAST(sqlite_version() AS INTEGER) = 3 AND CAST(substr(sqlite_version(),3) AS INTEGER) >= 38)) AND json_valid('{}')"
        .. " AND (SELECT journal_mode = 'delete' FROM pragma_journal_mode)"
        .. " AND (SELECT synchronous = 3 FROM pragma_synchronous)"
        .. " AND (SELECT foreign_keys = 1 FROM pragma_foreign_keys)"
        .. " AND (SELECT user_version IN (0,1,2) FROM pragma_user_version);",
      SCHEMA,
      "PRAGMA user_version=2;",
    }
    for index = 1, count do
      sql[#sql + 1] = self.queue[index].sql
    end
    sql[#sql + 1] = "COMMIT;"
    local started, process_or_error = pcall(
      nvim.system,
      { "sqlite3", "-batch", "-bail", "-json", "-nofollow", "-init", "/dev/null", self.path },
      {
        cwd = self.directory,
        env = {},
        text = true,
        stdin = table.concat(sql, "\n"),
        timeout = 5000,
      },
      nvim.schedule_wrap(function(result)
        if result.code == 0 then
          finished(nil)
        else
          finished(sqlite_error(result.stderr or ""))
        end
      end)
    )
    if not started or process_or_error == nil then
      -- Spawn details may contain paths/environment; expose only a typed failure.
      finished(failure("unavailable"))
    end
  end)
end

---Construct a private asynchronous recorder; construction performs no I/O.
---@param directory string Absolute private directory; database is turns.sqlite3.
---@param changed fun(error: louiselm.session.RecordingError?, pending: boolean) Called on the main loop.
---@return louiselm.session.RecordingStore? store
---@return louiselm.session.RecordingError? error
function M.new(directory, changed)
  if not nonempty(directory) or nvim.fs.abspath(directory) ~= directory then
    return nil, failure("invalid")
  end
  return setmetatable({
    directory = directory,
    path = nvim.fs.joinpath(directory, "turns.sqlite3"),
    queue = {},
    busy = false,
    changed = changed,
  }, Store),
    nil
end

---Queue an immutable normalized fact. Callback fires once on the main loop.
---Failure retains the encoded write for flush/retry; identical retries are idempotent.
---Extra caller fields are omitted, never persisted. No raw ACP payload is accepted.
---@param self louiselm.session.RecordingStore
---@param record louiselm.session.PreparedTurn|louiselm.session.TurnObservation|louiselm.session.OptionTransition
---@param callback? louiselm.session.RecordingCallback
function Store:append(record, callback)
  local ok, sql = pcall(record_sql, record)
  local invalid
  if not ok or sql == nil then
    sql = ""
    invalid = failure("invalid")
  end
  self.queue[#self.queue + 1] = { sql = sql, error = invalid, callback = callback }
  -- Active work may keep appending after a failure; only explicit flush retries it.
  if self.error == nil then
    drain(self)
  end
end

---Retry pending immutable writes and acknowledge the barrier asynchronously.
---Later writes are ordered after this barrier. No write is dropped after a failed ACK.
---@param self louiselm.session.RecordingStore
---@param callback louiselm.session.RecordingCallback
function Store:flush(callback)
  self.queue[#self.queue + 1] = { sql = "", callback = callback }
  drain(self)
end

---@param self louiselm.session.RecordingStore
---@param query string
---@param callback fun(rows: table[]?, error?: louiselm.session.RecordingError)
local function read(self, query, callback)
  nvim.schedule(function()
    local stat, _, code = nvim.uv.fs_lstat(self.path)
    if stat == nil then
      if code == "ENOENT" then
        callback({})
      else
        callback(nil, failure("storage"))
      end
      return
    end
    local err = private_store(self, false)
    if err ~= nil then
      callback(nil, err)
      return
    end
    local sql = [[
.timeout 1000
PRAGMA trusted_schema=OFF;
PRAGMA foreign_keys=ON;
PRAGMA synchronous=EXTRA;
BEGIN;
CREATE TEMP TABLE history_guard(ok INTEGER NOT NULL CHECK(ok=1));
INSERT INTO history_guard SELECT
  (CAST(sqlite_version() AS INTEGER)>3 OR
    (CAST(sqlite_version() AS INTEGER)=3 AND CAST(substr(sqlite_version(),3) AS INTEGER)>=38))
  AND json_valid('{}')
  AND (SELECT user_version IN (1,2) FROM pragma_user_version)
  AND (SELECT journal_mode='delete' FROM pragma_journal_mode)
  AND (SELECT synchronous=3 FROM pragma_synchronous)
  AND (SELECT foreign_keys=1 FROM pragma_foreign_keys);
]] .. query .. [[
COMMIT;
]]
    local started, process = pcall(
      nvim.system,
      { "sqlite3", "-readonly", "-batch", "-bail", "-json", "-nofollow", "-init", "/dev/null", self.path },
      { cwd = self.directory, env = {}, text = true, stdin = sql, timeout = 5000 },
      nvim.schedule_wrap(function(result)
        if result.code ~= 0 then
          callback(nil, sqlite_error(result.stderr or ""))
          return
        end
        local ok, records =
          pcall(nvim.json.decode, result.stdout ~= "" and result.stdout or "[]", { luanil = { object = true } })
        if not ok or type(records) ~= "table" then
          callback(nil, failure("corrupt"))
          return
        end
        callback(records)
      end)
    )
    if not started or process == nil then
      callback(nil, failure("unavailable"))
    end
  end)
end

---Read committed presentation associations for exactly one Agent/ACP Session.
---Does not flush, create, migrate, or write history. Missing stores return an empty
---list; old dispatches without an observed ordinal remain unassociated. Results
---include unmeasured turns so callers cannot substitute legacy measurements.
---@param self louiselm.session.RecordingStore
---@param agent string Configured Agent.
---@param acp_session_id string Agent-side conversation identity.
---@param callback louiselm.session.UsageHistoryCallback Called once on the main loop, including errors.
function Store:usage_history(agent, acp_session_id, callback)
  if not nonempty(agent) or not nonempty(acp_session_id) then
    nvim.schedule(function()
      callback(nil, failure("invalid"))
    end)
    return
  end
  read(self, [[
SELECT t.id, json_extract(d.data,'$.transcript_turn') AS turn,
       json_extract(o.data,'$.usage') AS usage
FROM turns t JOIN turn_events d ON d.turn_id=t.id AND d.kind='dispatch'
LEFT JOIN turn_events o ON o.turn_id=t.id AND o.kind='outcome'
WHERE t.agent=]] .. sql_text(agent) .. " AND t.acp_session_id=" .. sql_text(acp_session_id) .. [[
 AND json_type(d.data,'$.transcript_turn')='integer'
 AND json_extract(d.data,'$.transcript_turn')>0
ORDER BY turn,t.id;
]], function(records, err)
    if records == nil then
      callback(nil, err)
      return
    end
    for _, record in ipairs(records) do
      if record.usage ~= nil then
        local decoded, usage = pcall(nvim.json.decode, record.usage)
        if not decoded or type(usage) ~= "table" then
          callback(nil, failure("corrupt"))
          return
        end
        for field, value in pairs(usage) do
          if not TOKEN_FIELDS[field] or not finite(value) or value % 1 ~= 0 then
            callback(nil, failure("corrupt"))
            return
          end
        end
        record.usage = usage
      end
    end
    callback(records)
  end)
end

---Query exact candidate cohorts together in one read-only SQLite snapshot.
---SQL owns all counts and means. Excludes unsent attempts and option-changing
---turns; absent telemetry stays absent. Missing stores yield empty summaries.
---Does not flush or write; callers needing pending observations must flush first.
---@param self louiselm.session.RecordingStore
---@param cohorts louiselm.session.UsageCohort[] Closed filters; Agent, Provider and complete typed options only.
---@param callback louiselm.session.UsageSummariesCallback Called once on the main loop, including validation/storage failures.
function Store:usage_summaries(cohorts, callback)
  local values, summaries = {}, {}
  local valid = type(cohorts) == "table" and nvim.islist(cohorts) and #cohorts > 0
  for index, cohort in ipairs(valid and cohorts or {}) do
    if
      type(cohort) ~= "table"
      or nvim.tbl_count(cohort) ~= 3
      or not nonempty(cohort.agent)
      or not nonempty(cohort.provider)
      or not tuple_valid(cohort.options)
    then
      valid = false
      break
    end
    values[#values + 1] = "("
      .. index
      .. ","
      .. sql_text(cohort.agent)
      .. ","
      .. sql_text(cohort.provider)
      .. ","
      .. sql_text(json(cohort.options))
      .. ")"
    summaries[index] = { turns = 0, outcomes = {}, tokens = {}, costs = {} }
  end
  if not valid then
    nvim.schedule(function()
      callback(nil, failure("invalid"))
    end)
    return
  end
  read(self, [[
CREATE TEMP TABLE cohort_turns AS
WITH candidates(candidate,agent,provider,options) AS (VALUES ]] .. table.concat(values, ",") .. [[)
SELECT c.candidate,t.*,o.data AS outcome_data
FROM candidates c JOIN turns t ON t.agent=c.agent AND t.provider=c.provider AND t.options=c.options
LEFT JOIN turn_events o ON o.turn_id=t.id AND o.kind='outcome'
WHERE EXISTS (SELECT 1 FROM turn_events d WHERE d.turn_id=t.id AND d.kind='dispatch')
AND NOT EXISTS (SELECT 1 FROM option_events e WHERE e.turn_id=t.id);

-- Refuse malformed consumed telemetry rather than letting SQLite coerce it.
INSERT INTO history_guard SELECT NOT EXISTS (
 SELECT 1 FROM cohort_turns t WHERE t.outcome_data IS NOT NULL AND (
  json_type(t.outcome_data) IS NOT 'object'
  OR json_extract(t.outcome_data,'$.outcome') NOT IN ('completed','cancelled','failed','disposed','not_sent')
  OR json_type(t.outcome_data,'$.outcome') IS NOT 'text'
  OR json_type(t.outcome_data,'$.peer_response') NOT IN ('true','false')
  OR json_type(t.outcome_data,'$.peer_response') IS NULL
  OR (json_type(t.outcome_data,'$.usage') IS NOT NULL AND json_type(t.outcome_data,'$.usage') IS NOT 'object')
  OR EXISTS (SELECT 1 FROM json_each(t.outcome_data,'$.usage') u WHERE
   u.key NOT IN ('total_tokens','input_tokens','output_tokens','thought_tokens','cached_read_tokens','cached_write_tokens')
   OR u.type IS NOT 'integer' OR u.value<0)
 ));
CREATE TEMP TABLE cost_readings AS
SELECT candidate,id,0 AS sequence,cost_baseline AS cost FROM cohort_turns
UNION ALL
SELECT t.candidate,t.id,e.sequence,json_extract(e.data,'$.cost')
FROM cohort_turns t JOIN turn_events e ON e.turn_id=t.id AND e.kind='cost';
INSERT INTO history_guard SELECT NOT EXISTS (
 SELECT 1 FROM cost_readings WHERE cost IS NOT NULL AND json_type(cost) IS NOT 'null' AND (
  json_type(cost) IS NOT 'object' OR (SELECT count(*) FROM json_each(cost))<>2
  OR json_type(cost,'$.amount') NOT IN ('integer','real') OR json_type(cost,'$.amount') IS NULL
  OR json_extract(cost,'$.amount')<0 OR json_extract(cost,'$.amount')>=1e999
  OR json_type(cost,'$.currency') IS NOT 'text'
  OR json_extract(cost,'$.currency') NOT GLOB '[A-Z][A-Z][A-Z]'
 ));
WITH ordered_costs AS (
 SELECT *,json_extract(cost,'$.amount') AS amount,json_extract(cost,'$.currency') AS currency,
 lag(json_extract(cost,'$.amount')) OVER (PARTITION BY candidate,id ORDER BY sequence) AS previous,
 lag(json_extract(cost,'$.currency')) OVER (PARTITION BY candidate,id ORDER BY sequence) AS previous_currency
 FROM cost_readings
), deltas AS (
 SELECT candidate,id,min(currency) AS currency,max(amount)-min(amount) AS amount
 FROM ordered_costs GROUP BY candidate,id
 HAVING count(*)>1 AND count(amount)=count(*)
 AND sum(CASE WHEN sequence>0 AND (previous IS NULL OR amount<previous OR currency IS NOT previous_currency) THEN 1 ELSE 0 END)=0
), metrics AS (
 SELECT candidate,'turns' AS kind,'' AS name,count(*) AS samples,NULL AS average FROM cohort_turns GROUP BY candidate
 UNION ALL
 SELECT candidate,'outcome',coalesce(json_extract(outcome_data,'$.outcome'),'unobserved'),count(*),NULL
 FROM cohort_turns GROUP BY candidate,coalesce(json_extract(outcome_data,'$.outcome'),'unobserved')
 UNION ALL
 SELECT t.candidate,'token',u.key,count(*),avg(u.value)
 FROM cohort_turns t,json_each(t.outcome_data,'$.usage') u GROUP BY t.candidate,u.key
 UNION ALL
 SELECT d.candidate,'cost',d.currency,count(*),avg(d.amount) FROM deltas d
 JOIN cohort_turns t ON t.candidate=d.candidate AND t.id=d.id
 WHERE json_extract(t.outcome_data,'$.peer_response')=1 GROUP BY d.candidate,d.currency
)
SELECT * FROM metrics ORDER BY candidate,kind,name;
]], function(rows, err)
    if rows == nil then
      callback(nil, err)
      return
    end
    for _, row in ipairs(rows) do
      local summary = summaries[row.candidate]
      if
        summary == nil
        or not finite(row.samples)
        or row.samples % 1 ~= 0
        or (row.kind ~= "turns" and row.kind ~= "outcome" and row.kind ~= "token" and row.kind ~= "cost")
        or ((row.kind == "token" or row.kind == "cost") and not finite(row.average))
      then
        callback(nil, failure("corrupt"))
        return
      end
      if row.kind == "turns" then
        summary.turns = row.samples
      elseif row.kind == "outcome" then
        summary.outcomes[row.name] = row.samples
      elseif row.kind == "token" then
        summary.tokens[row.name] = { samples = row.samples, average = row.average }
      else
        summary.costs[#summary.costs + 1] = { currency = row.name, samples = row.samples, average = row.average }
      end
    end
    callback(summaries)
  end)
end

return M
