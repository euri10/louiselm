---@class louiselm.routing.EvidenceTarget
---@field phase? louiselm.routing.PhaseName Phase the observation belongs to; absent means global.
---@field agent string Configured Agent name.
---@field model? string Model selected in the Agent's Session.
---@field options? table<string, string|boolean> Agent configuration options used for the observation.

---@class louiselm.routing.EvidenceRecord
---@field phase? louiselm.routing.PhaseName
---@field agent string
---@field model? string
---@field options? table<string, string|boolean>
---@field samples integer
---@field successes integer
---@field quality_total integer
---@field quality_samples integer
---@field notes? string[] Most recent feedback context, capped at five entries.
---@field updated_at integer Unix timestamp of the latest change.

---@class louiselm.routing.Evidence
---@field path string Persistent JSON path.
---@field observe fun(self: louiselm.routing.Evidence, target: unknown, outcome: unknown): boolean, string? Record one completed or failed turn.
---@field feedback fun(self: louiselm.routing.Evidence, target: unknown, rating: unknown, context?: unknown): boolean, string? Record optional human feedback.
---@field records fun(self: louiselm.routing.Evidence): louiselm.routing.EvidenceRecord[]?, string? Return detached persistent records.
---@field evidence fun(self: louiselm.routing.Evidence): louiselm.routing.RoutingEvidence[]?, string? Return records shaped for routing.

local Phase = require("louiselm.routing.phase")
local M = {}
local Evidence = {}
Evidence.__index = Evidence

local VERSION = 1
local MAX_NOTES = 5
local MAX_NOTE_LENGTH = 500

---@return table
local function nvim()
  ---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
  return vim
end

---@param value unknown
---@return boolean
local function is_options(value)
  if value == nil then
    return true
  end
  if type(value) ~= "table" then
    return false
  end
  for key, option in pairs(value) do
    if type(key) ~= "string" or (type(option) ~= "string" and type(option) ~= "boolean") then
      return false
    end
  end
  return true
end

---@param value? table<string, string|boolean>
---@return table<string, string|boolean>?
local function copy_options(value)
  if value == nil then
    return nil
  end
  local copy = {}
  for key, option in pairs(value) do
    copy[key] = option
  end
  return copy
end

---@param left? table<string, string|boolean>
---@param right? table<string, string|boolean>
---@return boolean
local function same_options(left, right)
  if left == nil or right == nil then
    return left == right
  end
  for key, value in pairs(left) do
    if right[key] ~= value then
      return false
    end
  end
  for key in pairs(right) do
    if left[key] == nil then
      return false
    end
  end
  return true
end

---@param value unknown
---@return louiselm.routing.EvidenceTarget? target
---@return string? error_message
local function normalize_target(value)
  if type(value) ~= "table" then
    return nil, "evidence target must be a table"
  end
  if type(value.agent) ~= "string" or value.agent == "" then
    return nil, "evidence requires an agent"
  end
  if value.phase ~= nil and not Phase.is_canonical(value.phase) then
    return nil,
      "unknown phase '"
        .. tostring(value.phase)
        .. "'; expected one of design, planning, implementation, review, qa, mechanical"
  end
  if value.model ~= nil and (type(value.model) ~= "string" or value.model == "") then
    return nil, "evidence model must be a non-empty string"
  end
  if not is_options(value.options) then
    return nil, "options must map option ids to strings or booleans"
  end
  return {
    phase = value.phase,
    agent = value.agent,
    model = value.model,
    options = copy_options(value.options),
  },
    nil
end

---@param record louiselm.routing.EvidenceRecord
---@return louiselm.routing.EvidenceRecord
local function copy_record(record)
  local copy = {
    phase = record.phase,
    agent = record.agent,
    model = record.model,
    options = copy_options(record.options),
    samples = record.samples,
    successes = record.successes,
    quality_total = record.quality_total,
    quality_samples = record.quality_samples,
    notes = record.notes and nvim().list_extend({}, record.notes) or nil,
    updated_at = record.updated_at,
  }
  return copy
end

---@param value unknown
---@return boolean
local function is_integer(value)
  return type(value) == "number" and value % 1 == 0
end

---@param value unknown
---@return louiselm.routing.EvidenceRecord?
local function validate_record(value)
  if type(value) ~= "table" then
    return nil
  end
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
  for key in pairs(value) do
    if not allowed[key] then
      return nil
    end
  end
  if
    type(value.agent) ~= "string"
    or value.agent == ""
    or (value.phase ~= nil and not Phase.is_canonical(value.phase))
    or (value.model ~= nil and (type(value.model) ~= "string" or value.model == ""))
    or not is_options(value.options)
    or not is_integer(value.samples)
    or value.samples < 0
    or not is_integer(value.successes)
    or value.successes < 0
    or value.successes > value.samples
    or not is_integer(value.quality_total)
    or value.quality_total < 0
    or not is_integer(value.quality_samples)
    or value.quality_samples < 0
    or value.quality_total > value.quality_samples
    or (value.notes ~= nil and type(value.notes) ~= "table")
    or not is_integer(value.updated_at)
  then
    return nil
  end
  if value.notes ~= nil then
    for index, note in ipairs(value.notes) do
      if type(note) ~= "string" or index > MAX_NOTES then
        return nil
      end
    end
    for key in pairs(value.notes) do
      if type(key) ~= "number" or key < 1 or key % 1 ~= 0 or key > #value.notes then
        return nil
      end
    end
  end
  return copy_record(value)
