---@class louiselm.skills.MetadataResult
---@field skill louiselm.skills.Skill
---@field disable_model_invocation? boolean
---@field warnings string[]

---@class louiselm.skills.Yaml
---@field load fun(value: string): unknown

local Phase = require("louiselm.routing.phase")
local M = {}

local STANDARD_FIELDS = {
  name = true,
  description = true,
  license = true,
  compatibility = true,
  metadata = true,
  ["allowed-tools"] = true,
  phase = true,
  ["disable-model-invocation"] = true,
}

---@return table
local function nvim()
  ---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
  return vim
end

---@param value string
---@return string
local function trim(value)
  return value:match("^%s*(.-)%s*$") or ""
end

---@param value string
---@return integer
local function character_count(value)
  return nvim().str_utfindex(value)
end

---@param lines string[]
---@return string? frontmatter
---@return string? error_message
local function frontmatter(lines)
  if lines[1] ~= "---" then
    return nil, "missing YAML frontmatter"
  end
  for index = 2, #lines do
    if lines[index] == "---" then
      return table.concat(lines, "\n", 2, index - 1)
    end
  end
  return nil, "unterminated YAML frontmatter"
end

---@param yaml louiselm.skills.Yaml
---@param source string
---@return table? value
local function load_mapping(yaml, source)
  local call_ok, value = pcall(yaml.load, source)
  if not call_ok or type(value) ~= "table" then
    return nil
  end
  for key in pairs(value) do
    if type(key) ~= "string" then
      return nil
    end
  end
  return value
end

---@param fields table
---@return table<string, unknown>? stage
local function workflow_stage(fields)
  if fields.workflow == nil then
    return nil
  end
  local stage = {}
  for key, value in pairs(fields) do
    if not STANDARD_FIELDS[key] then
      stage[key] = value
    end
  end
  return stage
end

---@param value unknown
---@return boolean
local function string_map(value)
  if type(value) ~= "table" then
    return false
  end
  for key, item in pairs(value) do
    if type(key) ~= "string" or type(item) ~= "string" then
      return false
    end
  end
  return true
end

---@param fields table
---@param directory_name string
---@return string? error_message
local function validate_core(fields, directory_name)
  if type(fields.name) ~= "string" or fields.name == "" then
    return "frontmatter requires a non-empty name"
  end
  if #fields.name > 64 then
    return "frontmatter name must be at most 64 characters"
  end
  if not fields.name:match("^[a-z0-9-]+$") then
    return "skill name must contain only lowercase letters, numbers, and hyphens"
  end
  if fields.name:sub(1, 1) == "-" or fields.name:sub(-1) == "-" then
    return "skill name must not start or end with a hyphen"
  end
  if fields.name:find("--", 1, true) ~= nil then
    return "skill name must not contain consecutive hyphens"
  end
  if fields.name ~= directory_name then
    return "skill name must match its parent directory '" .. directory_name .. "'"
  end

  if type(fields.description) ~= "string" or trim(fields.description) == "" then
    return "frontmatter requires a non-empty description"
  end
  if character_count(fields.description) > 1024 then
    return "frontmatter description must be at most 1024 characters"
  end
  if fields.license ~= nil and type(fields.license) ~= "string" then
    return "frontmatter license must be a string"
  end
  if fields.compatibility ~= nil then
    if type(fields.compatibility) ~= "string" or trim(fields.compatibility) == "" then
      return "frontmatter compatibility must be a non-empty string"
    end
    if character_count(fields.compatibility) > 500 then
      return "frontmatter compatibility must be at most 500 characters"
    end
  end
  if fields.metadata ~= nil and not string_map(fields.metadata) then
    return "frontmatter metadata must map string keys to string values"
  end
  if fields["allowed-tools"] ~= nil and type(fields["allowed-tools"]) ~= "string" then
    return "frontmatter allowed-tools must be a string"
  end
  return nil
end

---Parse and semantically validate one complete SKILL.md.
---@param lines string[] File lines, including a final empty item when the file ends in a newline.
---@param path string Absolute SKILL.md path.
---@param content string Complete original file content.
---@param directory_name string Parent skill directory name.
---@param yaml louiselm.skills.Yaml Loaded lyaml module.
---@return louiselm.skills.MetadataResult? result
---@return string? error_message
function M.skill(lines, path, content, directory_name, yaml)
  local source, frontmatter_error = frontmatter(lines)
  if source == nil then
    return nil, frontmatter_error
  end
  local fields = load_mapping(yaml, source)
  if fields == nil then
    return nil, "invalid YAML frontmatter"
  end
  local validation_error = validate_core(fields, directory_name)
  if validation_error ~= nil then
    return nil, validation_error
  end
  local phase, phase_error = Phase.resolve(fields.phase, fields.name)
  if phase_error ~= nil then
    return nil, phase_error
  end

  local warnings = {}
  local disable_model_invocation = fields["disable-model-invocation"]
  local explicit_only = false
  if disable_model_invocation ~= nil and type(disable_model_invocation) ~= "boolean" then
    warnings[#warnings + 1] = "frontmatter disable-model-invocation must be a boolean; treating skill as explicit-only"
    disable_model_invocation = nil
    explicit_only = true
  elseif disable_model_invocation == true then
    explicit_only = true
  end

  local line_count = #lines
  if lines[line_count] == "" then
    line_count = line_count - 1
  end
  if line_count > 500 then
    warnings[#warnings + 1] = "SKILL.md exceeds 500 lines; move detailed material into referenced files"
  end

  return {
    skill = {
      name = fields.name,
      description = trim(fields.description),
      short_description = type(fields.metadata) == "table" and fields.metadata["short-description"] or nil,
      path = path,
      content = content,
      explicit_only = explicit_only,
      phase = phase,
      workflow = workflow_stage(fields),
    },
    disable_model_invocation = disable_model_invocation,
    warnings = warnings,
  }
end

---Parse the recognized invocation policy from agents/openai.yaml.
---@param content string Complete metadata file content.
---@param yaml louiselm.skills.Yaml Loaded lyaml module.
---@return boolean? allow_implicit_invocation
---@return string? error_message
function M.openai(content, yaml)
  local fields = load_mapping(yaml, content)
  if fields == nil then
    return nil, "invalid agents/openai.yaml; treating skill as explicit-only"
  end
  if fields.policy == nil then
    return nil
  end
  if type(fields.policy) ~= "table" then
    return nil, "agents/openai.yaml policy must be a mapping; treating skill as explicit-only"
  end
  local allow_implicit_invocation = fields.policy.allow_implicit_invocation
  if allow_implicit_invocation == nil then
    return nil
  end
  if type(allow_implicit_invocation) ~= "boolean" then
    return nil, "agents/openai.yaml policy.allow_implicit_invocation must be a boolean; treating skill as explicit-only"
  end
  return allow_implicit_invocation
end

return M
