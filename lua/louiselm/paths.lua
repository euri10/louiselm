local M = {}

---Return the shared LouiseLM state directory without creating it.
---Independent of Neovim's application/profile name; explicit store paths bypass this default.
---@return string directory XDG_STATE_HOME/louiselm, or ~/.local/state/louiselm when unset or empty.
function M.state()
  ---@diagnostic disable-next-line: undefined-global -- Neovim runtime supplies environment and path facilities.
  local nvim = vim
  local root = nvim.env.XDG_STATE_HOME
  if root == nil or root == "" then
    root = nvim.fs.joinpath(nvim.fn.expand("~"), ".local", "state")
  end
  return nvim.fs.joinpath(root, "louiselm")
end

return M
