---Explicit replacement-policy negotiation for a failed ACP Session load.
local M = { key = "io.github.euri10.louiselm.resumePermissions" }

local function bounded_text(value, maximum)
  return type(value) == "string" and #value > 0 and #value <= maximum and not value:find("%c")
end

---Validate an explicit mode without interpreting Agent-specific identifiers.
---@param value unknown
---@return boolean valid
function M.valid_mode(value)
  return bounded_text(value, 128)
end

---Copy only a bounded, well-formed recovery offer; ordinary errors return nil.
---@param rpc_error unknown
---@return louiselm.session.ResumePermissionMode[]? modes
function M.from_error(rpc_error)
  if type(rpc_error) ~= "table" or rpc_error.code ~= -32600 then
    return nil
  end
  local data = rpc_error.data
  if type(data) ~= "table" or data.reason ~= "resume_permissions_unavailable" then
    return nil
  end
  local offer = data[M.key]
  if type(offer) ~= "table" or offer.version ~= 1 or type(offer.modes) ~= "table" then
    return nil
  end
  local count = #offer.modes
  if count < 1 or count > 16 then
    return nil
  end
  for index in pairs(offer.modes) do
    if type(index) ~= "number" or index % 1 ~= 0 or index < 1 or index > count then
      return nil
    end
  end
  local modes, seen = {}, {}
  for index = 1, count do
    local mode = offer.modes[index]
    if
      type(mode) ~= "table"
      or not M.valid_mode(mode.id)
      or seen[mode.id]
      or not bounded_text(mode.name, 256)
      or (mode.description ~= nil and not bounded_text(mode.description, 1024))
    then
      return nil
    end
    seen[mode.id] = true
    modes[#modes + 1] = { id = mode.id, name = mode.name, description = mode.description }
  end
  return modes
end

---Require the Agent to acknowledge the explicit replacement, never silently ignore it.
---@param result unknown
---@param mode string Requested replacement mode.
---@return boolean acknowledged
function M.acknowledged(result, mode)
  local meta = type(result) == "table" and result._meta
  local acknowledgement = type(meta) == "table" and meta[M.key]
  return type(acknowledgement) == "table" and acknowledgement.version == 1 and acknowledgement.modeId == mode
end

return M
