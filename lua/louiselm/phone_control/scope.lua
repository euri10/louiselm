---Project and Agent scope checks shared by protected phone-control reads.
local M = {}

---Check an exact value against a bounded local grant list.
---@param values table Local grant entries.
---@param wanted string Value to match.
---@return boolean matched
function M.contains(values, wanted)
  if type(values) ~= "table" or #values > 50 then
    return false
  end
  for _, value in ipairs(values) do
    if type(value) == "string" and value == wanted then
      return true
    end
  end
  return false
end

---Check a canonical Session directory against approved project roots.
---@param projects table Approved canonical roots.
---@param cwd string Canonical Session directory.
---@return boolean matched
function M.within_project(projects, cwd)
  if type(projects) ~= "table" or #projects > 50 then
    return false
  end
  for _, root in ipairs(projects) do
    if type(root) == "string" and root:sub(1, 1) == "/" then
      if cwd == root or (root == "/" and cwd:sub(1, 1) == "/") or cwd:sub(1, #root + 1) == root .. "/" then
        return true
      end
    end
  end
  return false
end

return M
