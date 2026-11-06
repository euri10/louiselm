local Apply = require("louiselm.ui.diff.apply")
local Buffer = require("louiselm.ui.diff.buffer")
local Gates = require("louiselm.permission.gates")
local DecisionPrompt = require("louiselm.ui.decision_prompt")

---@class louiselm.ui.Diff
---@field buffer integer? Current diff buffer.
---@field preview louiselm.ui.DiffPreview? Current file preview.
---@field request table? Current ACP permission request.
---@field response (fun(result: unknown): boolean, string?)? Permission response callback.
---@field package prompt? louiselm.ui.DecisionPrompt Owned review or lifetime picker.
---@field package revision integer Invalidates callbacks from previous presentations.
---@field package keys_revision integer Invalidates custom mappings when a review is hidden.
---@field package reviewing boolean Whether the current presentation is the preview.
---@field package can_show? fun(): boolean Whether the requesting Session currently owns the host.
---@field disposed boolean Whether this controller has been disposed.
---@field session_label? string Single-line requesting Session identity, when hosted by Chat.
---@field open fun(self: louiselm.ui.Diff, request: table, respond: (fun(result: unknown, error?: louiselm.acp.JsonRpcError): boolean, string?), session_label?: string): boolean, string? Open a permission request.
---@field accept fun(self: louiselm.ui.Diff): boolean, string? Allow the displayed edit.
---@field reject fun(self: louiselm.ui.Diff): boolean, string? Reject the displayed edit.
---@field close fun(self: louiselm.ui.Diff): boolean Close the displayed diff.
---@field hide fun(self: louiselm.ui.Diff): boolean Hide without answering.
---@field show fun(self: louiselm.ui.Diff): boolean, string? Restore the retained presentation.
---@field contains_current fun(self: louiselm.ui.Diff): boolean Whether this review owns current focus.
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
    content = raw_input.content,
  }
  local old_text = raw_input.old_text
  if old_text == nil then
    old_text = raw_input.oldText
  end
  if old_text == nil then
    old_text = raw_input.old_string
  end
  local new_text = raw_input.new_text or raw_input.newText or raw_input.new_string
  if old_text ~= nil then
    edit.replacement = { old = old_text, new = new_text, all = raw_input.replace_all == true }
  else
    edit.content = edit.content or new_text
  end
  if edit.diff ~= nil or edit.content ~= nil or edit.replacement ~= nil then
    return edit
  end
  -- When rawInput has no reviewable text, use the ACP diff content entry:
  -- whole-file text without old text, otherwise an exact fragment replacement.
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
  local prompt = self.prompt
  self.prompt = nil
  self.buffer = nil
  self.preview = nil
  self.request = nil
  self.response = nil
  self.session_label = nil
  self.revision = self.revision + 1
  self.keys_revision = self.keys_revision + 1
  self.reviewing = false
  if prompt ~= nil then
    prompt:dispose()
  end
end

local function report_failure(ok)
  if not ok then
    nvim.notify("louiselm: diff decision failed", nvim.log.levels.ERROR)
  end
end

---@param self louiselm.ui.Diff
local function bind_review_keys(self)
  local prompt, buffer = self.prompt, self.buffer
  if prompt == nil or buffer == nil then
    return
  end
  self.keys_revision = self.keys_revision + 1
  local revision = self.keys_revision
  for key, decision in pairs({ a = "allow", d = "deny" }) do
    nvim.keymap.set("n", key, function()
      if
        self.keys_revision == revision
        and self.prompt == prompt
        and self.buffer == buffer
        and prompt:contains_current()
      then
        local ok
        if decision == "allow" then
          ok = self:accept()
        else
          ok = self:reject()
        end
        if self.preview ~= nil then
          report_failure(ok)
        end
      end
    end, {
      buffer = buffer,
      silent = true,
      nowait = true,
      desc = decision == "allow" and "Allow louiselm file edit" or "Reject louiselm file edit",
    })
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
  clear(self)
  local call_ok, sent, send_error = pcall(callback, result)
  if not call_ok then
    return false, tostring(sent)
  end
  if not sent then
    return false, send_error or "permission response could not be sent"
  end
  return true
end

