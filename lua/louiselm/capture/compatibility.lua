---Capture companion interface checks; package versions need not match the plugin.
local M = {}

---Validate bounded companion identity and the interface consumed by this client.
---@param metadata unknown Untrusted metadata from the connected daemon.
---@param interface string Required interface name.
---@return boolean compatible
---@return string? error_message Specific installed identity and update instruction.
function M.check(metadata, interface)
  local identity = "unidentified capture service"
  if
    type(metadata) == "table"
    and metadata.component == "capture"
    and type(metadata.version) == "string"
    and #metadata.version <= 32
    and metadata.version:match("^%d+%.%d+%.%d+$") ~= nil
  then
    identity = "louiselm-capture " .. metadata.version
    if type(metadata.interfaces) == "table" and metadata.interfaces[interface] == 1 then
      return true
    end
  end
  return false,
    identity
      .. ": incompatible "
      .. interface
      .. " interface; install a capture release supporting "
      .. interface
      .. " interface 1 (see docs/releases.md)"
end

---Explain an older CLI's refusal without changing other service diagnostics.
---@param message string Sanitized service error.
---@return string error_message Actionable update instruction or the original error.
function M.command_error(message)
  if message:find("unknown command '--require-interface=1'", 1, true) ~= nil then
    local _, err = M.check(nil, "capture")
    return err or message
  end
  return message
end

return M