end

---@param path string
---@return louiselm.routing.EvidenceRecord[]? records
---@return string? error_message
local function read_records(path)
  local editor = nvim()
  local stat = editor.uv.fs_stat(path)
  if stat == nil then
    return {}, nil
  end
  if stat.type ~= "file" then
    return nil, "routing evidence path is not a regular file"
  end
  local file, open_error = editor.uv.fs_open(path, "r", 384)
  if file == nil then
    return nil, "could not read routing evidence: " .. tostring(open_error)
  end
  local content, read_error = editor.uv.fs_read(file, stat.size, 0)
  local closed, close_error = editor.uv.fs_close(file)
  if content == nil then
    return nil, "could not read routing evidence: " .. tostring(read_error)
  end
  if not closed then
    return nil, "could not close routing evidence: " .. tostring(close_error)
  end
  local decoded_ok, decoded = pcall(editor.json.decode, content)
  if not decoded_ok then
    return nil, "routing evidence is not valid JSON"
  end
  if type(decoded) ~= "table" then
    return nil, "routing evidence has invalid schema"
  end
  for key in pairs(decoded) do
    if key ~= "version" and key ~= "records" then
      return nil, "routing evidence has invalid schema"
    end
  end
  if decoded.version ~= VERSION or type(decoded.records) ~= "table" then
    return nil, "routing evidence has invalid schema"
  end
  local records = {}
  for index, value in ipairs(decoded.records) do
    local record = validate_record(value)
    if record == nil then
      return nil, "routing evidence has invalid schema"
    end
    records[index] = record
  end
  for key in pairs(decoded.records) do
    if type(key) ~= "number" or key < 1 or key % 1 ~= 0 or key > #records then
      return nil, "routing evidence has invalid schema"
    end
  end
  return records, nil
end

---@param path string
---@param records louiselm.routing.EvidenceRecord[]
---@return boolean written
---@return string? error_message
local function write_records(path, records)
  local editor = nvim()
  local directory = editor.fs.dirname(path)
  if editor.fn.mkdir(directory, "p", 448) == 0 and editor.fn.isdirectory(directory) ~= 1 then
    return false, "could not create routing evidence directory"
  end
  local encoded_ok, content = pcall(editor.json.encode, { version = VERSION, records = records })
  if not encoded_ok then
    return false, "could not encode routing evidence"
  end
  local file, temporary_or_error = editor.uv.fs_mkstemp(path .. ".tmp-XXXXXX")
  if file == nil then
    return false, "could not create temporary routing evidence: " .. tostring(temporary_or_error)
  end
  local temporary = temporary_or_error
  local written, write_error = editor.uv.fs_write(file, content, 0)
  if written ~= #content then
    editor.uv.fs_close(file)
    editor.uv.fs_unlink(temporary)
    return false, "could not write routing evidence: " .. tostring(write_error or "short write")
  end
  local synced, sync_error = editor.uv.fs_fsync(file)
  if not synced then
    editor.uv.fs_close(file)
    editor.uv.fs_unlink(temporary)
    return false, "could not sync routing evidence: " .. tostring(sync_error)
  end
  local closed, close_error = editor.uv.fs_close(file)
  if not closed then
    editor.uv.fs_unlink(temporary)
    return false, "could not close routing evidence: " .. tostring(close_error)
  end
  local renamed, rename_error = editor.uv.fs_rename(temporary, path)
  if not renamed then
    editor.uv.fs_unlink(temporary)
    return false, "could not replace routing evidence: " .. tostring(rename_error)
  end
  return true, nil
end

---@param left louiselm.routing.EvidenceRecord
---@param right louiselm.routing.EvidenceRecord
---@return boolean
local function before(left, right)
  local left_phase = left.phase or ""
  local right_phase = right.phase or ""
  if left_phase ~= right_phase then
    return left_phase < right_phase
  end
  if left.agent ~= right.agent then
    return left.agent < right.agent
  end
  local left_model = left.model or ""
  local right_model = right.model or ""
  if left_model ~= right_model then
    return left_model < right_model
  end
  local left_options = nvim().json.encode(left.options or {})
  return left_options < nvim().json.encode(right.options or {})
end

