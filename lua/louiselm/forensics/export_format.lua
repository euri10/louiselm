local M = {}

local OBSERVATIONS = {
  agent = true,
  agent_version = true,
  cwd = true,
  model = true,
  options = true,
  capabilities = true,
  neovim_version = true,
  louiselm_version = true,
  git_commit = true,
  git_branch = true,
  dirty_files = true,
}
-- A structural export never attempts to recognize secrets by their spelling.
-- Only these fixed field names and protocol enums survive; other scalar values
-- and unknown keys are removed, including values nested below familiar keys.
local FIELDS = {
  jsonrpc = true,
  id = true,
  method = true,
  params = true,
  result = true,
  error = true,
  code = true,
  message = true,
  data = true,
  update = true,
  sessionUpdate = true,
  sessionId = true,
  content = true,
  type = true,
  text = true,
  role = true,
  status = true,
  direction = true,
  timestamp = true,
  payload = true,
  frame = true,
  load_session = true,
  list_sessions = true,
  embedded_context = true,
}
local ENUMS = {
  jsonrpc = { ["2.0"] = true },
  method = {
    initialize = true,
    ["session/new"] = true,
    ["session/load"] = true,
    ["session/prompt"] = true,
    ["session/cancel"] = true,
    ["session/update"] = true,
    ["session/request_permission"] = true,
  },
  sessionUpdate = {
    user_message_chunk = true,
    agent_message_chunk = true,
    agent_thought_chunk = true,
    tool_call = true,
    tool_call_update = true,
    plan = true,
    available_commands_update = true,
    current_mode_update = true,
  },
  type = { text = true, image = true, audio = true, resource = true, resource_link = true },
  role = { user = true, assistant = true, system = true, tool = true },
  status = { pending = true, in_progress = true, completed = true, failed = true },
}

---@class louiselm.forensics.ExportSelection
---@field selector string Validated operator selection.
---@field observation? string Selected observation field.
---@field source? integer Evidence source index.
---@field first? integer First JSONL line, inclusive.
---@field last? integer Last JSONL line, inclusive.

---Validate and detach a dense selection list; no implicit all-evidence selection.
---@param values unknown
---@return louiselm.forensics.ExportSelection[]? selections
---@return string? error_message
function M.selections(values)
  if type(values) ~= "table" or #values == 0 or #values > 16 then
    return nil, "select 1 to 16 observations or source ranges"
  end
  local result, seen, count, lines = {}, {}, 0, 0
  for key in pairs(values) do
    if type(key) ~= "number" or key % 1 ~= 0 or key < 1 or key > #values then
      return nil, "selections must be a dense list"
    end
    count = count + 1
  end
  if count ~= #values then
    return nil, "selections must be a dense list"
  end
  for _, selector in ipairs(values) do
    if type(selector) ~= "string" or #selector > 80 or seen[selector] then
      return nil, "selections must be distinct bounded strings"
    end
    seen[selector] = true
    local observation = selector:match("^observation:([a-z_]+)$")
    if observation and OBSERVATIONS[observation] then
      result[#result + 1] = { selector = selector, observation = observation }
    else
      local source, first, last = selector:match("^source:([1-9]%d*):([1-9]%d*):([1-9]%d*)$")
      local s, f, l = tonumber(source), tonumber(first), tonumber(last)
      if not s or not f or not l or s > 100 or l > 10000 or f > l then
        return nil, "use observation:FIELD or source:INDEX:FIRST:LAST (lines 1 to 10000)"
      end
      lines = lines + l - f + 1
      if lines > 200 then
        return nil, "select at most 200 JSONL lines in total"
      end
      result[#result + 1] = { selector = selector, source = s, first = f, last = l }
    end
  end
  return result
end

---Return a bounded structural projection with all non-allowlisted content redacted.
---Pure transformation: never mutates the input. Depth/width limits redact subtrees.
---@param value unknown Decoded JSON or selected observation.
---@return unknown redacted
function M.redact(value)
  local remaining = 2048
  local function visit(item, key, depth)
    remaining = remaining - 1
    if remaining < 0 or depth > 8 then
      return "[redacted:limit]"
    end
    if type(item) == "boolean" then
      return item
    end
    if type(item) == "string" and ENUMS[key] and ENUMS[key][item] then
      return item
    end
    if type(item) ~= "table" then
      return "[redacted]"
    end
    -- The JSON decoder marks empty objects with a metatable; preserve that shape.
    local result, width, omitted = setmetatable({}, getmetatable(item)), 0, 0
    for _ in pairs(item) do
      width = width + 1
      if width > 128 then
        return "[redacted:limit]"
      end
    end
    for child_key, child in pairs(item) do
      if type(child_key) == "number" or FIELDS[child_key] then
        result[child_key] = visit(child, child_key, depth + 1)
      else
        omitted = omitted + 1
      end
    end
    if omitted > 0 then
      result.redacted_fields = omitted
    end
    return result
  end
  local result = visit(value, "", 0)
  -- Refuse the whole value on node exhaustion, independent of table traversal.
  if remaining < 0 then
    return "[redacted:limit]"
  end
  return result
end

return M
