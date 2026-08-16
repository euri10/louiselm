local M = {}
local Picker = require("louiselm.ui.picker")
local Skills = require("louiselm.skills")

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

---Build a slash-command context item for a discovered skill.
---@param skill louiselm.skills.Skill Skill metadata.
---@return louiselm.ui.ContextItem item Skill invocation context.
function M.context(skill)
  return { label = "skill: " .. skill.name, text = "/" .. skill.name }
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
  Picker.select(choices, {
    prompt = "louiselm skill: ",
    format_item = function(skill)
      return skill.name .. " — " .. skill.description
    end,
  }, function(choice)
    if choice == nil then
      callback(nil)
      return
    end
    local content = Skills.read(choice.path)
    if content == nil then
      callback(nil, "could not read selected skill: " .. choice.path)
      return
    end
    callback({
      name = choice.name,
      description = choice.description,
      path = choice.path,
      content = content,
      explicit_only = choice.explicit_only == true,
    })
  end)
  return true
end

return M
