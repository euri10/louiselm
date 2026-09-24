---Explicitly owned JSONL display decorations; buffer content remains untouched.
---@diagnostic disable-next-line: undefined-global -- Neovim runtime.
local nvim = vim
local Jsonl = require("louiselm.jsonl")
local M = {}
local DISPLAY = { wrap = false, conceallevel = 2, concealcursor = "" }

---@class louiselm.ui.JsonlView
---@field package namespace integer
---@field package group integer
---@field package buffers table<integer, boolean>
---@field package windows table<integer, table>
---@field package disposed boolean
---@field package scheduled boolean
local View = {}
View.__index = View

local function restore(win, state)
  if not nvim.api.nvim_win_is_valid(win) then
    return
  end
  for name, value in pairs(DISPLAY) do
    if nvim.api.nvim_get_option_value(name, { win = win }) == value then
      nvim.api.nvim_set_option_value(name, state.options[name], { win = win })
    end
  end
end

local function synchronize(self)
  for win, state in pairs(self.windows) do
    if
      not nvim.api.nvim_win_is_valid(win)
      or nvim.api.nvim_win_get_buf(win) ~= state.buffer
      or not self.buffers[state.buffer]
    then
      restore(win, state)
      self.windows[win] = nil
    end
  end
  for _, win in ipairs(nvim.api.nvim_list_wins()) do
    local buffer = nvim.api.nvim_win_get_buf(win)
    if self.buffers[buffer] and self.windows[win] == nil then
      local options = {}
      -- A split inherits the decorated window's options. Preserve its original
      -- baseline too, so leaving the JSONL buffer never leaks display settings.
      for name, value in pairs(DISPLAY) do
        options[name] = nvim.api.nvim_get_option_value(name, { win = win })
        if options[name] == value then
          for _, other in pairs(self.windows) do
            if other.buffer == buffer then
              options[name] = other.options[name]
              break
            end
          end
        end
        nvim.api.nvim_set_option_value(name, value, { win = win })
      end
      self.windows[win] = { buffer = buffer, options = options, cache = {}, count = 0 }
    end
  end
end

---Construct one display owner; disposal removes all its callbacks and options.
---@return louiselm.ui.JsonlView view
function M.new()
  local namespace = nvim.api.nvim_create_namespace("")
  local self =
    setmetatable({ namespace = namespace, buffers = {}, windows = {}, disposed = false, scheduled = false }, View)
  self.group = nvim.api.nvim_create_augroup("louiselm-jsonl-" .. namespace, { clear = true })
  nvim.api.nvim_create_autocmd({ "BufWinEnter", "BufWinLeave", "WinEnter", "WinClosed", "BufWipeout" }, {
    group = self.group,
    callback = function(event)
      if event.event == "BufWipeout" then
        self.buffers[event.buf] = nil
      end
      if self.scheduled then
        return
      end
      self.scheduled = true
      nvim.schedule(function()
        self.scheduled = false
        if not self.disposed then
          synchronize(self)
        end
      end)
    end,
  })
  nvim.api.nvim_create_autocmd("ModeChanged", {
    group = self.group,
    callback = function()
      -- Mode changes otherwise redraw only touched rows, leaving old summaries
      -- on screen while Visual selection exposes the underlying JSON bytes.
      if not self.disposed and next(self.buffers) ~= nil then
        nvim.cmd("redraw!")
      end
    end,
  })
  nvim.api.nvim_set_decoration_provider(namespace, {
    on_win = function(_, win, buffer)
      local mode = nvim.api.nvim_get_mode().mode:sub(1, 1)
      return not self.disposed
        and self.buffers[buffer] == true
        and self.windows[win] ~= nil
        and mode ~= "v"
        and mode ~= "V"
        and mode ~= "\22"
    end,
    on_range = function(_, win, buffer, first, _, last, last_column)
      local state = self.windows[win]
      if self.disposed or state == nil then
        return
      end
      local cursor = nvim.api.nvim_win_get_cursor(win)[1] - 1
      local stop = math.min(last + (last_column > 0 and 1 or 0), first + 256)
      for index, line in ipairs(nvim.api.nvim_buf_get_lines(buffer, first, stop, false)) do
        local row = first + index - 1
        if row ~= cursor and #line <= 16384 then
          local entry = state.cache[row]
          if entry == nil or entry.line ~= line then
            -- Bound retained source bytes even after scrolling through huge logs.
            if state.count >= 256 then
              state.cache, state.count = {}, 0
            end
            local summary = Jsonl.summary(line)
            entry = { line = line, summary = summary }
            state.cache[row], state.count = entry, state.count + 1
          end
          if entry.summary ~= nil then
            nvim.api.nvim_buf_set_extmark(buffer, namespace, row, 0, {
              end_col = #line,
              conceal = "",
              ephemeral = true,
              virt_text = { { entry.summary, "Normal" } },
              virt_text_pos = "overlay",
              priority = 200,
            })
          end
        end
      end
    end,
  })
  return self
end

---Toggle compact display for a text buffer in every window displaying it.
---@param self louiselm.ui.JsonlView
---@param buffer integer Buffer id; 0 means current buffer.
---@return boolean? enabled Nil on invalid buffer or disposed owner.
---@return string? error_message Fixed actionable error, without buffer contents.
function View:toggle(buffer)
  if self.disposed then
    return nil, "JSONL view is disposed"
  end
  if buffer == 0 then
    buffer = nvim.api.nvim_get_current_buf()
  end
  if not nvim.api.nvim_buf_is_valid(buffer) or nvim.bo[buffer].buftype ~= "" then
    return nil, "JSONL display requires an ordinary text buffer"
  end
  self.buffers[buffer] = not self.buffers[buffer] or nil
  synchronize(self)
  nvim.cmd.redraw()
  return self.buffers[buffer] == true
end

---Remove owned decorations, autocmds and unchanged display overrides.
---@param self louiselm.ui.JsonlView
function View:dispose()
  if self.disposed then
    return
  end
  self.disposed = true
  nvim.api.nvim_set_decoration_provider(self.namespace, {})
  nvim.api.nvim_del_augroup_by_id(self.group)
  for win, state in pairs(self.windows) do
    restore(win, state)
  end
  self.windows, self.buffers = {}, {}
  nvim.cmd.redraw()
end

return M
