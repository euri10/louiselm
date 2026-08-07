local M = {}

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

---@return string
local function buffer_path(buffer)
  local path = nvim.fs.normalize(nvim.api.nvim_buf_get_name(buffer))
  return path == "" and "[No Name]" or path
end

---@param lines string[] Selected buffer lines.
---@param start_column integer Zero-based inclusive start column.
---@param end_column integer Zero-based inclusive end column.
---@return string[] selected Selected text lines.
local function slice_lines(lines, start_column, end_column)
  if #lines == 1 then
    return { string.sub(lines[1], start_column + 1, end_column + 1) }
  end
  lines[1] = string.sub(lines[1], start_column + 1)
  lines[#lines] = string.sub(lines[#lines], 1, end_column + 1)
  return lines
end

---Capture the visual selection in the current buffer.
---@return louiselm.ui.ContextItem? item Selection context, or nil when no selection exists.
---@return string? error_message Selection or buffer error.
function M.current(buffer)
  buffer = buffer or 0
  local start = nvim.api.nvim_buf_get_mark(buffer, "<")
  local finish = nvim.api.nvim_buf_get_mark(buffer, ">")
  if start[1] == 0 or finish[1] == 0 then
    return nil, "no visual selection"
  end
  if start[1] > finish[1] or (start[1] == finish[1] and start[2] > finish[2]) then
    start, finish = finish, start
  end

  local lines = nvim.api.nvim_buf_get_lines(buffer, start[1] - 1, finish[1], false)
  if #lines == 0 then
    return nil, "visual selection is empty"
  end
  local selected = slice_lines(lines, start[2], finish[2])
  local path = buffer_path(buffer)
  local location = string.format("%s:%d-%d", path, start[1], finish[1])
  return {
    label = "selection: " .. location,
    text = "Selection from " .. location .. "\n```\n" .. table.concat(selected, "\n") .. "\n```",
  }
end

return M
