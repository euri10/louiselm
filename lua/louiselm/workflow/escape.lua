---Synthesize the human escape that every workflow stage owns.

local M = {}

M.PARK_OUTCOME = "__louiselm_park"

---@param value unknown
---@return unknown copy
local function copy(value)
  if type(value) ~= "table" then
    return value
  end
  local result = {}
  for key, item in pairs(value) do
    if type(item) == "table" then
      result[key] = copy(item)
    else
      result[key] = item
    end
  end
  return result
end

---Return a workflow manifest with an unremovable human Park outcome on every stage.
---The caller's manifest is never mutated. The reserved outcome name makes an attempted author
---override a Validation rejection instead of allowing the escape to be replaced silently.
---@param workflow string Workflow name.
---@param manifest table<string, table> Discovered stage manifest.
---@return table<string, table> synthesized
function M.apply(workflow, manifest)
  local synthesized = {}
  for name, block in pairs(manifest) do
    if type(block) == "table" and block.workflow == workflow then
      local stage = copy(block)
      local outcomes = {}
      if type(stage.outcomes) == "table" then
        for index, outcome in ipairs(stage.outcomes) do
          outcomes[index] = copy(outcome)
        end
      end
      outcomes[#outcomes + 1] = {
        name = M.PARK_OUTCOME,
        resolver = "human",
        terminal = true,
      }
      stage.outcomes = outcomes
      synthesized[name] = stage
    else
      synthesized[name] = block
    end
  end
  return synthesized
end

return M