---@param records louiselm.routing.EvidenceRecord[]
---@param target louiselm.routing.EvidenceTarget
---@return louiselm.routing.EvidenceRecord? record
local function find_record(records, target)
  for _, record in ipairs(records) do
    if
      record.phase == target.phase
      and record.agent == target.agent
      and record.model == target.model
      and same_options(record.options, target.options)
    then
      return record
    end
  end
  return nil
end

---@param target louiselm.routing.EvidenceTarget
---@return louiselm.routing.EvidenceRecord
local function new_record(target)
  return {
    phase = target.phase,
    agent = target.agent,
    model = target.model,
    options = copy_options(target.options),
    samples = 0,
    successes = 0,
    quality_total = 0,
    quality_samples = 0,
    updated_at = os.time(),
  }
end

---@param target louiselm.routing.EvidenceTarget
---@return louiselm.routing.EvidenceTarget[] targets Phase-specific target followed by its global fallback.
local function target_variants(target)
  if target.phase == nil then
    return { target }
  end
  return {
    target,
    { agent = target.agent, model = target.model, options = copy_options(target.options) },
  }
end

---@param self louiselm.routing.Evidence
---@param target unknown
---@param outcome unknown
---@return boolean ok
---@return string? error_message
function Evidence:observe(target, outcome)
  local normalized, target_error = normalize_target(target)
  if normalized == nil then
    return false, target_error
  end
  if outcome ~= "completed" and outcome ~= "failed" and outcome ~= "cancelled" then
    return false, "outcome must be completed, failed, or cancelled"
  end
  if outcome == "cancelled" then
    return true, nil
  end
  local records, read_error = read_records(self.path)
  if records == nil then
    return false, read_error
  end
  for _, variant in ipairs(target_variants(normalized)) do
    local record = find_record(records, variant)
    if record == nil then
      record = new_record(variant)
      records[#records + 1] = record
    end
    record.samples = record.samples + 1
    if outcome == "completed" then
      record.successes = record.successes + 1
    end
    record.updated_at = os.time()
  end
  table.sort(records, before)
  return write_records(self.path, records)
end

---@param self louiselm.routing.Evidence
---@param target unknown
---@param rating unknown
---@param context? unknown
---@return boolean ok
---@return string? error_message
function Evidence:feedback(target, rating, context)
  local normalized, target_error = normalize_target(target)
  if normalized == nil then
    return false, target_error
  end
  if rating ~= "good" and rating ~= "skip" and rating ~= "poor" then
    return false, "rating must be good, skip, or poor"
  end
  if rating == "poor" and (type(context) ~= "string" or nvim().trim(context) == "") then
    return false, "poor feedback requires context"
  end
  local note_context = type(context) == "string" and context or ""
  if rating == "skip" then
    return true, nil
  end
  local records, read_error = read_records(self.path)
  if records == nil then
    return false, read_error
  end
  for _, variant in ipairs(target_variants(normalized)) do
    local record = find_record(records, variant)
    if record == nil then
      record = new_record(variant)
      records[#records + 1] = record
    end
    record.quality_samples = record.quality_samples + 1
    if rating == "good" then
      record.quality_total = record.quality_total + 1
    else
      record.notes = record.notes or {}
      record.notes[#record.notes + 1] = note_context:sub(1, MAX_NOTE_LENGTH)
      while #record.notes > MAX_NOTES do
        table.remove(record.notes, 1)
      end
    end
    record.updated_at = os.time()
  end
  table.sort(records, before)
  return write_records(self.path, records)
end

---Create a persistent routing evidence store.
---@param path string JSON path used for local evidence.
---@return louiselm.routing.Evidence? store
---@return string? error_message
function M.new(path)
  if type(path) ~= "string" or path == "" then
    return nil, "routing evidence path must be a non-empty string"
  end
  return setmetatable({ path = nvim().fs.normalize(path) }, Evidence), nil
end

---Return detached records, including operational counters and feedback notes.
---@param self louiselm.routing.Evidence
---@return louiselm.routing.EvidenceRecord[]? records
---@return string? error_message
function Evidence:records()
  local records, read_error = read_records(self.path)
  if records == nil then
    return nil, read_error
  end
  table.sort(records, before)
  local copies = {}
  for index, record in ipairs(records) do
    copies[index] = copy_record(record)
  end
  return copies, nil
end

---Return evidence in the flat shape consumed by workflow routing.
---@param self louiselm.routing.Evidence
---@return louiselm.routing.RoutingEvidence[]? evidence
---@return string? error_message
function Evidence:evidence()
  local records, read_error = read_records(self.path)
  if records == nil then
    return nil, read_error
  end
  table.sort(records, before)
  local evidence = {}
  for _, record in ipairs(records) do
    local value = {
      phase = record.phase,
      agent = record.agent,
      model = record.model,
      samples = record.samples,
      reliability = record.samples == 0 and 0 or record.successes / record.samples,
    }
    if record.quality_samples > 0 then
      value.quality = record.quality_total / record.quality_samples
    end
    evidence[#evidence + 1] = value
  end
  return evidence, nil
end

return M
