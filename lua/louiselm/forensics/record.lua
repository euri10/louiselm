---@class louiselm.forensics.Subject
---@field agent string Configured Agent name.
---@field acp_session_id string Agent-side ACP Session ID.

---@class louiselm.forensics.EvidenceSource
---@field kind string Known evidence source kind.
---@field state "present"|"absent"|"inaccessible"|"unsupported"|"omitted" Availability state.
---@field path? string Known source path, when present.
---@field mutable? boolean Whether the source can change after observation.
---@field reason? string Bounded reason for a non-present state.

---@class louiselm.forensics.Record
---@field schema_version integer Record schema version.
---@field id string Unique record identifier.
---@field observed_at integer Unix observation timestamp.
---@field subject louiselm.forensics.Subject Exactly one diagnosed Session.
---@field diagnosing_session? string Durable diagnosing Session identity.
---@field observations table<string, unknown> Fixed, sanitized observations.
---@field evidence_sources louiselm.forensics.EvidenceSource[] Known evidence pointers.

local M = {}

local VERSION = 1
local MAX_DIRTY_FILES = 100
local MAX_TEXT = 256

---@param value unknown
---@return unknown
local function copy(value)
  if type(value) ~= "table" then
    return value
  end
  local result = {}
  for key, item in pairs(value) do
    result[key] = copy(item)
  end
  return result
end

---@param value unknown
---@param limit integer
---@return string?
local function bounded_string(value, limit)
  if type(value) ~= "string" or value == "" then
    return nil
  end
  return #value > limit and value:sub(1, limit) or value
end

---@param value unknown
---@return boolean
local function is_string_map(value)
  if value == nil then
    return true
  end
  if type(value) ~= "table" then
    return false
  end
  for key, item in pairs(value) do
    if type(key) ~= "string" or (type(item) ~= "string" and type(item) ~= "boolean") then
      return false
    end
  end
  return true
end

---@param value unknown
---@return string[]
local function dirty_files(value)
  if type(value) ~= "table" then
    return {}
  end
  local result = {}
  for index, path in ipairs(value) do
    if index > MAX_DIRTY_FILES then
      break
    end
    local bounded = bounded_string(path, MAX_TEXT)
    if bounded ~= nil then
      result[#result + 1] = bounded
    end
  end
  return result
end

---@param value unknown
---@return louiselm.forensics.EvidenceSource[]?
local function evidence_sources(value)
  if value == nil then
    return {}
  end
  if type(value) ~= "table" then
    return nil
  end
  local result = {}
  for _, source in ipairs(value) do
    if type(source) ~= "table" or type(source.kind) ~= "string" or type(source.state) ~= "string" then
      return nil
    end
    if source.path ~= nil and bounded_string(source.path, MAX_TEXT) == nil then
      return nil
    end
    if source.reason ~= nil and bounded_string(source.reason, MAX_TEXT) == nil then
      return nil
    end
    if
      source.state ~= "present"
      and source.state ~= "absent"
      and source.state ~= "inaccessible"
      and source.state ~= "unsupported"
      and source.state ~= "omitted"
    then
      return nil
    end
    result[#result + 1] = {
      kind = source.kind,
      state = source.state,
      path = bounded_string(source.path, MAX_TEXT),
      mutable = source.mutable == true,
      reason = bounded_string(source.reason, MAX_TEXT),
    }
  end
  return result
end

---@param value unknown
---@return louiselm.forensics.Record? record
---@return string? error_message
function M.build(value)
  if type(value) ~= "table" then
    return nil, "forensics input must be a table"
  end
  if type(value.id) ~= "string" or value.id == "" then
    return nil, "forensics record requires an id"
  end
  local subject = value.subject
  if type(subject) ~= "table" then
    return nil, "forensics record requires one subject"
  end
  if type(subject.agent) ~= "string" or subject.agent == "" then
    return nil, "forensics subject requires an Agent"
  end
  if type(subject.acp_session_id) ~= "string" or subject.acp_session_id == "" then
    return nil, "forensics subject requires an ACP Session ID"
  end
  if type(value.observed_at) ~= "number" or value.observed_at % 1 ~= 0 then
    return nil, "forensics record requires an integer observation time"
  end
  local observations = value.observations
  if type(observations) ~= "table" then
    return nil, "forensics observations must be a table"
  end
  local sources = evidence_sources(value.evidence_sources)
  if sources == nil then
    return nil, "forensics evidence sources are malformed"
  end
  local record = {
    schema_version = VERSION,
    id = value.id,
    observed_at = value.observed_at,
    subject = { agent = subject.agent, acp_session_id = subject.acp_session_id },
    diagnosing_session = bounded_string(value.diagnosing_session, MAX_TEXT),
    observations = {
      agent = bounded_string(observations.agent, MAX_TEXT),
      agent_version = bounded_string(observations.agent_version, MAX_TEXT),
      cwd = bounded_string(observations.cwd, MAX_TEXT),
      model = bounded_string(observations.model, MAX_TEXT),
      options = is_string_map(observations.options) and copy(observations.options) or nil,
      capabilities = is_string_map(observations.capabilities) and copy(observations.capabilities) or nil,
      neovim_version = bounded_string(observations.neovim_version, MAX_TEXT),
      louiselm_version = bounded_string(observations.louiselm_version, MAX_TEXT),
      git_commit = bounded_string(observations.git_commit, MAX_TEXT),
      git_branch = bounded_string(observations.git_branch, MAX_TEXT),
      dirty_files = dirty_files(observations.dirty_files),
    },
    evidence_sources = sources,
  }
  if observations.options ~= nil and record.observations.options == nil then
    return nil, "forensics options are malformed"
  end
  if observations.capabilities ~= nil and record.observations.capabilities == nil then
    return nil, "forensics capabilities are malformed"
  end
  return record, nil
end

---@param record louiselm.forensics.Record
---@return louiselm.forensics.Record
function M.copy(record)
  return copy(record) --[[@as louiselm.forensics.Record]]
end

---@return integer
function M.version()
  return VERSION
end

return M
