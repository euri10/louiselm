---@class louiselm.ui.ContextItem
---@field label string Short label shown in the chat prompt.
---@field text string ACP text content.

local M = {}

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

---Build a context item pointing at a buffer.
---@param buffer? integer Buffer handle; defaults to the current buffer.
---@return louiselm.ui.ContextItem item Context for the buffer.
function M.current(buffer)
  buffer = buffer or 0
  local path = nvim.fs.normalize(nvim.api.nvim_buf_get_name(buffer))
  if path == "" then
    return { label = "buffer: [No Name]", text = "Current buffer is unnamed." }
  end
  return { label = "buffer: " .. path, text = "Current buffer: " .. path }
end

return M
