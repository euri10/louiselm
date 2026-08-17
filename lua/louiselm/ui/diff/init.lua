local Apply = require("louiselm.ui.diff.apply")
local Buffer = require("louiselm.ui.diff.buffer")
local Gates = require("louiselm.permission.gates")
local Picker = require("louiselm.ui.picker")

---@class louiselm.ui.Diff
---@field buffer integer? Current diff buffer.
---@field preview louiselm.ui.DiffPreview? Current file preview.
---@field request table? Current ACP permission request.
---@field response fun(result: unknown): boolean, string? Permission response callback.
---@field previous_buffer integer? Buffer focused before opening the diff.
---@field disposed boolean Whether this controller has been disposed.
---@field open fun(self: louiselm.ui.Diff, request: table, respond: fun(result: unknown, error?: louiselm.acp.JsonRpcError): boolean, string?): boolean, string? Open a permission request.
---@field accept fun(self: louiselm.ui.Diff): boolean, string? Allow the displayed edit.
---@field reject fun(self: louiselm.ui.Diff): boolean, string? Reject the displayed edit.
---@field close fun(self: louiselm.ui.Diff): boolean Close the displayed diff.
---@field dispose fun(self: louiselm.ui.Diff): boolean Dispose the controller.

local M = {}
local Diff = {}
Diff.__index = Diff

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

---@param tool_call unknown ACP tool call.
---@return table? entry First ACP diff content entry, when the agent sent one.
local function acp_diff_entry(tool_call)
  if type(tool_call) ~= "table" or type(tool_call.content) ~= "table" then
    return nil
  end
  for _, entry in ipairs(tool_call.content) do
    if type(entry) == "table" and entry.type == "diff" and type(entry.newText) == "string" then
      return entry
    end
  end
  return nil
end

---@param request table ACP permission request data.
---@return louiselm.ui.DiffEdit? edit
---@return string? error_message
local function request_edit(request)
  if type(request) ~= "table" then
    return nil, "diff permission request must be a table"
  end
  local operation = request.operation
  if type(operation) ~= "table" or operation.kind ~= "file_edit" then
    return nil, "diff UI only supports file edits"
  end
  local tool_call = request.toolCall or request.tool_call
  local raw_input = type(tool_call) == "table" and (tool_call.rawInput or tool_call.raw_input) or nil
  if type(raw_input) ~= "table" then
    raw_input = tool_call
  end
  if type(raw_input) ~= "table" then
    raw_input = {}
  end
  local edit = {
    path = raw_input.path or raw_input.filePath or raw_input.file_path or operation.path,
    diff = raw_input.diff or operation.diff,
    content = raw_input.content or raw_input.newText or raw_input.new_text,
  }
  if edit.diff ~= nil or edit.content ~= nil then
    return edit
  end
  -- An agent that forwards its edit tool arguments verbatim leaves nothing reviewable in
  -- rawInput and carries the text in the ACP diff content entry instead: whole-file text
  -- when it has no old text, otherwise the exact fragment it expects to replace.
  local entry = acp_diff_entry(tool_call)
  if entry == nil then
    return edit
  end
  edit.path = edit.path or entry.path
  if type(entry.oldText) == "string" and entry.oldText ~= "" then
    edit.replacement = { old = entry.oldText, new = entry.newText, all = raw_input.replace_all == true }
  else
    edit.content = entry.newText
  end
  return edit
end

---@param self louiselm.ui.Diff
local function clear(self)
  local buffer = self.buffer
  -- Read every field before closing: wiping the review buffer runs the BufWipeout
  -- handler below, which clears them all.
  local previous_buffer = self.previous_buffer
  self.buffer = nil
  self.preview = nil
  self.response = nil
  self.previous_buffer = nil
  if buffer ~= nil then
    Buffer.close(buffer)
  end
  if previous_buffer ~= nil and nvim.api.nvim_buf_is_valid(previous_buffer) then
    nvim.api.nvim_set_current_buf(previous_buffer)
  end
end

---@param self louiselm.ui.Diff
---@param result table
---@return boolean responded
---@return string? error_message
local function send_response(self, result)
  local callback = self.response
  if callback == nil or self.preview == nil then
    return false, "no diff permission request is open"
  end
  local call_ok, sent, send_error = pcall(callback, result)
  if not call_ok then
    return false, tostring(sent)
  end
  if not sent then
    return false, send_error or "permission response could not be sent"
  end
  clear(self)
  return true
end

---@param option unknown
---@return string
local function option_label(option)
  if type(option) == "string" then
    return option
  end
  if type(option) ~= "table" then
    return "invalid option"
  end
  return option.name or option.kind or option.optionId or option.option_id or "invalid option"
