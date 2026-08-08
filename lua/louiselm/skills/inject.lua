local M = {}

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

---@param value unknown
---@param full_content boolean
---@return louiselm.skills.Skill[]? skills
---@return string? error_message
local function validate_skills(value, full_content)
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
    if full_content and type(skill.content) ~= "string" then
      return nil, string.format("skill at index %d must contain full content", index)
    end
  end
  return value
end

---Build a skill prompt index, optionally including each full SKILL.md document.
---@param skills unknown Discovered skill metadata.
---@param full_content? boolean Include full content for agents without file-read tools.
---@return string? index Prompt text for an ACP text content item.
---@return string? error_message Validation failure.
function M.index(skills, full_content)
  if full_content ~= nil and type(full_content) ~= "boolean" then
    return nil, "full content option must be a boolean"
  end
  full_content = full_content == true
  local valid_skills, validation_error = validate_skills(skills, full_content)
  if valid_skills == nil then
    return nil, validation_error
  end

  local entries = {}
  for _, skill in ipairs(valid_skills) do
    entries[#entries + 1] = {
      name = skill.name,
      description = skill.description,
      path = skill.path,
    }
    if full_content then
      entries[#entries].content = skill.content
    end
  end
  table.sort(entries, function(left, right)
    if left.name == right.name then
      return left.path < right.path
    end
    return left.name < right.name
  end)

  local lines = { "<louiselm-skills>" }
  for _, entry in ipairs(entries) do
    lines[#lines + 1] = nvim().json.encode(entry)
  end
  lines[#lines + 1] = "</louiselm-skills>"
  return table.concat(lines, "\n")
end

return M
