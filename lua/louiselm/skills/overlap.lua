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

---@return table
local function nvim()
  ---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
  return vim
end

---@param path string
---@return string
local function canonical(path)
  local editor = nvim()
  local realpath = editor.uv.fs_realpath(path)
  return editor.fs.normalize(realpath or path)
end

---@param path string
---@param root string
---@return boolean
local function is_same_or_child(path, root)
  if path == root then
    return true
  end
  if root == "/" then
    return path:sub(1, 1) == "/"
  end
  return path:sub(1, #root + 1) == root .. "/"
end

---Detect a native skill directory overlapping a configured skill path.
---@param native_path unknown Native agent skill directory.
---@param configured_paths unknown Dense array of configured skill directories.
---@return boolean? overlaps True and the matching configured path when found.
---@return string? overlap_path The first configured path containing or contained by the native path.
---@return string? error_message Validation failure, when inputs are malformed.
function M.detect(native_path, configured_paths)
  if type(native_path) ~= "string" or native_path == "" then
    return nil, nil, "native skill path must be a non-empty string"
  end
  if type(configured_paths) ~= "table" or not is_dense_array(configured_paths) then
    return nil, nil, "configured skill paths must be a dense array"
  end

  local native = canonical(native_path)
  for index, configured_path in ipairs(configured_paths) do
    if type(configured_path) ~= "string" or configured_path == "" then
      return nil, nil, string.format("configured skill path at index %d must be a non-empty string", index)
    end
    local configured = canonical(configured_path)
    if is_same_or_child(native, configured) or is_same_or_child(configured, native) then
      return true, configured_path
    end
  end
  return false
end

return M
