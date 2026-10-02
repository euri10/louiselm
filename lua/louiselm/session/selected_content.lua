---Creation contract enforced by the maintained adapter, independent of permission mode.
local M = { key = "io.github.euri10.louiselm.selectedContent" }

---@class louiselm.session.SelectedContentLimits
---@field version integer Exactly 1.
---@field input_bytes integer 1..131072, complete encoded prompt bytes.
---@field output_bytes integer 1..65536, combined answer and thought bytes.
---@field max_tokens integer 1..8192, upstream output cap.
---@field timeout_ms integer 1..30000, local streaming deadline.

---Validate the closed, immutable single-request limits before starting a Session.
---@param value unknown
---@return boolean valid
function M.valid(value)
  if type(value) ~= "table" or value.version ~= 1 then
    return false
  end
  local maxima = { input_bytes = 131072, output_bytes = 65536, max_tokens = 8192, timeout_ms = 30000 }
  for key in pairs(value) do
    if key ~= "version" and maxima[key] == nil then
      return false
    end
  end
  for key, maximum in pairs(maxima) do
    local number = value[key]
    if type(number) ~= "number" or number % 1 ~= 0 or number < 1 or number > maximum then
      return false
    end
  end
  return true
end

return M
