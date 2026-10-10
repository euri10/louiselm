---Closed launcher document encoding; authorization remains owned by the broker.
local M = {}
---@diagnostic disable-next-line: undefined-global -- Neovim runtime API.
local nvim = vim

---@class louiselm.acp.LaunchRequest
---@field schema string
---@field protocol_version integer
---@field request_id string
---@field authorization_id string
---@field session_id string
---@field run_id string
---@field agent_id string
---@field envelope_id string
---@field envelope_revision integer
---@field skills louiselm.acp.SkillInputs
---@field session_input_manifest_id string

-- Serde struct order in skills-core/src/launch.rs is the canonical wire format.
local fields = {
  "schema",
  "protocol_version",
  "request_id",
  "authorization_id",
  "session_id",
  "run_id",
  "agent_id",
  "envelope_id",
  "envelope_revision",
  "skills",
  "session_input_manifest_id",
}

---@class louiselm.acp.SkillsOff
---@field mode "off"
---@field view_digest string
---@class louiselm.acp.SkillsEnabled
---@field mode "enabled"
---@field generation_digest string
---@field view_digest string
---@alias louiselm.acp.SkillInputs louiselm.acp.SkillsOff|louiselm.acp.SkillsEnabled

local function digest(value)
  return type(value) == "string" and #value == 71 and value:match("^sha256:[0-9a-f]+$") ~= nil
end

---Validate and encode an explicit skills binding in Rust's canonical field order.
---Rust verifies the exact empty-view identity; the trusted producer verifies witnessed supply.
---@param skills unknown
---@return string? bytes
---@return string? error_message
function M.encode_skills(skills)
  if type(skills) ~= "table" or not digest(skills.view_digest) then
    return nil, "invalid skills binding"
  end
  local fields
  if skills.mode == "off" then
    fields = { "mode", "view_digest" }
  elseif skills.mode == "enabled" and digest(skills.generation_digest) then
    fields = { "mode", "generation_digest", "view_digest" }
  else
    return nil, "invalid skills binding"
  end
  for key in pairs(skills) do
    if not nvim.tbl_contains(fields, key) then
      return nil, "unknown skills field: " .. tostring(key)
    end
  end
  local parts = {}
  for _, field in ipairs(fields) do
    parts[#parts + 1] = '"' .. field .. '":' .. nvim.json.encode(skills[field])
  end
  return "{" .. table.concat(parts, ",") .. "}"
end

---Encode a validated closed request in the installed launcher's canonical order.
---@param request unknown
---@return string? bytes No trailing newline; nil on malformed input.
---@return string? error_message
function M.encode(request)
  if type(request) ~= "table" then
    return nil, "launch request must be a table"
  end
  for key in pairs(request) do
    if not nvim.tbl_contains(fields, key) then
      return nil, "unknown launch request field: " .. tostring(key)
    end
  end
  local parts = {}
  for _, field in ipairs(fields) do
    local value = request[field]
    local valid
    local encoded
    if field == "schema" then
      valid = value == "louiselm.launch.request/3"
    elseif field == "protocol_version" then
      valid = value == 1
    elseif field == "envelope_revision" then
      valid = type(value) == "number" and value >= 1 and value <= 9007199254740991 and value % 1 == 0
    elseif field == "skills" then
      encoded = M.encode_skills(value)
      valid = encoded ~= nil
    elseif field == "session_input_manifest_id" then
      valid = digest(value)
    else
      valid = type(value) == "string" and #value <= 128 and value:match("^[%w_%-]+$") ~= nil
    end
    if not valid then
      return nil, "invalid launch request field: " .. field
    end
    parts[#parts + 1] = '"' .. field .. '":' .. (encoded or nvim.json.encode(value))
  end
  return "{" .. table.concat(parts, ",") .. "}"
end

return M
