local M = {}
local Picker = require("louiselm.ui.picker")

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

---@param value unknown
---@return boolean
local function is_dense_array(value)
  if type(value) ~= "table" then
    return false
  end
  local length = 0
  while value[length + 1] ~= nil do
    length = length + 1
  end
  for key in pairs(value) do
    if type(key) ~= "number" or key < 1 or key % 1 ~= 0 or key > length then
      return false
    end
  end
  return true
end

---@param skill unknown
---@return boolean
local function valid_skill(skill)
  return type(skill) == "table"
    and type(skill.name) == "string"
    and skill.name ~= ""
    and type(skill.description) == "string"
    and type(skill.path) == "string"
end

---Build an exact text context from a selected skill body.
---@param skill louiselm.skills.Skill Skill metadata with content captured at selection.
---@return louiselm.ui.ContextItem item Skill instruction context.
function M.context(skill)
  return { label = "skill: " .. skill.name, text = skill.content }
end

---Pick a discovered skill through Neovim's configured UI picker.
---@param skills unknown Discovered skill metadata.
---@param callback fun(skill: louiselm.skills.Skill?, error_message?: string) Called once with the selection.
---@return boolean started True when the picker was opened.
---@return string? error_message Validation error.
function M.pick(skills, callback)
  if not is_dense_array(skills) then
    return false, "skill picker requires a dense skill array"
  end
  if type(callback) ~= "function" then
    return false, "skill picker callback must be a function"
  end
  local choices = {}
  for index, skill in ipairs(skills) do
    if not valid_skill(skill) then
      return false, string.format("skill at index %d is malformed", index)
    end
    choices[index] = skill
  end
  table.sort(choices, function(left, right)
    return left.name < right.name
  end)
  -- `kind` and `file` are the stable vim.ui.select extension points a picker
  -- provider (e.g. Snacks' `picker.sources.select.kinds.louiselm_skill`) can
  -- opt into for a SKILL.md preview; louiselm never detects the provider.
  local items = {}
  for index, skill in ipairs(choices) do
    items[index] = nvim.tbl_extend("force", {}, skill, { file = skill.path })
  end
  Picker.select(items, {
    prompt = "louiselm skill: ",
    kind = "louiselm_skill",
    format_item = function(skill)
      return skill.name .. " — " .. skill.description
    end,
  }, function(choice)
    if choice == nil then
      callback(nil)
      return
    end
    callback(choice)
  end)
  return true
end

return M
