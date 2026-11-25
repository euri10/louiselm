---Decide whether a completed readiness check may continue after input cleanup.
local M = {}

---@param removed boolean Whether the private input was removed.
---@param cleanup_error string? Filesystem failure, retained for the operator.
---@param disposed boolean Whether the controller was disposed meanwhile.
---@return boolean? proceed False settles a current check as a failure; nil suppresses a late callback.
---@return string? error_message Cleanup failure, including after disposal.
function M.after_cleanup(removed, cleanup_error, disposed)
  if not removed then
    local message = "Readiness input cleanup failed; paid dispatch refused: " .. tostring(cleanup_error)
    if disposed then
      return nil, message
    end
    return false, message
  end
  if disposed then
    return nil
  end
  return true
end

return M
