local Inspector = require("louiselm.ui.chat.inspector")

---@diagnostic disable-next-line: undefined-global -- Neovim injects its runtime API.
local nvim = vim

-- Structural views of the Neovim Tree-sitter APIs consumed by this renderer.
---@class louiselm.ui.TableNode
---@field type fun(self: louiselm.ui.TableNode): string
---@field range fun(self: louiselm.ui.TableNode): integer, integer, integer, integer
---@field iter_children fun(self: louiselm.ui.TableNode): fun(): louiselm.ui.TableNode?

---@class louiselm.ui.TableTree
---@field root fun(self: louiselm.ui.TableTree): louiselm.ui.TableNode

---@class louiselm.ui.TableParser
---@field parse fun(self: louiselm.ui.TableParser): louiselm.ui.TableTree[]

---@class louiselm.ui.TableQuery
---@field iter_captures fun(self: louiselm.ui.TableQuery, root: louiselm.ui.TableNode, buffer: integer, first: integer, last: integer): fun(): integer?, louiselm.ui.TableNode

---@class louiselm.ui.ChatTables
---@field package buffer integer
---@field package namespace integer
---@field package group integer
---@field package prompt_start fun(): integer
---@field package parser louiselm.ui.TableParser
---@field package query louiselm.ui.TableQuery
---@field package pending boolean
---@field package disposed boolean
---@field package inspector integer?
local Tables = {}
Tables.__index = Tables
local M = {}

