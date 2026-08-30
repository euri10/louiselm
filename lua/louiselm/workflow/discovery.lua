---Assemble one workflow manifest from discovered SKILL.md stage frontmatter.

local Skills = require("louiselm.skills")

local M = {}

---@param diagnostics louiselm.skills.DiscoveryDiagnostic[]
local function sort_diagnostics(diagnostics)
  table.sort(diagnostics, function(left, right)
    if left.path == right.path then
      return left.message < right.message
    end
    return left.path < right.path
  end)
end

---@param workflow unknown
---@param paths unknown
---@param cwd? string
---@return table<string, table> manifest
---@return louiselm.skills.DiscoveryDiagnostic[] diagnostics
function M.discover(workflow, paths, cwd)
  if type(workflow) ~= "string" or workflow == "" then
    return {}, { { path = "workflow", message = "workflow name must be a non-empty string" } }
  end

  local skills, diagnostics = Skills.discover(paths, cwd)
  local manifest = {}
  for _, skill in ipairs(skills) do
    local stage = skill.workflow
    if stage ~= nil then
      if type(stage.workflow) ~= "string" or stage.workflow == "" then
        diagnostics[#diagnostics + 1] = {
          path = skill.path,
          message = "workflow frontmatter must name a non-empty workflow",
        }
      elseif stage.workflow == workflow then
        manifest[skill.name] = stage
      end
    end
  end
  sort_diagnostics(diagnostics)
  return manifest, diagnostics
end

return M
