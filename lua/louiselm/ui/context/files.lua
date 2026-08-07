local M = {}

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

---@param root? string Directory to scan; defaults to the current working directory.
---@return string? path Normalized directory path.
---@return string? error_message Validation or filesystem error.
local function normalize_root(root)
  if root == nil then
    root = nvim.fn.getcwd()
  end
  if type(root) ~= "string" or root == "" then
    return nil, "file context root must be a non-empty string"
  end
  local path = nvim.fs.normalize(nvim.fn.fnamemodify(nvim.fn.expand(root), ":p"))
  local stat = nvim.uv.fs_stat(path)
  if stat == nil or stat.type ~= "directory" then
    return nil, "file context root is not an existing directory"
  end
  return path
end

---List project files below a directory.
---@param root? string Directory to scan; defaults to the current working directory.
---@return string[]? paths Sorted absolute file paths, or nil on failure.
---@return string? error_message Validation or filesystem error.
function M.list(root)
  local path, root_error = normalize_root(root)
  if path == nil then
    return nil, root_error
  end
  local found_ok, found = pcall(nvim.fs.find, function(name)
    return name ~= ".git"
  end, { path = path, type = "file", limit = math.huge })
  if not found_ok then
    return nil, "could not scan file context root"
  end

  local files = {}
  for _, file in ipairs(found) do
    local normalized = nvim.fs.normalize(file)
    if not normalized:find("/.git/", 1, true) then
      files[#files + 1] = normalized
    end
  end
  table.sort(files)
  return files
end

---Build a context item pointing at one file.
---@param path string Absolute or relative file path.
---@return louiselm.ui.ContextItem? item File context, or nil on invalid input.
---@return string? error_message Validation error.
function M.context(path)
  if type(path) ~= "string" or path == "" then
    return nil, "file context path must be a non-empty string"
  end
  path = nvim.fs.normalize(nvim.fn.fnamemodify(nvim.fn.expand(path), ":p"))
  return { label = "file: " .. path, text = "Referenced file: " .. path }
end

---Pick a file through Neovim's configured UI picker.
---@param root? string Directory to scan; defaults to the current working directory.
---@param callback fun(path: string?, error_message?: string) Called once with the selection.
---@return boolean started True when the picker was opened.
---@return string? error_message Validation or filesystem error.
function M.pick(root, callback)
  if type(callback) ~= "function" then
    return false, "file context callback must be a function"
  end
  local files, list_error = M.list(root)
  if files == nil then
    callback(nil, list_error)
    return false, list_error
  end
  nvim.ui.select(files, { prompt = "louiselm file: " }, function(choice)
    callback(choice)
  end)
  return true
end

return M
