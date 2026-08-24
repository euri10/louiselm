local M = {}

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

---@class louiselm.ui.DiffBufferOptions
---@field focus? boolean Whether to focus the opened buffer; defaults to true.
---@field instruction? string Controller-owned instructions displayed above the diff.

local function diff_lines(value)
  local lines = {}
  local start = 1
  while start <= #value do
    local newline = value:find("\n", start, true)
    if newline == nil then
      lines[#lines + 1] = value:sub(start)
      break
    end
    lines[#lines + 1] = value:sub(start, newline - 1)
    start = newline + 1
  end
  return lines
end

---@param preview louiselm.ui.DiffPreview
---@param instruction? string
---@return string[] lines
local function render_lines(preview, instruction)
  local lines = {
    "louiselm diff: " .. preview.path,
    "",
  }
  if instruction ~= nil then
    lines[#lines + 1] = instruction
    lines[#lines + 1] = ""
  end
  lines[#lines + 1] = "--- original"
  lines[#lines + 1] = "+++ proposed"
  for _, line in ipairs(diff_lines(preview.diff)) do
    lines[#lines + 1] = line
  end
  return lines
end

---Open a read-only scratch buffer containing a file diff.
---@param preview louiselm.ui.DiffPreview Preview returned by `louiselm.ui.diff.apply.preview`.
---@param options? louiselm.ui.DiffBufferOptions Optional buffer options.
---@return integer? buffer Buffer handle, or nil on invalid input.
---@return string? error_message Validation or buffer error.
function M.open(preview, options)
  if type(preview) ~= "table" or type(preview.path) ~= "string" or type(preview.diff) ~= "string" then
    return nil, "diff buffer requires a file preview"
  end
  if options ~= nil and type(options) ~= "table" then
    return nil, "diff buffer options must be a table"
  end
  local buffer = nvim.api.nvim_create_buf(false, true)
  local ok, error_message = pcall(function()
    nvim.api.nvim_buf_set_name(buffer, "louiselm-diff://" .. preview.path)
    nvim.api.nvim_set_option_value("buftype", "nofile", { buf = buffer })
    nvim.api.nvim_set_option_value("bufhidden", "wipe", { buf = buffer })
    nvim.api.nvim_set_option_value("swapfile", false, { buf = buffer })
    nvim.api.nvim_set_option_value("modifiable", true, { buf = buffer })
    nvim.api.nvim_buf_set_lines(buffer, 0, -1, false, render_lines(preview, options and options.instruction))
    nvim.api.nvim_set_option_value("filetype", "diff", { buf = buffer })
    nvim.api.nvim_set_option_value("modifiable", false, { buf = buffer })
  end)
  if not ok then
    if nvim.api.nvim_buf_is_valid(buffer) then
      nvim.api.nvim_buf_delete(buffer, { force = true })
    end
    return nil, tostring(error_message)
  end
  if options == nil or options.focus ~= false then
    nvim.api.nvim_set_current_buf(buffer)
    if nvim.api.nvim_get_mode().mode:sub(1, 1) == "i" then
      nvim.api.nvim_input("<Esc>")
    end
  end
  return buffer
end

---Close a diff buffer.
---@param buffer integer Buffer handle.
---@return boolean closed True when the buffer was removed.
function M.close(buffer)
  if type(buffer) ~= "number" or not nvim.api.nvim_buf_is_valid(buffer) then
    return false
  end
  nvim.api.nvim_buf_delete(buffer, { force = true })
  return true
end

return M