---@param self louiselm.ui.Diff
---@return boolean opened
---@return string? error_message
local function show_preview(self)
  local prompt, preview = self.prompt, self.preview
  if prompt == nil or preview == nil then
    return false, "no diff permission request is open"
  end
  local instruction = (self.session_label and self.session_label .. " · " or "")
    .. "Review proposed edit:  a = accept, d/q/Esc = reject"
  self.revision = self.revision + 1
  local revision = self.revision
  local opened, err = prompt:inspect(Buffer.render_lines(preview, instruction), instruction, function()
    if self.prompt == prompt and self.revision == revision then
      local ok = self:reject()
      if self.preview ~= nil then
        report_failure(ok)
      end
    end
  end)
  if not opened then
    return false, err
  end
  local buffer = prompt.buffer
  if buffer == nil then
    return false, "diff preview has no buffer"
  end
  self.buffer = buffer
  self.reviewing = true
  nvim.api.nvim_set_option_value("filetype", "diff", { buf = buffer })
  bind_review_keys(self)
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
  local prompt, request = self.prompt, self.request
  if self.preview == nil or prompt == nil or request == nil then
    return false, "no diff permission request is open"
  end
  local options = Gates.decision_options(request, decision)
  if #options == 1 then
    local result = Gates.select_response(options[1])
    if result == nil then
      return false, "permission option has no identifier"
    end
    return send_response(self, result)
  end
  if #options > 1 then
    self.reviewing = false
    self.revision = self.revision + 1
    local revision = self.revision
    local opened, err = prompt:select(options, {
      prompt = (self.session_label and self.session_label .. " · " or "") .. "louiselm file edit " .. decision .. ": ",
      format_item = option_label,
    }, function(option)
      local function deliver()
        if self.disposed or self.prompt ~= prompt or self.revision ~= revision then
          return
        end
        if option == nil then
          local restored = show_preview(self)
          if not restored then
            send_response(self, { outcome = { outcome = "cancelled" } })
            report_failure(false)
          end
        else
          local result = Gates.select_response(option)
          if result ~= nil then
            -- The response owner reports transport failures; this callback owns only UI transitions.
            send_response(self, result)
          end
        end
      end
      if nvim.in_fast_event() then
        nvim.schedule(deliver)
      else
        deliver()
      end
    end)
    if not opened and self.prompt == prompt then
      local restored, restore_error = show_preview(self)
      if not restored then
        send_response(self, { outcome = { outcome = "cancelled" } })
        return false, restore_error
      end
    end
    self.buffer = self.prompt and self.prompt.buffer or nil
    return opened, err
  end
  local result = Gates.response(request, decision)
  if result == nil then
    return false, "permission request has no matching " .. decision .. " option"
  end
  return send_response(self, result)
end

---Create a diff review controller without opening a buffer.
---@param can_show? fun(): boolean Defer display while the requesting Session is not visible.
---@return louiselm.ui.Diff diff Diff controller.
function M.new(can_show)
  return setmetatable(
    { disposed = false, revision = 0, keys_revision = 0, reviewing = false, can_show = can_show },
    Diff
  )
end

---Open a file-edit permission request for review.
---@param self louiselm.ui.Diff
---@param request table ACP permission request data containing `operation`, `toolCall`, and `options`.
---@param response fun(result: unknown, error?: louiselm.acp.JsonRpcError): boolean, string? Callback for the ACP response.
---@param session_label? string Single-line requesting Session identity, when hosted by Chat.
---@return boolean opened
---@return string? error_message Validation or filesystem error.
function Diff:open(request, response, session_label)
  if self.disposed then
    return false, "diff UI is disposed"
  end
  if self.preview ~= nil then
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
  self.request = request
  self.preview = preview
  self.response = response
  self.session_label = session_label
  self.prompt = DecisionPrompt.new(self.can_show)
  local opened, err = show_preview(self)
  if not opened then
    clear(self)
  end
  return opened, err
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
  if self.preview == nil then
    return false
  end
  clear(self)
  return true
end

---Hide the active preview or lifetime picker without resolving its request.
---@param self louiselm.ui.Diff
---@return boolean hidden Whether a visible window was closed.
function Diff:hide()
  self.keys_revision = self.keys_revision + 1
  return self.prompt ~= nil and self.prompt:hide()
end

---Restore the retained preview or lifetime picker, including its cursor.
---@param self louiselm.ui.Diff
---@return boolean shown
---@return string? error_message Lifecycle or editor failure.
function Diff:show()
  if self.prompt == nil then
    return false, "no diff permission request is open"
  end
  local shown, err = self.prompt:show()
  if shown and self.reviewing then
    bind_review_keys(self)
  end
  return shown, err
end

---Report whether this review owns the focused window and buffer.
---@param self louiselm.ui.Diff
---@return boolean owned False when hidden, closed or disposed.
function Diff:contains_current()
  return self.prompt ~= nil and self.prompt:contains_current()
end

---Dispose the controller and close any open review.
---@param self louiselm.ui.Diff
---@return boolean disposed Always true.
function Diff:dispose()
  if self.disposed then
    return true
  end
  self.disposed = true
  self:close()
  return true
end

return M