---@param buffer integer
---@param node louiselm.ui.TableNode
---@return string[]? lines Aligned source cells, or nil for unsupported/malformed rows.
local function layout(buffer, node)
  local rows, widths, alignment, prefixes = {}, {}, {}, {}
  local source_width = 0
  for row in node:iter_children() do
    local kind = row:type()
    if kind == "pipe_table_header" or kind == "pipe_table_delimiter_row" or kind == "pipe_table_row" then
      local line, start_col, _, end_col = row:range()
      local source = nvim.api.nvim_buf_get_lines(buffer, line, line + 1, false)[1]
      source_width = math.max(source_width, nvim.fn.strdisplaywidth(source))
      prefixes[#rows + 1] = source:sub(1, start_col)
      local cells, first = {}, start_col
      for child in row:iter_children() do
        if child:type() == "|" then
          local _, col, _, stop = child:range()
          if first > start_col or nvim.trim(source:sub(first + 1, col)) ~= "" then
            cells[#cells + 1] = nvim.trim(source:sub(first + 1, col))
          end
          first = stop
        end
      end
      if first < end_col and nvim.trim(source:sub(first + 1, end_col)) ~= "" then
        cells[#cells + 1] = nvim.trim(source:sub(first + 1, end_col))
      end
      if #cells == 0 or (#rows > 0 and #cells ~= #rows[1]) then
        return nil
      end
      for col, cell in ipairs(cells) do
        if kind == "pipe_table_delimiter_row" then
          alignment[col] = cell:match(":$") and (cell:match("^:") and "center" or "right") or "left"
        else
          widths[col] = math.max(widths[col] or 3, nvim.fn.strdisplaywidth(cell))
        end
      end
      rows[#rows + 1] = cells
    end
  end
  if #rows < 2 then
    return nil
  end
  local lines = {}
  for index, cells in ipairs(rows) do
    local parts = {}
    for col, cell in ipairs(cells) do
      local width = widths[col]
      if index == 2 then
        parts[col] = string.rep("-", width)
      else
        local padding = width - nvim.fn.strdisplaywidth(cell)
        local left = alignment[col] == "right" and padding
          or (alignment[col] == "center" and math.floor(padding / 2) or 0)
        parts[col] = string.rep(" ", left) .. cell .. string.rep(" ", padding - left)
      end
    end
    local line = prefixes[index] .. "| " .. table.concat(parts, " | ") .. " |"
    -- Overlay must cover even unusually padded source rows without concealment.
    lines[index] = line .. string.rep(" ", math.max(0, source_width - nvim.fn.strdisplaywidth(line)))
  end
  return lines
end

---Refresh decorations using the narrowest window showing this buffer.
---@param self louiselm.ui.ChatTables
function Tables:refresh()
  if self.disposed or not nvim.api.nvim_buf_is_valid(self.buffer) then
    return
  end
  nvim.api.nvim_buf_clear_namespace(self.buffer, self.namespace, 0, -1)
  local width
  for _, window in ipairs(nvim.fn.win_findbuf(self.buffer)) do
    local info = nvim.fn.getwininfo(window)[1]
    local available = nvim.api.nvim_win_get_width(window) - info.textoff
    width = math.min(width or available, available)
  end
  if width == nil then
    return
  end
  local tree = self.parser:parse()[1]
  if tree == nil then
    return
  end
  local boundary = self.prompt_start()
  for _, node in self.query:iter_captures(tree:root(), self.buffer, 0, boundary) do
    local first, _, last = node:range()
    local lines = last <= boundary and layout(self.buffer, node) or nil
    if lines ~= nil then
      if nvim.fn.strdisplaywidth(lines[1]) > width then
        nvim.api.nvim_buf_set_extmark(self.buffer, self.namespace, first, 0, {
          virt_lines = { { { "Wide table · gT to inspect (zh/zl to scroll)", "Comment" } } },
          virt_lines_above = true,
        })
      else
        for index, line in ipairs(lines) do
          nvim.api.nvim_buf_set_extmark(self.buffer, self.namespace, first + index - 1, 0, {
            virt_text = { { line, index == 1 and "Title" or (index == 2 and "Comment" or "Normal") } },
            virt_text_pos = "overlay",
            virt_text_hide = true,
            hl_mode = "replace",
          })
        end
      end
    end
  end
end

---@param self louiselm.ui.ChatTables
local function schedule(self)
  if self.pending or self.disposed then
    return
  end
  self.pending = true
  nvim.schedule(function()
    self.pending = false
    self:refresh()
  end)
end

---Inspect the table under the cursor without truncating its cells.
---@param self louiselm.ui.ChatTables
---@return boolean opened False outside a supported transcript table.
function Tables:inspect()
  if self.disposed or nvim.api.nvim_get_current_buf() ~= self.buffer then
    return false
  end
  local line = nvim.api.nvim_win_get_cursor(0)[1] - 1
  if line >= self.prompt_start() then
    return false
  end
  local tree = self.parser:parse()[1]
  if tree == nil then
    return false
  end
  for _, node in self.query:iter_captures(tree:root(), self.buffer, line, line + 1) do
    local first, _, last = node:range()
    if first <= line and line < last and last <= self.prompt_start() then
      local lines = layout(self.buffer, node)
      if lines ~= nil then
        if self.inspector ~= nil and nvim.api.nvim_win_is_valid(self.inspector) then
          nvim.api.nvim_win_close(self.inspector, true)
        end
        self.inspector = Inspector.open(lines, function()
          self.inspector = nil
        end, "Table · zh/zl scroll · q close")
        nvim.api.nvim_win_set_height(self.inspector, math.min(math.max(10, #lines), math.max(1, nvim.o.lines - 4)))
        nvim.api.nvim_set_option_value("wrap", false, { win = self.inspector })
        return true
      end
    end
  end
  return false
end

---Release handlers, pending rendering and the owned inspection window; safe to repeat.
---@param self louiselm.ui.ChatTables
function Tables:dispose()
  if self.disposed then
    return
  end
  self.disposed = true
  nvim.api.nvim_del_augroup_by_id(self.group)
  if self.inspector ~= nil and nvim.api.nvim_win_is_valid(self.inspector) then
    nvim.api.nvim_win_close(self.inspector, true)
  end
  if nvim.api.nvim_buf_is_valid(self.buffer) then
    nvim.api.nvim_buf_clear_namespace(self.buffer, self.namespace, 0, -1)
  end
end

---Attach native table presentation to a Markdown-parsed chat buffer.
---@param buffer integer Owned chat buffer with an available Markdown parser.
---@param prompt_start fun(): integer Zero-based start of the editable prompt.
---@return louiselm.ui.ChatTables tables Caller must dispose before deleting the buffer.
function M.new(buffer, prompt_start)
  local self = setmetatable({
    buffer = buffer,
    namespace = nvim.api.nvim_create_namespace("louiselm.chat.tables"),
    group = nvim.api.nvim_create_augroup("louiselm.chat.tables." .. buffer, { clear = true }),
    prompt_start = prompt_start,
    parser = nvim.treesitter.get_parser(buffer, "markdown"),
    query = nvim.treesitter.query.parse("markdown", "(pipe_table) @table"),
    pending = false,
    disposed = false,
  }, Tables)
  nvim.api.nvim_buf_attach(buffer, false, {
    on_lines = function()
      schedule(self)
    end,
    on_detach = function()
      self:dispose()
    end,
  })
  nvim.api.nvim_create_autocmd("BufWinEnter", {
    group = self.group,
    buffer = buffer,
    callback = function()
      schedule(self)
    end,
  })
  nvim.api.nvim_create_autocmd({ "WinResized", "VimResized" }, {
    group = self.group,
    callback = function()
      schedule(self)
    end,
  })
  nvim.keymap.set("n", "gT", function()
    self:inspect()
  end, {
    buffer = buffer,
    silent = true,
    desc = "Inspect Markdown table",
  })
  schedule(self)
  return self
end

return M
