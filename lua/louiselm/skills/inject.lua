---@class louiselm.skills.Catalog
---@field text string Complete hidden Agent Skills catalog, bounded to 8,000 UTF-8 bytes.
---@field truncated string[] Sorted skill names whose descriptions were shortened.
---@field omitted string[] Sorted tail skill names omitted because minimum metadata could not fit.

local M = {}

local MAX_BYTES = 8000
local PREAMBLE = table.concat({
  "<skills_instructions>",
  "Skills are sets of instructions stored in SKILL.md files. When a task matches a skill description below, read its SKILL.md from the listed location and follow it.",
  "</skills_instructions>",
}, "\n")

---@return table
local function nvim()
  ---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
  return vim
end

---@param value table
---@return boolean
local function is_dense_array(value)
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

---@param value string
---@return string
local function xml_escape(value)
  return (value:gsub("&", "&amp;"):gsub("<", "&lt;"):gsub(">", "&gt;"))
end

---@param value string
---@param index integer
---@return string? codepoint
---@return integer next_index
local function next_codepoint(value, index)
  local first = value:byte(index)
  if first == nil then
    return nil, index
  end
  local length = 1
  if first >= 0xC2 and first <= 0xDF then
    length = 2
  elseif first >= 0xE0 and first <= 0xEF then
    length = 3
  elseif first >= 0xF0 and first <= 0xF4 then
    length = 4
  end
  return value:sub(index, index + length - 1), index + length
end

---@param value unknown
---@return louiselm.skills.Skill[]? skills
---@return string? error_message
local function validate_skills(value)
  if type(value) ~= "table" or not is_dense_array(value) then
    return nil, "skills index must be a dense array"
  end
  for index, skill in ipairs(value) do
    if type(skill) ~= "table" then
      return nil, string.format("skill at index %d must be a table", index)
    end
    if type(skill.name) ~= "string" or type(skill.description) ~= "string" or type(skill.path) ~= "string" then
      return nil, string.format("skill at index %d must contain name, description, and path strings", index)
    end
    local absolute_path = nvim().fs.normalize(nvim().fn.fnamemodify(skill.path, ":p"))
    if nvim().fs.normalize(skill.path) ~= absolute_path then
      return nil, string.format("skill at index %d path must be absolute", index)
    end
  end
  return value
end

---@class louiselm.skills.CatalogEntry
---@field name string
---@field description string
---@field path string

---@param entries louiselm.skills.CatalogEntry[]
---@param descriptions string[]
---@param count integer
---@return string
local function render(entries, descriptions, count)
  local lines = { PREAMBLE, "<available_skills>" }
  for index = 1, count do
    local entry = entries[index]
    lines[#lines + 1] = "<skill>"
    lines[#lines + 1] = "<name>" .. xml_escape(entry.name) .. "</name>"
    lines[#lines + 1] = "<description>" .. descriptions[index] .. "</description>"
    lines[#lines + 1] = "<location>" .. xml_escape(entry.path) .. "</location>"
    lines[#lines + 1] = "</skill>"
  end
  lines[#lines + 1] = "</available_skills>"
  return table.concat(lines, "\n")
end

---Build a bounded hidden Agent Skills catalog.
---@param skills unknown Discovered skill metadata with absolute configured-alias paths.
---@return louiselm.skills.Catalog? catalog Catalog text and truncation diagnostics.
---@return string? error_message Validation failure.
function M.index(skills, ...)
  if select("#", ...) > 0 then
    return nil, 'full-content injection was removed; use skills.policy = "inject"'
  end
  local valid_skills, validation_error = validate_skills(skills)
  if valid_skills == nil then
    return nil, validation_error
  end

  local entries = {}
  for _, skill in ipairs(valid_skills) do
    if skill.explicit_only ~= true then
      entries[#entries + 1] = {
        name = skill.name,
        description = skill.description,
        path = skill.path,
      }
    end
  end
  table.sort(entries, function(left, right)
    if left.name == right.name then
      return left.path < right.path
    end
    return left.name < right.name
  end)

  local descriptions = {}
  for index = 1, #entries do
    descriptions[index] = ""
  end
  local included = #entries
  while included > 0 and #render(entries, descriptions, included) > MAX_BYTES do
    included = included - 1
  end

  local minimum = render(entries, descriptions, included)
  local remaining = MAX_BYTES - #minimum
  local positions = {}
  local pieces = {}
  for index = 1, included do
    positions[index] = 1
    pieces[index] = {}
  end
  while remaining > 0 do
    local progressed = false
    for index = 1, included do
      local codepoint, next_index = next_codepoint(entries[index].description, positions[index])
      if codepoint ~= nil then
        local escaped = xml_escape(codepoint)
        if #escaped <= remaining then
          pieces[index][#pieces[index] + 1] = escaped
          positions[index] = next_index
          remaining = remaining - #escaped
          progressed = true
        end
      end
    end
    if not progressed then
      break
    end
  end

  local truncated = {}
  for index = 1, included do
    descriptions[index] = table.concat(pieces[index])
    if positions[index] <= #entries[index].description then
      truncated[#truncated + 1] = entries[index].name
    end
  end
  local omitted = {}
  for index = included + 1, #entries do
    omitted[#omitted + 1] = entries[index].name
  end
  return {
    text = render(entries, descriptions, included),
    truncated = truncated,
    omitted = omitted,
  }
end

return M
