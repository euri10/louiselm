---@class louiselm.skills.Skill
---@field name string Agent Skills name.
---@field description string Short description shown in the injected catalog.
---@field short_description? string OpenAI `metadata.short-description` extension, used to correlate native picker commands.
---@field path string Absolute path to SKILL.md.
---@field content? string Complete original SKILL.md content captured for deliberate activation.
---@field explicit_only boolean Whether only deliberate picker activation may select the skill.

---@class louiselm.skills.DiscoveryDiagnostic
---@field path string File or configured directory related to the error.
---@field message string Actionable discovery failure.
---@field detail? string Expanded guidance for health checks.
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
---@param cwd string
---@return string
local function absolute_path(value, cwd)
  local editor = nvim()
  local normalized = editor.fs.normalize(value)
  if editor.fs.abspath(normalized) == normalized then
    return normalized
  end
  return editor.fs.normalize(editor.fs.joinpath(cwd, normalized))
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
---@return string? error_message
local function load_yaml()
  local call_ok, module = pcall(require, "lyaml")
  if not call_ok then
    return nil, tostring(module)
  end
  if type(module) ~= "table" or type(module.load) ~= "function" then
    return nil, "lyaml does not expose the expected load function"
  end
  return module, nil
end

---@param error_message string
---@return string
local function yaml_dependency_message(error_message)
  if error_message:find("module 'lyaml' not found", 1, true) ~= nil then
    return 'Neovim cannot find lyaml in package.path or package.cpath; install it with `luarocks --lua-version 5.1 install lyaml` or, if LuaRocks already reports it installed, add `eval "$(luarocks path --lua-version 5.1 --no-bin)"` to the shell startup file that launches Neovim'
  end
  return "Neovim found lyaml but could not load it; reinstall lyaml for Lua 5.1 and verify that LibYAML is available"
end

