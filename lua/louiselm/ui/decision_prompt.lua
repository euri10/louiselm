---@diagnostic disable-next-line: undefined-global -- Neovim owns this presentation boundary.
local nvim = vim

---@class louiselm.ui.DecisionSelectOptions
---@field prompt? string Single-line title; control characters are displayed as spaces.
---@field format_item? fun(item: unknown): string Display label for each choice.

---@class louiselm.ui.DecisionInputOptions
---@field prompt? string Input title.
---@field default? string Initial editable text.

---@class louiselm.ui.DecisionPrompt
---@field buffer? integer Owned buffer, readable by consumers; retained while hidden.
---@field window? integer Owned visible window, readable by consumers.
---@field package disposed boolean
---@field package opening boolean Whether opening has entered its buffer before returning a window.
---@field package revision integer Invalidates mappings from an earlier presentation.
---@field package kind? "select"|"input"|"inspect"
---@field package title? string
---@field package items? unknown[]
---@field package callback? fun(value?: unknown, index?: integer)
---@field package saved_view? table<string, integer>
---@field package can_show? fun(): boolean Whether the requesting Session currently owns the host.
---@field select fun(self: louiselm.ui.DecisionPrompt, items: unknown[], options: louiselm.ui.DecisionSelectOptions, callback: fun(item?: unknown, index?: integer)): boolean, string?
---@field input fun(self: louiselm.ui.DecisionPrompt, options: louiselm.ui.DecisionInputOptions, callback: fun(value?: string)): boolean, string?
---@field inspect fun(self: louiselm.ui.DecisionPrompt, lines: string[], title: string, on_close: fun()): boolean, string?
---@field hide fun(self: louiselm.ui.DecisionPrompt): boolean
---@field show fun(self: louiselm.ui.DecisionPrompt): boolean, string?
---@field contains_current fun(self: louiselm.ui.DecisionPrompt): boolean
---@field dispose fun(self: louiselm.ui.DecisionPrompt): boolean

local M = {}
local Prompt = {}
Prompt.__index = Prompt

local function single_line(value)
  return (value:gsub("%c", " "))
end

---@param self louiselm.ui.DecisionPrompt
local function clear(self)
  self:hide()
  local buffer = self.buffer
  self.buffer = nil
  self.kind = nil
  self.title = nil
  self.items = nil
  self.callback = nil
  self.saved_view = nil
  if buffer ~= nil and nvim.api.nvim_buf_is_valid(buffer) then
    nvim.api.nvim_buf_delete(buffer, { force = true })
  end
end

---@param self louiselm.ui.DecisionPrompt
local function bind_keys(self)
  local revision = self.revision
  local function finish(cancelled)
    if self.disposed or revision ~= self.revision or not self:contains_current() then
      return
    end
    local callback = self.callback
    local value, index
    if not cancelled and self.kind == "select" then
      index = nvim.api.nvim_win_get_cursor(self.window)[1]
      value = self.items[index]
    elseif not cancelled and self.kind == "input" then
      value = table.concat(nvim.api.nvim_buf_get_lines(self.buffer, 0, -1, false), "\n")
    end
    clear(self)
    if callback ~= nil then
      callback(value, index)
    end
  end
  local function bind(mode, key, cancelled)
    nvim.keymap.set(mode, key, function()
      finish(cancelled)
    end, { buffer = self.buffer, silent = true, nowait = true })
  end
  bind("n", "<Esc>", true)
  if self.kind == "input" then
    bind("n", "<CR>", false)
    bind("i", "<CR>", false)
    bind("i", "<Esc>", true)
  else
    bind("n", "q", true)
    if self.kind == "select" then
      bind("n", "<CR>", false)
    end
  end
end

---Report whether the current window and buffer belong to this presentation.
---@param self louiselm.ui.DecisionPrompt
---@return boolean owned False for hidden, disposed or unfocused presentations.
function Prompt:contains_current()
  return not self.disposed
    and self.buffer ~= nil
    and nvim.api.nvim_get_current_buf() == self.buffer
    and (self.opening or nvim.api.nvim_get_current_win() == self.window)
end

---Hide without answering; retain text, viewport and cursor for the next show.
---@param self louiselm.ui.DecisionPrompt
---@return boolean hidden Whether a visible window was closed.
function Prompt:hide()
  self.revision = self.revision + 1
  local window = self.window
  if
    self.kind == "input"
    and self.buffer == nvim.api.nvim_get_current_buf()
    and (self.opening or window == nvim.api.nvim_get_current_win())
  then
    nvim.cmd.stopinsert()
  end
  self.window = nil
  if window == nil or not nvim.api.nvim_win_is_valid(window) then
    return false
  end
  self.saved_view = nvim.api.nvim_win_call(window, nvim.fn.winsaveview)
  nvim.api.nvim_win_close(window, true)
  return true
end

