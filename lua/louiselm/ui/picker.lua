local M = {}

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

---Open a UI selector outside insert mode, then leave interaction to the configured provider.
---@generic T
---@param items T[] Choices passed unchanged to `vim.ui.select`.
---@param options table Picker options passed unchanged to `vim.ui.select`.
---@param callback fun(item: T?, index?: integer) Selection callback.
function M.select(items, options, callback)
  nvim.cmd.stopinsert()
  nvim.ui.select(items, options, callback)
end

return M