---@param paths unknown
---@param cwd string
---@return string[]? normalized
---@return louiselm.skills.DiscoveryDiagnostic[] errors
local function normalize_paths(paths, cwd)
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
      normalized[#normalized + 1] = absolute_path(path, cwd)
    end
  end
  return normalized, errors
end

---@param diagnostics louiselm.skills.DiscoveryDiagnostic[]
---@param path string
---@param message string
local function add_warning(diagnostics, path, message)
  diagnostics[#diagnostics + 1] = { path = path, message = message, severity = "warning" }
end

---Read a discovered SKILL.md without caching its contents.
---@param path unknown Absolute path advertised by discovery.
---@return string? content Complete current file content.
---@return string? error_message Validation or read failure.
function M.read(path)
  if type(path) ~= "string" or path == "" then
    return nil, "skill path must be a non-empty string"
  end
  local _, content = read_file(path)
  if content == nil then
    return nil, "could not read SKILL.md"
  end
  return content, nil
end

---@param path string
---@param root string
---@return boolean
local function is_within(path, root)
  if path == root then
    return true
  end
  local prefix = root:sub(-1) == "/" and root or (root .. "/")
  return path:sub(1, #prefix) == prefix
end

---@class louiselm.skills.ScanEntry
---@field name string
---@field type string

---@param directory string
---@return louiselm.skills.ScanEntry[]? entries
local function scan_entries(directory)
  local handle = nvim().uv.fs_scandir(directory)
  if handle == nil then
    return nil
  end

  local entries = {}
  while true do
    local name, entry_type = nvim().uv.fs_scandir_next(handle)
    if name == nil then
      break
    end
    entries[#entries + 1] = { name = name, type = entry_type }
  end
  table.sort(entries, function(left, right)
    return left.name < right.name
  end)
  return entries
end

---@param diagnostics louiselm.skills.DiscoveryDiagnostic[]
---@param alias string
---@param canonical string
local function warn_outside_root(diagnostics, alias, canonical)
  add_warning(diagnostics, alias, "symlink resolves outside configured root: " .. alias .. " -> " .. canonical)
end

---@param root string
---@param canonical_root string
---@param seen_directories table<string, string>
---@param seen_files table<string, string>
---@param diagnostics louiselm.skills.DiscoveryDiagnostic[]
---@return string[] files
local function scan_root(root, canonical_root, seen_directories, seen_files, diagnostics)
  local editor = nvim()
  local directories = { root }
  local files = {}
  local index = 1

  while index <= #directories do
    local directory = directories[index]
    index = index + 1
    local entries = scan_entries(directory)
    if entries == nil then
      diagnostics[#diagnostics + 1] = { path = directory, message = "could not scan skill path" }
    else
      for _, entry in ipairs(entries) do
        local alias = editor.fs.joinpath(directory, entry.name)
        local stat = editor.uv.fs_stat(alias)
        if stat ~= nil and stat.type == "directory" then
          local canonical = canonical_path(alias)
          if entry.type == "link" and not is_within(canonical, canonical_root) then
            warn_outside_root(diagnostics, alias, canonical)
          end
          local first_alias = seen_directories[canonical]
          if first_alias ~= nil then
            add_warning(diagnostics, alias, "canonical directory already scanned: " .. alias .. " -> " .. first_alias)
          else
            seen_directories[canonical] = alias
            directories[#directories + 1] = alias
          end
        elseif entry.name == "SKILL.md" and stat ~= nil and stat.type == "file" then
          local canonical = canonical_path(alias)
          if entry.type == "link" and not is_within(canonical, canonical_root) then
            warn_outside_root(diagnostics, alias, canonical)
          end
          local first_alias = seen_files[canonical]
          if first_alias ~= nil then
            add_warning(diagnostics, alias, "canonical SKILL.md already discovered: " .. alias .. " -> " .. first_alias)
          else
            seen_files[canonical] = alias
            files[#files + 1] = alias
          end
        end
      end
    end
  end

  table.sort(files)
  return files
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

---Discover Agent Skills metadata below configured directories.
---@param paths unknown Dense array of directories containing SKILL.md files.
---@param cwd? string Base directory for relative configured paths. Defaults to Neovim's current working directory.
---@return louiselm.skills.Skill[] skills Valid skills in root order, then sorted by name and path within each root.
---@return louiselm.skills.DiscoveryDiagnostic[] diagnostics All invalid paths, metadata errors, and warnings.
function M.discover(paths, cwd)
  local editor = nvim()
  local base_directory = editor.fs.abspath(editor.fs.normalize(cwd or editor.fn.getcwd()))
  local normalized_paths, diagnostics = normalize_paths(paths, base_directory)
  if normalized_paths == nil then
    return {}, diagnostics
  end

  local files_by_root = {}
  local seen_directories = {}
  local seen_files = {}
  for _, root in ipairs(normalized_paths) do
    local stat = editor.uv.fs_stat(root)
    if stat == nil or stat.type ~= "directory" then
      diagnostics[#diagnostics + 1] = { path = root, message = "skill path is not an existing directory" }
    else
      local canonical_root = canonical_path(root)
      local first_alias = seen_directories[canonical_root]
      if first_alias ~= nil then
        add_warning(diagnostics, root, "configured root resolves to the already scanned root " .. first_alias)
      else
        seen_directories[canonical_root] = root
        files_by_root[#files_by_root + 1] = scan_root(root, canonical_root, seen_directories, seen_files, diagnostics)
      end
    end
  end

  if next(seen_files) == nil then
    sort_diagnostics(diagnostics)
    return {}, diagnostics
  end
  local yaml, yaml_error = load_yaml()
  if yaml == nil then
    diagnostics[#diagnostics + 1] = {
      path = "skills",
      message = "Neovim cannot load lyaml; run :checkhealth louiselm",
      detail = yaml_dependency_message(yaml_error or "unknown lyaml load failure"),
      code = "missing_dependency",
    }
    sort_diagnostics(diagnostics)
    return {}, diagnostics
  end

  local skills = {}
  local names = {}
  for _, files in ipairs(files_by_root) do
    local root_skills = {}
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
          add_warning(
            diagnostics,
            path,
            "skill '" .. result.skill.name .. "' is shadowed by " .. names[result.skill.name]
          )
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

          names[result.skill.name] = path
          root_skills[#root_skills + 1] = result.skill
        end
      end
    end

    table.sort(root_skills, function(left, right)
      if left.name == right.name then
        return left.path < right.path
      end
      return left.name < right.name
    end)
    editor.list_extend(skills, root_skills)
  end
  sort_diagnostics(diagnostics)
  return skills, diagnostics
end

return M
