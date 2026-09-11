---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local M = {}

---Open a read-only payload inspector; the caller owns the returned window.
---@param lines string[]
---@param on_close fun() Called during BufWipeout to release caller-owned state.
---@param title? string Window title.
---@return integer window
function M.open(lines, on_close, title)
  local buffer = nvim.api.nvim_create_buf(false, true)
  nvim.api.nvim_set_option_value("buftype", "nofile", { buf = buffer })
  nvim.api.nvim_set_option_value("bufhidden", "wipe", { buf = buffer })
  nvim.api.nvim_set_option_value("swapfile", false, { buf = buffer })
  nvim.api.nvim_buf_set_lines(buffer, 0, -1, false, lines)
  nvim.api.nvim_set_option_value("modifiable", false, { buf = buffer })
  local width = math.min(100, math.max(1, nvim.o.columns - 4))
  local height = math.min(#lines, math.max(1, nvim.o.lines - 4))
  local window = nvim.api.nvim_open_win(buffer, true, {
    relative = "editor",
    row = 1,
    col = 2,
    width = width,
    height = height,
    style = "minimal",
    border = "rounded",
    title = title,
  })
  nvim.api.nvim_set_option_value("wrap", true, { win = window })
  nvim.api.nvim_create_autocmd("BufWipeout", {
    buffer = buffer,
    once = true,
    callback = function()
      on_close()
    end,
  })
  local function close()
    if nvim.api.nvim_buf_is_valid(buffer) then
      nvim.api.nvim_buf_delete(buffer, { force = true })
    end
  end
  nvim.keymap.set("n", "q", close, { buffer = buffer, silent = true, nowait = true, desc = "Close tool inspector" })
  nvim.keymap.set("n", "<Esc>", close, { buffer = buffer, silent = true, nowait = true, desc = "Close tool inspector" })
  return window
end

return M
