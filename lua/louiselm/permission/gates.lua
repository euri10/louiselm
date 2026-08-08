local M = {}

local function is_dense_string_array(value)
  if type(value) ~= "table" then
    return false
  end
  local length = 0
  while value[length + 1] ~= nil do
    if type(value[length + 1]) ~= "string" or value[length + 1] == "" then
      return false
    end
    length = length + 1
  end
  for key in pairs(value) do
    if type(key) ~= "number" or key < 1 or key % 1 ~= 0 or key > length then
      return false
    end
  end
  return true
end

local function validate_policy(policy)
  if type(policy) ~= "table" or type(policy.evaluate) ~= "function" then
    return nil, "permission policy must provide an evaluate function"
  end
  return true
end

local function evaluate(policy, request)
  local valid, policy_error = validate_policy(policy)
  if not valid then
    return nil, policy_error
  end
  local call_ok, decision, evaluation_error = pcall(policy.evaluate, policy, request)
  if not call_ok then
    return nil, tostring(decision)
  end
  if decision ~= "allow" and decision ~= "deny" and decision ~= "ask" then
    return nil, "permission policy returned an invalid decision"
  end
  return decision, evaluation_error
end

local function lower(value)
  return string.lower(tostring(value))
end

local function option_identifier(option)
  if type(option) == "string" then
    return option, lower(option)
  end
  if type(option) ~= "table" then
    return nil, ""
  end
  local identifier = option.optionId or option.option_id
  local label = option.kind or option.name or identifier
  if type(identifier) ~= "string" or identifier == "" then
    return nil, lower(label)
  end
  return identifier, lower(label)
end

---Extract a gate operation from an ACP permission payload.
---@param data table ACP permission request parameters.
---@return louiselm.permission.Request request Normalized operation; unknown operations remain askable.
function M.from_acp(data)
  local tool_call = data.toolCall or data.tool_call
  if type(tool_call) ~= "table" then
    return { kind = "unknown" }
  end
  local raw_input = tool_call.rawInput or tool_call.raw_input
  if type(raw_input) ~= "table" then
    raw_input = tool_call
  end
  local kind = lower(tool_call.kind)
  if kind == "edit" or kind == "file_edit" or kind == "write" then
    return {
      kind = "file_edit",
      path = raw_input.path or raw_input.filePath or raw_input.file_path,
      diff = raw_input.diff,
    }
  end
  if kind == "command" or kind == "execute" or kind == "shell" then
    local command = raw_input.command or raw_input.argv
    if type(command) == "string" then
      command = { command }
    end
    return { kind = "command", command = command }
  end
  return { kind = "unknown" }
end

---Build the standard ACP result for an automatic permission decision.
---@param data table ACP permission request parameters.
---@param decision louiselm.permission.Decision Automatic decision.
---@return table? result Result when a matching ACP option exists.
function M.response(data, decision)
  if decision == "ask" then
    return nil
  end
  if decision == "deny" and type(data.options) ~= "table" then
    return { outcome = { outcome = "cancelled" } }
  end
  if type(data.options) ~= "table" then
    return nil
  end
  local wanted = decision == "allow" and { "allow", "approve", "yes" } or { "deny", "reject", "no", "cancel" }
  for _, option in ipairs(data.options) do
    local identifier, label = option_identifier(option)
    if identifier ~= nil then
      for _, word in ipairs(wanted) do
        if label:find(word, 1, true) ~= nil then
          return { outcome = { outcome = "selected", optionId = identifier } }
        end
      end
    end
  end
  if decision == "deny" then
    return { outcome = { outcome = "cancelled" } }
  end
  return nil
end

---Evaluate a normalized permission request.
---@param policy louiselm.permission.Policy Policy to apply.
---@param request louiselm.permission.Request Operation to gate.
---@return louiselm.permission.Decision? decision Decision, or nil on invalid input.
---@return string? error_message Validation or policy error.
function M.check(policy, request)
  if type(request) ~= "table" then
    return nil, "permission request must be a table"
  end
  if request.kind ~= "file_edit" and request.kind ~= "command" and request.kind ~= "unknown" then
    return nil, "permission request kind must be file_edit, command, or unknown"
  end
  if request.kind == "file_edit" and (type(request.path) ~= "string" or request.path == "") then
    return nil, "permission file edit must have a non-empty path"
  end
  if request.kind == "command" and not is_dense_string_array(request.command) then
    return nil, "permission command must be a dense array of strings"
  end
  return evaluate(policy, request)
end

---Gate one file edit.
---@param policy louiselm.permission.Policy Policy to apply.
---@param path string File path.
---@param diff? string Proposed diff, if available.
---@return louiselm.permission.Decision? decision Decision, or nil on invalid input.
---@return string? error_message Validation or policy error.
function M.file_edit(policy, path, diff)
  if type(diff) ~= "nil" and type(diff) ~= "string" then
    return nil, "permission file edit diff must be a string"
  end
  return M.check(policy, { kind = "file_edit", path = path, diff = diff })
end

---Gate one command without invoking a shell.
---@param policy louiselm.permission.Policy Policy to apply.
---@param command unknown Command argv.
---@return louiselm.permission.Decision? decision Decision, or nil on invalid input.
---@return string? error_message Validation or policy error.
function M.command(policy, command)
  return M.check(policy, { kind = "command", command = command })
end

return M
