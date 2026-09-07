---@diagnostic disable-next-line: undefined-global -- Neovim owns process and filesystem effects.
local nvim = vim

---@alias louiselm.session.RecordingErrorCode "invalid"|"unavailable"|"permissions"|"locked"|"corrupt"|"conflict"|"storage"
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

---@alias louiselm.session.RecordingCallback fun(error?: louiselm.session.RecordingError)
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
---@field append fun(self: louiselm.session.RecordingStore, record: louiselm.session.PreparedTurn|louiselm.session.TurnObservation, callback?: louiselm.session.RecordingCallback)
---@field flush fun(self: louiselm.session.RecordingStore, callback: louiselm.session.RecordingCallback)

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

---@param record louiselm.session.PreparedTurn|louiselm.session.TurnObservation
---@return string?
local function record_sql(record)
  if type(record) ~= "table" then
    return nil
  end
  local columns, values, table_name, key, equal
  if record.id ~= nil then
    if
      not nonempty(record.id)
      or not nonempty(record.agent)
      or not nonempty(record.provider)
      or not nonempty(record.acp_session_id)
      or not nonempty(record.prepared_at)
      or type(record.options) ~= "table"
      or not cost_valid(record.cost_baseline)
      or (record.model ~= nil and type(record.model) ~= "string" and type(record.model) ~= "boolean")
    then
      return nil
    end
    for option, value in pairs(record.options) do
      if not nonempty(option) or (type(value) ~= "string" and type(value) ~= "boolean") then
        return nil
      end
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
      data = { request_id = data.request_id }
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
  if table_name == "turns" then
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
---@return louiselm.session.RecordingError?
local function private_store(self)
  local ok, result = pcall(nvim.fn.mkdir, self.directory, "p", 448)
  if not ok then
    return failure("permissions")
  end
  local stat = nvim.uv.fs_lstat(self.directory)
  local uid = nvim.uv.getuid()
  if result == 0 and stat == nil then
    return failure("permissions")
  end
  if stat == nil or stat.type ~= "directory" or stat.uid ~= uid or stat.mode % 512 ~= 448 then
    return failure("permissions")
  end
  local fd, _, code = nvim.uv.fs_open(self.path, "wx", 384)
  if fd ~= nil then
    if not nvim.uv.fs_close(fd) then
      return failure("storage")
    end
  elseif code ~= "EEXIST" then
    return failure("storage")
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
        local remaining = {}
        for index = count + 1, #self.queue do
          remaining[#remaining + 1] = self.queue[index]
        end
        self.queue = remaining
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
    local private_error = private_store(self)
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
        .. " AND (SELECT user_version IN (0,1) FROM pragma_user_version);",
      SCHEMA,
      "PRAGMA user_version=1;",
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
      function(result)
        nvim.schedule(function()
          if result.code == 0 then
            finished(nil)
          else
            finished(sqlite_error(result.stderr or ""))
          end
        end)
      end
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
---@param record louiselm.session.PreparedTurn|louiselm.session.TurnObservation
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

return M
