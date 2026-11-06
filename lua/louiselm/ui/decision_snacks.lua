---@diagnostic disable-next-line: undefined-global -- Neovim owns this optional UI boundary.
local nvim = vim

-- Only the consumed Snacks surface is declared here; runtime types do not require
-- an optional plugin to be installed in every LouiseLM user's workspace.
---@class louiselm.ui.DecisionSnacksPicker
---@field closed? boolean
---@field input {get: fun(self: table): string}
---@field list {cursor: integer, top: integer, set_target: fun(self: table, cursor: integer, top: integer)}
---@field layout {closed?: boolean, close: fun(self: table)}
---@field is_focused fun(self: louiselm.ui.DecisionSnacksPicker): boolean
---@field focus fun(self: louiselm.ui.DecisionSnacksPicker)
---@field close fun(self: louiselm.ui.DecisionSnacksPicker)

---@class louiselm.ui.DecisionSnacks
---@field package provider fun(items: integer[], options: table, callback: fun(index?: integer)): louiselm.ui.DecisionSnacksPicker?
---@field package labels string[]
---@field package title string
---@field package callback fun(index?: integer)
---@field package can_show fun(): boolean
---@field package revision integer
---@field package picker? louiselm.ui.DecisionSnacksPicker
---@field package pattern string
---@field package cursor integer
---@field package top integer
---@field show fun(self: louiselm.ui.DecisionSnacks): boolean, string?
---@field hide fun(self: louiselm.ui.DecisionSnacks): boolean
---@field contains_current fun(self: louiselm.ui.DecisionSnacks): boolean

local M = {}
local Select = {}
Select.__index = Select

---@param picker louiselm.ui.DecisionSnacksPicker
local function close(picker)
  if picker.closed and picker.layout.closed then
    return
  end
  -- Snacks closes asynchronously and stops insert mode even in a foreign buffer.
  -- Close its whole layout now (including splits), before restoring foreign input.
  local foreign_insert = not picker:is_focused()
    and nvim.api.nvim_get_mode().mode:sub(1, 1) == "i"
    and nvim.api.nvim_get_option_value("buftype", { buf = 0 }) ~= "prompt"
  local window, buffer = nvim.api.nvim_get_current_win(), nvim.api.nvim_get_current_buf()
  if foreign_insert then
    -- stopinsert takes effect after this callback returns. Starting insert mode
    -- before the actual transition cannot cancel that pending exit.
    nvim.api.nvim_create_autocmd("ModeChanged", {
      pattern = "i*:n",
      once = true,
      callback = function()
        nvim.schedule(function()
          if nvim.api.nvim_get_current_win() == window and nvim.api.nvim_get_current_buf() == buffer then
            nvim.cmd.startinsert()
          end
        end)
      end,
    })
  end
  if not picker.closed then
    picker:close()
  end
  if not picker.layout.closed then
    picker.layout:close()
  end
end

---Report focus across the provider's input, list and preview windows.
---@param self louiselm.ui.DecisionSnacks
---@return boolean owned
function Select:contains_current()
  return self.picker ~= nil and not self.picker.closed and self.picker:is_focused()
end

---Retain search and list position; retire callbacks before closing provider UI.
---@param self louiselm.ui.DecisionSnacks
---@return boolean hidden
function Select:hide()
  self.revision = self.revision + 1
  local picker = self.picker
  self.picker = nil
  if picker == nil or picker.closed then
    return false
  end
  self.pattern = picker.input:get()
  self.cursor, self.top = picker.list.cursor, picker.list.top
  close(picker)
  return true
end

---Open a fresh provider view of the same pending choice, never a global resume.
---@param self louiselm.ui.DecisionSnacks
---@return boolean shown
---@return string? error_message Provider creation failure; no choice is delivered.
function Select:show()
  if self.picker ~= nil then
    return true
  end
  self.revision = self.revision + 1
  local revision = self.revision
  local host_window, host_buffer = nvim.api.nvim_get_current_win(), nvim.api.nvim_get_current_buf()
  local creating = true
  local choices = {}
  for index = 1, #self.labels do
    choices[index] = index
  end
  nvim.cmd.stopinsert()
  local ok, picker = pcall(self.provider, choices, {
    prompt = self.title,
    format_item = function(index)
      return self.labels[index]
    end,
    snacks = {
      auto_close = false,
      auto_confirm = false,
      enter = false,
      pattern = self.pattern,
      on_show = function(shown)
        if creating and self.picker == nil and revision == self.revision then
          self.picker = shown
        end
        -- Snacks stores select options in its global resume history. A resumed
        -- clone must never enter or display another Session's retired decision.
        if revision ~= self.revision or shown ~= self.picker then
          close(shown)
          return
        end
        if not self.can_show() then
          self:hide()
          return
        end
        shown.list:set_target(self.cursor, self.top)
        shown:focus()
        if
          revision ~= self.revision
          or not nvim.api.nvim_win_is_valid(host_window)
          or nvim.api.nvim_win_get_buf(host_window) ~= host_buffer
          or not self.can_show()
        then
          self:hide()
        end
      end,
    },
  }, function(index)
    if revision ~= self.revision or not self.can_show() then
      return
    end
    if self.picker ~= nil then
      close(self.picker)
    end
    -- Deliver after Snacks' layout-close callback, which can stop insert mode.
    -- A follow-up text prompt must not inherit that deferred teardown.
    nvim.schedule(function()
      if revision == self.revision and self.can_show() then
        self:hide()
        self.callback(index)
      end
    end)
  end)
  creating = false
  if not ok or picker == nil then
    self:hide()
    return false, ok and "decision picker did not open" or "could not open decision picker"
  end
  if revision ~= self.revision then
    close(picker)
  else
    self.picker = picker
  end
  return true
end

---Construct an optional bridge only for the operator's active Snacks select provider.
---@param labels string[] Preformatted, single-line choice labels.
---@param title string Single-line requester title.
---@param callback fun(index?: integer) Original item index; nil means explicit cancellation.
---@param can_show fun(): boolean Owner visibility guard, also checked for globally resumed clones.
---@return louiselm.ui.DecisionSnacks? selection Nil when Snacks is absent or not the configured provider.
function M.new(labels, title, callback, can_show)
  local provider = package.loaded["snacks.picker"]
  if type(provider) ~= "table" or nvim.ui.select ~= provider.select then
    return nil
  end
  return setmetatable({
    provider = provider.select,
    labels = labels,
    title = title,
    callback = callback,
    can_show = can_show,
    revision = 0,
    pattern = "",
    cursor = 1,
    top = 1,
  }, Select)
end

return M
