---@alias louiselm.skills.Policy "native"|"inject"|"off"

local M = {}

local policies = {
  inject = true,
  native = true,
  off = true,
}

---Normalize one agent's skills policy.
---@param value? unknown `native`, `inject`, or `off`.
---@param native_supported? boolean Whether the agent can load skills natively.
---@return louiselm.skills.Policy? policy Normalized policy, or nil when invalid.
---@return string? error_message Specific validation failure.
function M.normalize(value, native_supported)
  if native_supported ~= nil and type(native_supported) ~= "boolean" then
    return nil, "native skill support must be a boolean"
  end
  if value == nil then
    return native_supported == false and "inject" or "native"
  end
  if type(value) ~= "string" or not policies[value] then
    return nil, "skills policy must be one of: inject, native, off"
  end
  return value
end

return M