end

---@param self louiselm.ui.Diff
---@param decision louiselm.permission.Decision
---@return boolean responded
---@return string? error_message
local function respond(self, decision)
  local options = Gates.decision_options(self.request, decision)
  if #options == 1 then
    local result = Gates.select_response(options[1])
    if result == nil then
      return false, "permission option has no identifier"
    end
    return send_response(self, result)
  end
  if #options > 1 then
    Picker.select(options, {
      prompt = "louiselm file edit " .. decision .. ": ",
      format_item = option_label,
    }, function(option)
      if option ~= nil and not self.disposed then
        local result = Gates.select_response(option)
        if result ~= nil then
          send_response(self, result)
        end
      end
    end)
    return true
  end
  local result = Gates.response(self.request, decision)
  if result == nil then
    return false, "permission request has no matching " .. decision .. " option"
  end
  return send_response(self, result)
end

---Create a diff review controller without opening a buffer.
---@return louiselm.ui.Diff diff Diff controller.
function M.new()
  return setmetatable({ buffer = nil, preview = nil, response = nil, previous_buffer = nil, disposed = false }, Diff)
end

---Open a file-edit permission request for review.
---@param self louiselm.ui.Diff
---@param request table ACP permission request data containing `operation`, `toolCall`, and `options`.
---@param response fun(result: unknown, error?: louiselm.acp.JsonRpcError): boolean, string? Callback for the ACP response.
---@return boolean opened
---@return string? error_message Validation or filesystem error.
function Diff:open(request, response)
  if self.disposed then
    return false, "diff UI is disposed"
  end
  if self.buffer ~= nil then
    return false, "a diff review is already open"
  end
  if type(response) ~= "function" then
    return false, "diff permission response must be a function"
  end
  local edit, edit_error = request_edit(request)
  if edit == nil then
    return false, edit_error
  end
  local preview, preview_error = Apply.preview(edit)
  if preview == nil then
    return false, preview_error
  end
  self:close()
  self.request = request
  self.previous_buffer = nvim.api.nvim_get_current_buf()
  local buffer, buffer_error = Buffer.open(preview)
  if buffer == nil then
    self.previous_buffer = nil
    return false, buffer_error
  end
  self.buffer = buffer
  self.preview = preview
  self.response = response
  nvim.api.nvim_create_autocmd("BufWipeout", {
    buffer = buffer,
    once = true,
    callback = function()
      -- The review buffer is `bufhidden=wipe`, so navigating away abandons the review.
      -- Field teardown happens before the response so answering cannot re-enter this
      -- buffer, and the previously focused buffer is left alone: the user already moved.
      local pending = self.response
      self.buffer = nil
      self.preview = nil
      self.response = nil
      self.previous_buffer = nil
      if pending == nil then
        return
      end
      local call_ok, sent = pcall(pending, { outcome = { outcome = "cancelled" } })
      if not call_ok or not sent then
        nvim.notify("louiselm: abandoned diff review could not be cancelled", nvim.log.levels.WARN)
      end
    end,
  })
  nvim.keymap.set("n", "a", function()
    self:accept()
  end, { buffer = buffer, silent = true, nowait = true, desc = "Allow louiselm file edit" })
  nvim.keymap.set("n", "d", function()
    self:reject()
  end, { buffer = buffer, silent = true, nowait = true, desc = "Reject louiselm file edit" })
  nvim.keymap.set("n", "q", function()
    self:reject()
  end, { buffer = buffer, silent = true, nowait = true, desc = "Reject louiselm file edit" })
  return true
end

---Allow the displayed edit through the ACP permission response.
---@param self louiselm.ui.Diff
---@return boolean responded
---@return string? error_message Response or lifecycle error.
function Diff:accept()
  return respond(self, "allow")
end

---Reject the displayed edit through the ACP permission response.
---@param self louiselm.ui.Diff
---@return boolean responded
---@return string? error_message Response or lifecycle error.
function Diff:reject()
  return respond(self, "deny")
end

---Close the current review without answering its permission request.
---@param self louiselm.ui.Diff
---@return boolean closed True when a review was open.
function Diff:close()
  if self.buffer == nil then
    return false
  end
  clear(self)
  return true
end

---Dispose the controller and close any open review.
---@param self louiselm.ui.Diff
---@return boolean disposed Always true.
function Diff:dispose()
  if self.disposed then
    return true
  end
  if self.buffer ~= nil then
    self:reject()
  end
  self.disposed = true
  self:close()
  return true
end

return M
