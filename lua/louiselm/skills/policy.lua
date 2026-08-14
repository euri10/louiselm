---@alias louiselm.skills.Policy "native"|"inject"|"off"

local M = {}

local policies = {
  inject = true,
  native = true,
  off = true,
}

---Normalize one agent's skills policy.
---@param value? unknown `native`, `inject`, or `off`.
---@return louiselm.skills.Policy? policy Normalized policy, or nil when invalid.
---@return string? error_message Specific validation failure.
function M.normalize(value)
  if value == nil then
    return "native"
  end
  if type(value) ~= "string" or not policies[value] then
    return nil, "skills policy must be one of: inject, native, off"
  end
  return value
end

return M
