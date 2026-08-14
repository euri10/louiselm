local M = {}

---Return whether local Agent Skills parsing is available.
---@return boolean available True when lyaml can be loaded.
function M.available()
  local loaded = pcall(require, "lyaml")
  return loaded
end

return M