---Show the retained presentation; an already visible window is left alone.
---@param self louiselm.ui.DecisionPrompt
---@return boolean shown
---@return string? error_message Lifecycle or editor failure; no callback is invoked.
function Prompt:show()
  if self.disposed then
    return false, "decision prompt is disposed"
  end
  if self.buffer == nil or not nvim.api.nvim_buf_is_valid(self.buffer) then
    return false, "no decision prompt is prepared"
  end
  if self.can_show ~= nil and not self.can_show() then
    self:hide()
    return true
  end
  if self.window ~= nil and nvim.api.nvim_win_is_valid(self.window) then
    return true
  end
  local ok, err = pcall(function()
    local host_window = nvim.api.nvim_get_current_win()
    local host_buffer = nvim.api.nvim_get_current_buf()
    self.revision = self.revision + 1
    local revision = self.revision
    bind_keys(self)
    self.opening = true
    self.window = nvim.api.nvim_open_win(self.buffer, true, {
      relative = "editor",
      row = 1,
      col = 2,
      width = math.min(100, math.max(1, nvim.o.columns - 4)),
      height = math.min(nvim.api.nvim_buf_line_count(self.buffer), math.max(1, nvim.o.lines - 4)),
      style = "minimal",
      border = "rounded",
      title = self.title,
    })
    self.opening = false
    if
      self.disposed
      or self.revision ~= revision
      or not nvim.api.nvim_win_is_valid(host_window)
      or nvim.api.nvim_win_get_buf(host_window) ~= host_buffer
      or not self:contains_current()
      or (self.can_show ~= nil and not self.can_show())
    then
      self:hide()
      return
    end
    nvim.api.nvim_set_option_value("wrap", true, { win = self.window })
    if self.saved_view ~= nil then
      nvim.fn.winrestview(self.saved_view)
    end
    if self.kind == "input" then
      nvim.cmd.startinsert()
    else
      nvim.cmd.stopinsert()
    end
  end)
  self.opening = false
  if not ok then
    self:hide()
    return false, type(err) == "string" and "could not open decision prompt" or "decision prompt raised an error"
  end
  return true
end

---@param self louiselm.ui.DecisionPrompt
---@param kind "select"|"input"|"inspect"
---@param lines string[]
---@param title string
---@param callback fun(value?: unknown, index?: integer)
---@param items? unknown[]
---@return boolean shown
---@return string? error_message
local function replace(self, kind, lines, title, callback, items)
  if self.disposed then
    return false, "decision prompt is disposed"
  end
  clear(self)
  local ok, err = pcall(function()
    self.buffer = nvim.api.nvim_create_buf(false, true)
    self.kind = kind
    self.title = single_line(title)
    self.callback = callback
    self.items = items
    nvim.api.nvim_set_option_value("buftype", "nofile", { buf = self.buffer })
    nvim.api.nvim_set_option_value("bufhidden", "hide", { buf = self.buffer })
    nvim.api.nvim_set_option_value("swapfile", false, { buf = self.buffer })
    nvim.api.nvim_buf_set_lines(self.buffer, 0, -1, false, lines)
    nvim.api.nvim_set_option_value("modifiable", kind == "input", { buf = self.buffer })
  end)
  if not ok then
    clear(self)
    return false, type(err) == "string" and "could not prepare decision prompt" or "decision prompt raised an error"
  end
  return self:show()
end

---Replace the current presentation with choices; Enter selects and Escape/q cancel.
---@generic T
---@param self louiselm.ui.DecisionPrompt
---@param items T[] Non-empty choices retained unchanged for the callback.
---@param options louiselm.ui.DecisionSelectOptions
---@param callback fun(item: T?, index?: integer) Called once on an explicit choice or cancellation.
---@return boolean shown
---@return string? error_message Empty choices, formatting or editor failure.
function Prompt:select(items, options, callback)
  if #items == 0 then
    return false, "decision choices must not be empty"
  end
  local lines = {}
  local format = options.format_item or tostring
  local ok, err = pcall(function()
    for _, item in ipairs(items) do
      lines[#lines + 1] = single_line(format(item))
    end
  end)
  if not ok then
    return false, type(err) == "string" and "could not format decision choices" or "decision formatter raised an error"
  end
  return replace(self, "select", lines, options.prompt or "Choose", callback, items)
end

---Replace the presentation with editable text; Enter submits and Escape cancels.
---@param self louiselm.ui.DecisionPrompt
---@param options louiselm.ui.DecisionInputOptions
---@param callback fun(value?: string) Called once; nil means explicit cancellation.
---@return boolean shown
---@return string? error_message Lifecycle or editor failure.
function Prompt:input(options, callback)
  return replace(
    self,
    "input",
    nvim.split(options.default or "", "\n", { plain = true }),
    options.prompt or "Input",
    callback
  )
end

---Replace the presentation with read-only content; q/Escape returns to its caller.
---@param self louiselm.ui.DecisionPrompt
---@param lines string[] Content lines.
---@param title string Inspector title.
---@param on_close fun() Called once on explicit close, never on hide or disposal.
---@return boolean shown
---@return string? error_message Lifecycle or editor failure.
function Prompt:inspect(lines, title, on_close)
  return replace(self, "inspect", lines, title, on_close)
end

---Dispose owned buffers/windows without sending a response; safe to repeat.
---@param self louiselm.ui.DecisionPrompt
---@return boolean disposed Always true.
function Prompt:dispose()
  if not self.disposed then
    self.disposed = true
    clear(self)
  end
  return true
end

---Construct an explicit presentation owner without creating editor resources.
---@param can_show? fun(): boolean Defer display while its requesting Session is not visible.
---@return louiselm.ui.DecisionPrompt prompt
function M.new(can_show)
  return setmetatable({ disposed = false, opening = false, revision = 0, can_show = can_show }, Prompt)
end

return M
