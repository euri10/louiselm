---@class louiselm.skills.Skill
---@field name string Agent Skills name.
---@field description string Short description shown in the injected index.
---@field path string Absolute path to SKILL.md.
---@field content string Complete original SKILL.md content for deliberate activation.
---@field explicit_only boolean Whether only deliberate picker activation may select the skill.

---@class louiselm.skills.DiscoveryDiagnostic
---@field path string File or configured directory related to the error.
---@field message string Actionable discovery failure.
---@field severity? "warning" Omitted for errors.
---@field code? "missing_dependency" Machine-readable code for failures callers must distinguish.

local Metadata = require("louiselm.skills.metadata")
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

---@param path string
---@return string[]? lines
---@return string? content
local function read_file(path)
  local call_ok, lines = pcall(nvim().fn.readfile, path, "b")
  if not call_ok or type(lines) ~= "table" then
    return nil, nil
  end
  return lines, table.concat(lines, "\n")
end

---@return louiselm.skills.Yaml? yaml
local function load_yaml()
  local call_ok, module = pcall(require, "lyaml")
  if not call_ok or type(module) ~= "table" or type(module.load) ~= "function" then
    return nil
  end
  return module
end

---@param paths unknown
---@return string[]? normalized
---@return louiselm.skills.DiscoveryDiagnostic[] errors
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

---@param diagnostics louiselm.skills.DiscoveryDiagnostic[]
local function sort_diagnostics(diagnostics)
  table.sort(diagnostics, function(left, right)
    if left.path == right.path then
      return left.message < right.message
    end
    return left.path < right.path
  end)
end

---@param diagnostics louiselm.skills.DiscoveryDiagnostic[]
---@param path string
---@param message string
local function add_warning(diagnostics, path, message)
  diagnostics[#diagnostics + 1] = { path = path, message = message, severity = "warning" }
end

---Discover Agent Skills metadata below configured directories.
---@param paths unknown Dense array of directories containing SKILL.md files.
---@return louiselm.skills.Skill[] skills Valid skills, sorted by name and path.
---@return louiselm.skills.DiscoveryDiagnostic[] diagnostics All invalid paths, metadata errors, and warnings.
function M.discover(paths)
  local normalized_paths, diagnostics = normalize_paths(paths)
  if normalized_paths == nil then
    return {}, diagnostics
  end

  local editor = nvim()
  local files = {}
  local seen_files = {}
  for _, root in ipairs(normalized_paths) do
    local stat = editor.uv.fs_stat(root)
    if stat == nil or stat.type ~= "directory" then
      diagnostics[#diagnostics + 1] = { path = root, message = "skill path is not an existing directory" }
    else
      local found_ok, found_or_error = pcall(editor.fs.find, "SKILL.md", {
        path = root,
        limit = math.huge,
      })
      if not found_ok then
        diagnostics[#diagnostics + 1] = { path = root, message = "could not scan skill path" }
      else
        for _, file in ipairs(found_or_error) do
          -- vim.fs.find's file filter excludes symlinks before resolving their targets.
          local stat = editor.uv.fs_stat(file)
          if stat ~= nil and stat.type == "file" then
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
  end

  table.sort(files)
  if #files == 0 then
    sort_diagnostics(diagnostics)
    return {}, diagnostics
  end
  local yaml = load_yaml()
  if yaml == nil then
    diagnostics[#diagnostics + 1] = {
      path = "skills",
      message = "lyaml is required for local skill discovery; install it with `luarocks --lua-version 5.1 install lyaml`",
      code = "missing_dependency",
    }
    sort_diagnostics(diagnostics)
    return {}, diagnostics
  end

  local skills = {}
  local names = {}
  for _, path in ipairs(files) do
    local lines, content = read_file(path)
    if lines == nil or content == nil then
      diagnostics[#diagnostics + 1] = { path = path, message = "could not read SKILL.md" }
    else
      local directory_name = editor.fs.basename(editor.fs.dirname(path))
      local result, parse_error = Metadata.skill(lines, path, content, directory_name, yaml)
      if result == nil then
        diagnostics[#diagnostics + 1] = { path = path, message = parse_error or "invalid skill metadata" }
      elseif names[result.skill.name] ~= nil then
        diagnostics[#diagnostics + 1] = {
          path = path,
          message = "duplicate skill name '" .. result.skill.name .. "'",
        }
      else
        for _, warning in ipairs(result.warnings) do
          add_warning(diagnostics, path, warning)
        end

        local openai_path = editor.fs.joinpath(editor.fs.dirname(path), "agents", "openai.yaml")
        local openai_stat = editor.uv.fs_stat(openai_path)
        local allow_implicit_invocation
        local openai_error
        if openai_stat ~= nil then
          if openai_stat.type ~= "file" then
            openai_error = "could not read agents/openai.yaml; treating skill as explicit-only"
          else
            local _, openai_content = read_file(openai_path)
            if openai_content == nil then
              openai_error = "could not read agents/openai.yaml; treating skill as explicit-only"
            else
              allow_implicit_invocation, openai_error = Metadata.openai(openai_content, yaml)
            end
          end
        end

        if openai_error ~= nil then
          result.skill.explicit_only = true
          add_warning(diagnostics, openai_path, openai_error)
        elseif allow_implicit_invocation ~= nil then
          local openai_explicit_only = not allow_implicit_invocation
          if result.disable_model_invocation ~= nil and result.disable_model_invocation ~= openai_explicit_only then
            add_warning(diagnostics, openai_path, "invocation controls disagree; treating skill as explicit-only")
          end
          result.skill.explicit_only = result.skill.explicit_only or openai_explicit_only
        end

        names[result.skill.name] = true
        skills[#skills + 1] = result.skill
      end
    end
  end

  table.sort(skills, function(left, right)
    if left.name == right.name then
      return left.path < right.path
    end
    return left.name < right.name
  end)
  sort_diagnostics(diagnostics)
  return skills, diagnostics
end

return M
