local M = {}

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

---@param text string
---@return string
local function normalize(text)
  return (text:gsub("%s+", " ")):match("^%s*(.-)%s*$")
end

---@param commands louiselm.session.AvailableCommand[]
---@param name string
---@return integer count
---@return louiselm.session.AvailableCommand? first
local function find(commands, name)
  local count = 0
  local first
  for _, command in ipairs(commands) do
    if command.name == name then
      count = count + 1
      if first == nil then
        first = command
      end
    end
  end
  return count, first
end

---@param description string
---@param skill louiselm.skills.Skill
---@param haystack string
---@param needle string
---@return boolean
local function starts_with(haystack, needle)
  return haystack:sub(1, #needle) == needle
end

---Real adapters annotate an advertised description with a trailing scope marker (Claude Code's
---ACP bridge appends " (user)" to every command sourced from a user-configured skill path), so a
---match only requires the advertised text to *start with* the local description, not equal it.
---@return boolean
local function description_matches(description, skill)
  local normalized = normalize(description)
  if starts_with(normalized, normalize(skill.description)) then
    return true
  end
  return skill.short_description ~= nil and starts_with(normalized, normalize(skill.short_description))
end

---Resolve one selected skill against the session's latest advertised commands.
---A unique `$name` (Codex-style) always wins, so a same-named built-in command can never steal
---a local skill. A bare `name` (Claude-style) only resolves when its advertised description
---agrees with the local skill's own description or OpenAI short description, since the name
---alone is not enough to trust an unrelated command. Any name with more than one advertised
---entry, dollar or bare, is ambiguous and never resolves; duplicates are not collapsed first.
---@param skill unknown Selected skill to invoke.
---@param commands unknown Latest agent-advertised commands, duplicates preserved.
---@return string? command_name Advertised command name to invoke, `$`-prefixed for Codex-style matches.
---@return string? error_message Why no command could be safely resolved, or a validation failure.
function M.resolve(skill, commands)
  if type(skill) ~= "table" or type(skill.name) ~= "string" or skill.name == "" then
    return nil, "skill must have a non-empty name"
  end
  if type(commands) ~= "table" or not is_dense_array(commands) then
    return nil, "advertised commands must be a dense array"
  end

  local dollar_name = "$" .. skill.name
  local dollar_count = find(commands, dollar_name)
  if dollar_count == 1 then
    return dollar_name
  end
  if dollar_count > 1 then
    return nil, "advertised command '" .. dollar_name .. "' is ambiguous"
  end

  local bare_count, bare_match = find(commands, skill.name)
  if bare_count == 0 then
    return nil, "no advertised command matches skill '" .. skill.name .. "'"
  end
  if bare_count > 1 then
    return nil, "advertised command '" .. skill.name .. "' is ambiguous"
  end
  ---@cast bare_match louiselm.session.AvailableCommand
  if not description_matches(bare_match.description, skill) then
    return nil, "advertised command '" .. skill.name .. "' description does not match the skill"
  end
  return skill.name
end

return M
