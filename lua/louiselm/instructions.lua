local M = {}

---@return table
local function nvim()
  ---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
  return vim
end

---Build a resource-link context item for a project instructions file, when it exists.
---@param filename? string Project-root-relative filename to link; nil or empty disables.
---@param cwd? string Base directory. Defaults to Neovim's current working directory.
---@return louiselm.ui.ContextItem? item Resource-link context, or nil when disabled or absent.
function M.link(filename, cwd)
  if type(filename) ~= "string" or filename == "" then
    return nil
  end
  local editor = nvim()
  local root = editor.fs.abspath(editor.fs.normalize(cwd or editor.fn.getcwd()))
  local path = editor.fs.joinpath(root, filename)
  local stat = editor.uv.fs_stat(path)
  if stat == nil or stat.type ~= "file" then
    return nil
  end
  return { label = filename, uri = "file://" .. path }
end

return M
