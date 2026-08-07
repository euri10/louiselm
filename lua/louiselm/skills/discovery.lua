---@class louiselm.skills.Skill
---@field name string Agent Skills name.
---@field description string Short description shown in the injected index.
---@field path string Absolute path to SKILL.md.

---@class louiselm.skills.DiscoveryError
---@field path string File or configured directory related to the error.
---@field message string Actionable discovery failure.

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

---@param value string
---@return string
local function absolute_path(value)
  local editor = nvim()
  return editor.fs.normalize(editor.fn.fnamemodify(editor.fn.expand(value), ":p"))
end

---@param value string
---@return string
local function canonical_path(value)
  local editor = nvim()
  local realpath = editor.uv.fs_realpath(value)
  if realpath ~= nil then
    return editor.fs.normalize(realpath)
  end
  return editor.fs.normalize(value)
end

---@param value string
---@return string
local function trim(value)
  return value:match("^%s*(.-)%s*$") or ""
end

---@param value string
---@return string
local function scalar(value)
  value = trim(value)
  local first = value:sub(1, 1)
  local last = value:sub(-1)
  if first == '"' and last == '"' and #value > 1 then
    local call_ok, decoded = pcall(nvim().json.decode, value)
    if call_ok and type(decoded) == "string" then
      return decoded
    end
  end
  if first == "'" and last == "'" and #value > 1 then
    local unquoted = value:sub(2, -2):gsub("''", "'")
    return unquoted
  end
  return value
end

---@param lines string[]
---@param path string
---@return louiselm.skills.Skill? skill
---@return string? error_message
local function parse(lines, path)
  if lines[1] ~= "---" then
    return nil, "missing YAML frontmatter"
  end

  local fields = {}
  local block_key
  local block_lines = {}
  local closed = false

  local function finish_block()
    if block_key ~= nil then
      fields[block_key] = table.concat(block_lines, " ")
      block_key = nil
      block_lines = {}
    end
  end

  for index = 2, #lines do
    local line = lines[index]
    if line == "---" then
      finish_block()
      closed = true
      break
    end

    local key, value = line:match("^([%w_-]+):[ \t]*(.*)$")
    if key ~= nil then
      finish_block()
      if value == ">" or value == ">-" or value == "|" or value == "|-" then
        block_key = key
      else
        fields[key] = scalar(value)
      end
    elseif block_key ~= nil and line:match("^%s+") then
      block_lines[#block_lines + 1] = trim(line)
    elseif trim(line) ~= "" then
      return nil, "malformed YAML frontmatter"
    end
  end

  if not closed then
    return nil, "unterminated YAML frontmatter"
  end
  if type(fields.name) ~= "string" or fields.name == "" then
    return nil, "frontmatter requires a non-empty name"
  end
  if not fields.name:match("^[a-z0-9][a-z0-9-]*$") then
    return nil, "skill name must contain only lowercase letters, numbers, and hyphens"
  end
  if type(fields.description) ~= "string" or trim(fields.description) == "" then
    return nil, "frontmatter requires a non-empty description"
  end

  return {
    name = fields.name,
    description = trim(fields.description),
    path = path,
  }
end

---@param path string
---@return string[]? lines
---@return string? error_message
local function read_file(path)
  local call_ok, lines_or_error = pcall(nvim().fn.readfile, path)
  if not call_ok then
    return nil, "could not read SKILL.md"
  end
  return lines_or_error
end

---@param paths unknown
---@return string[]? normalized
---@return louiselm.skills.DiscoveryError[] errors
local function normalize_paths(paths)
  local errors = {}
  if type(paths) ~= "table" or not is_dense_array(paths) then
    return nil, { { path = "skills.paths", message = "skill paths must be a dense array of strings" } }
  end

  local normalized = {}
  for index, path in ipairs(paths) do
    if type(path) ~= "string" or path == "" then
      errors[#errors + 1] = {
        path = string.format("skills.paths[%d]", index),
        message = "skill path must be a non-empty string",
      }
    else
      normalized[#normalized + 1] = absolute_path(path)
    end
  end
  return normalized, errors
end

---Discover Agent Skills metadata below configured directories.
---@param paths unknown Dense array of directories containing SKILL.md files.
---@return louiselm.skills.Skill[] skills Valid skills, sorted by name and path.
---@return louiselm.skills.DiscoveryError[] errors All invalid paths and metadata errors.
function M.discover(paths)
  local normalized_paths, errors = normalize_paths(paths)
  if normalized_paths == nil then
    return {}, errors
  end

  local editor = nvim()
  local files = {}
  local seen_files = {}
  for _, root in ipairs(normalized_paths) do
    local stat = editor.uv.fs_stat(root)
    if stat == nil or stat.type ~= "directory" then
      errors[#errors + 1] = { path = root, message = "skill path is not an existing directory" }
    else
      local found_ok, found_or_error = pcall(editor.fs.find, "SKILL.md", {
        path = root,
        type = "file",
        limit = math.huge,
      })
      if not found_ok then
        errors[#errors + 1] = { path = root, message = "could not scan skill path" }
      else
        for _, file in ipairs(found_or_error) do
          local absolute = absolute_path(file)
          local canonical = canonical_path(absolute)
          if not seen_files[canonical] then
            seen_files[canonical] = true
            files[#files + 1] = absolute
          end
        end
      end
    end
  end

  table.sort(files)
  local skills = {}
  local names = {}
  for _, path in ipairs(files) do
    local lines, read_error = read_file(path)
    if lines == nil then
      errors[#errors + 1] = { path = path, message = read_error or "could not read SKILL.md" }
    else
      local skill, parse_error = parse(lines, path)
      if skill == nil then
        errors[#errors + 1] = { path = path, message = parse_error or "invalid skill metadata" }
      elseif names[skill.name] ~= nil then
        errors[#errors + 1] = { path = path, message = "duplicate skill name '" .. skill.name .. "'" }
      else
        names[skill.name] = true
        skills[#skills + 1] = skill
      end
    end
  end

  table.sort(skills, function(left, right)
    if left.name == right.name then
      return left.path < right.path
    end
    return left.name < right.name
  end)
  table.sort(errors, function(left, right)
    if left.path == right.path then
      return left.message < right.message
    end
    return left.path < right.path
  end)
  return skills, errors
end

return M
