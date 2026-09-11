local Diff = require("louiselm.ui.diff")
local Gates = require("louiselm.permission.gates")
local Inspector = require("louiselm.ui.chat.inspector")
local Picker = require("louiselm.ui.picker")

---@diagnostic disable-next-line: undefined-global -- Neovim injects its runtime API.
local nvim = vim

---@class louiselm.ui.DecisionsOptions
---@field is_live fun(session: louiselm.session.Session): boolean Whether this Session has a usable attached view.
---@field report_error fun(session: louiselm.session.Session, message: string) Append an error to its transcript.
---@field resolved fun(session: louiselm.session.Session, request_id: unknown) Resolve its Attention item after a sent response.

---@class louiselm.ui.Decisions: louiselm.ui.DecisionsOptions
---@field package diff louiselm.ui.Diff Owned file-edit review controller.
---@field package active? louiselm.ui.ChatDecision Current authority, distinct from a pending picker callback.
---@field package picker? louiselm.ui.ChatDecision Provider-owned picker awaiting its callback after retirement.
---@field package queue louiselm.ui.ChatDecision[] Pending decisions across attached Sessions.
---@field package disposed boolean Whether this owner has been disposed.
---@field request fun(self: louiselm.ui.Decisions, session: louiselm.session.Session, data: unknown, respond?: fun(result: unknown, error?: louiselm.acp.JsonRpcError): boolean, string?) Queue one request.
---@field cancel fun(self: louiselm.ui.Decisions, session: louiselm.session.Session, request_ids: unknown) Retire Session-cancelled requests.
---@field retire fun(self: louiselm.ui.Decisions, session: louiselm.session.Session) Retire a closing Session before selecting the survivor.
---@field present_next fun(self: louiselm.ui.Decisions) Present pending work after host changes finish.
---@field is_active fun(self: louiselm.ui.Decisions): boolean Report active decision authority.
---@field dispose fun(self: louiselm.ui.Decisions): boolean Dispose owned resources.

local M = {}
local Decisions = {}
Decisions.__index = Decisions

---@param value unknown
---@return string? text
local function field(value, name)
  if type(value) == "table" and type(value[name]) == "string" and value[name] ~= "" then
    return value[name]
  end
  return nil
end

---@param option unknown
---@return string? identifier
---@return string label
local function permission_option(option)
  if type(option) == "string" and option ~= "" then
    return option, option
  end
  if type(option) ~= "table" then
    return nil, "invalid option"
  end
  local identifier = option.optionId or option.option_id
  if type(identifier) ~= "string" or identifier == "" then
    return nil, "invalid option"
  end
  local label = option.name or option.kind or identifier
  if type(label) ~= "string" or label == "" then
    label = identifier
  end
  return identifier, label
end

---@param value unknown
---@return unknown[]? options
local function permission_options(value)
  if type(value) ~= "table" then
    return nil
  end
  local options = {}
  for _, option in ipairs(value) do
    local identifier = permission_option(option)
    if identifier ~= nil then
      options[#options + 1] = option
    end
  end
  local ordered = {}
  local rejections = {}
  for _, option in ipairs(Gates.decision_options({ options = options }, "deny")) do
    ordered[#ordered + 1] = option
    rejections[option] = true
  end
  for _, option in ipairs(options) do
    if not rejections[option] then
      ordered[#ordered + 1] = option
    end
  end
  return ordered
end

---@param data table ACP permission data; raw tool details are display-only.
---@return string prompt
local function permission_prompt(data)
  local operation = data.operation
  local kind = type(operation) == "table" and operation.kind or "unknown"
  if kind == "command" and type(operation.command) == "table" then
    local encoded_ok, encoded_command = pcall(nvim.json.encode, operation.command)
    if encoded_ok and type(encoded_command) == "string" then
      return "louiselm permission (command): " .. encoded_command .. " "
    end
  end
  local tool_call = data.toolCall or data.tool_call
  local title = field(tool_call, "title")
  if title ~= nil then
    return "louiselm permission: " .. nvim.json.encode(title) .. " "
  end
  if type(kind) ~= "string" or kind == "" then
    kind = "unknown"
  end
  return "louiselm permission (" .. kind .. ", details unavailable): "
end

---@param self louiselm.ui.Decisions
---@param session louiselm.session.Session
---@return boolean hosted Whether this session can still host a permission decision.
local function hosts_view(self, session)
  return not self.disposed and self.is_live(session)
end

---@param self louiselm.ui.Decisions
---@param session louiselm.session.Session
---@param respond fun(result: unknown, error?: louiselm.acp.JsonRpcError): boolean, string?
---@param result table
---@return boolean sent
local function send_permission_response(self, session, respond, result)
  if not hosts_view(self, session) then
    -- A choice made after the session is gone must grant nothing, but the request still
    -- has to be answered or the agent blocks on it for the rest of the session.
    local closed_ok, closed = pcall(respond, { outcome = { outcome = "cancelled" } })
    return closed_ok and closed == true
  end
  local call_ok, sent, send_error = pcall(respond, result)
  if not call_ok or not sent then
    local message = call_ok and (send_error or "permission response could not be sent") or tostring(sent)
    self.report_error(session, message)
    return false
  end
  return true
end

---@param self louiselm.ui.Decisions
---@param session louiselm.session.Session
---@param respond? fun(result: unknown, error?: louiselm.acp.JsonRpcError): boolean, string?
---@return boolean sent
local function cancel_permission(self, session, respond)
  if type(respond) ~= "function" then
    self.report_error(session, "permission request has no response callback")
    return false
  end
  return send_permission_response(self, session, respond, { outcome = { outcome = "cancelled" } })
end

---@param self louiselm.ui.Decisions
---@param session louiselm.session.Session
---@param data table
---@param respond fun(result: unknown, error?: louiselm.acp.JsonRpcError): boolean, string? Decision responder.
local function prompt_permission(self, session, data, respond)
  local options = permission_options(data.options)
  if options == nil or #options == 0 then
    cancel_permission(self, session, respond)
    return
  end
  local decision = self.active
  local tool_call = data.toolCall or data.tool_call
  local details = {}
  if type(tool_call) == "table" then
    options[#options + 1] = details
  end
  Picker.select(options, {
    prompt = permission_prompt(data),
    format_item = function(option)
      if option == details then
        return "View request details"
      end
      local _, label = permission_option(option)
      return label
    end,
  }, function(choice)
    if choice == details and decision ~= nil then
      if decision.answered or not hosts_view(self, session) then
        cancel_permission(self, session, respond)
        return
      end
      decision.details_window = Inspector.open(nvim.split(nvim.inspect(tool_call), "\n", { plain = true }), function()
        nvim.schedule(function()
          decision.details_window = nil
          if decision.answered then
            -- Release the picker slot after retirement without reporting a stale answer as an error.
            respond({ outcome = { outcome = "cancelled" } })
            return
          end
          if self.active ~= decision or not hosts_view(self, session) then
            cancel_permission(self, session, respond)
            return
          end
          local ok, err = pcall(prompt_permission, self, session, data, respond)
          if not ok then
            self.report_error(session, "permission picker failed: " .. tostring(err))
            cancel_permission(self, session, respond)
          end
        end)
      end, "Request details · q/Esc: back to choices")
      return
    end
    if choice == nil then
      cancel_permission(self, session, respond)
      return
    end
    local result = Gates.select_response(choice)
    if result == nil then
      cancel_permission(self, session, respond)
      return
    end
    send_permission_response(self, session, respond, result)
  end)
end

---@class louiselm.ui.ChatDecision
---@field session louiselm.session.Session Session that asked for a decision.
---@field data table ACP permission request data.
---@field respond fun(result: unknown, error?: louiselm.acp.JsonRpcError): boolean, string? ACP responder.
---@field answered boolean Whether this decision was already answered or cancelled.
---@field details_window? integer Scrollable request inspector, if open.

---@param decision louiselm.ui.ChatDecision
local function close_permission_details(decision)
  local window = decision.details_window
  decision.details_window = nil
  if window ~= nil and nvim.api.nvim_win_is_valid(window) then
    nvim.api.nvim_win_close(window, true)
  end
end

---@type fun(self: louiselm.ui.Decisions)
local pump_decisions

---Wrap one ACP responder so the decision slot is released exactly once, after the answer.
---The slot is compared by identity: a stale picker answering late must not release the
---decision that replaced it.
---@param self louiselm.ui.Decisions
---@param decision louiselm.ui.ChatDecision Decision being opened.
---@return fun(result: unknown, error?: louiselm.acp.JsonRpcError): boolean, string? respond
local function decision_responder(self, decision)
  return function(result, rpc_error)
    if decision.answered then
      return false, "permission decision was already answered"
    end
    decision.answered = true
    local sent, send_error = decision.respond(result, rpc_error)
    if sent then
      self.resolved(decision.session, decision.data.request_id)
    end
    if self.active == decision then
      self.active = nil
      pump_decisions(self)
    end
    return sent, send_error
  end
end

---Open one decision: a diff review for file edits, a picker for everything else.
---@param self louiselm.ui.Decisions
---@param decision louiselm.ui.ChatDecision Decision to present.
local function open_decision(self, decision)
  local session = decision.session
  local data = decision.data
  local respond = decision_responder(self, decision)
  if not hosts_view(self, session) then
    cancel_permission(self, session, respond)
    return
  end
  if type(data.operation) == "table" and data.operation.kind == "file_edit" then
    local opened, open_error = self.diff:open(data, respond)
    if not opened then
      self.report_error(session, (open_error or "could not open diff review"))
      cancel_permission(self, session, respond)
    end
    return
  end
  -- A select provider that throws would otherwise strand every later decision behind it.
  self.picker = decision
  local function picker_respond(result, rpc_error)
    if self.picker == decision then
      self.picker = nil
    end
    local sent, send_error = respond(result, rpc_error)
    pump_decisions(self)
    return sent, send_error
  end
  local call_ok, prompt_error = pcall(prompt_permission, self, session, data, picker_respond)
  if not call_ok then
    self.report_error(session, "permission picker failed: " .. tostring(prompt_error))
    cancel_permission(self, session, picker_respond)
  end
end

---Present the next queued decision while none is open.
---One picker or review hosts one decision at a time across every attached session,
---because an async select provider closes the picker a new one replaces, which answers
---that request without the user choosing and can leave the replacement unanswered.
---@param self louiselm.ui.Decisions
pump_decisions = function(self)
  while self.active == nil and (self.picker == nil or self.disposed) do
    local decision = table.remove(self.queue, 1)
    if decision == nil then
      return
    end
    if decision.answered then
      -- Its turn was cancelled while it waited; the session already answered it.
    elseif self.disposed then
      -- Disposal answers what it can no longer host: an unanswered request blocks its agent.
      decision.answered = true
      cancel_permission(self, decision.session, decision.respond)
    else
      self.active = decision
      open_decision(self, decision)
    end
  end
end

---Retire requests already answered by Session cancellation; malformed ID lists do nothing.
---@param self louiselm.ui.Decisions
---@param session louiselm.session.Session Session whose requests were cancelled.
---@param request_ids unknown Identifiers reported by the session.
function Decisions:cancel(session, request_ids)
  if type(request_ids) ~= "table" then
    return
  end
  local cancelled = {}
  for _, request_id in ipairs(request_ids) do
    cancelled[request_id] = true
  end
  for _, decision in ipairs(self.queue) do
    if decision.session == session and cancelled[decision.data.request_id] then
      decision.answered = true
    end
  end
  local active = self.active
  if active == nil or active.session ~= session or not cancelled[active.data.request_id] then
    return
  end
  active.answered = true
  self.active = nil
  close_permission_details(active)
  -- A review is ours to close; an open picker is not, and answering it later reports
  -- that the decision was already answered.
  self.diff:close()
  pump_decisions(self)
end

---Queue a request or cancel malformed data; report missing responders through the error callback.
---@param self louiselm.ui.Decisions
---@param session louiselm.session.Session Session that asked for a decision.
---@param data unknown ACP permission request data.
---@param respond? fun(result: unknown, error?: louiselm.acp.JsonRpcError): boolean, string? ACP responder.
function Decisions:request(session, data, respond)
  if type(respond) ~= "function" then
    self.report_error(session, "permission request has no response callback")
    return
  end
  if type(data) ~= "table" then
    cancel_permission(self, session, respond)
    return
  end
  self.queue[#self.queue + 1] = { session = session, data = data, respond = respond, answered = false }
  pump_decisions(self)
end

---Retire a detached Session before disposing it; call present_next after selecting the survivor.
---@param self louiselm.ui.Decisions
---@param session louiselm.session.Session
function Decisions:retire(session)
  -- Retire authority now; a provider-owned picker may return much later. Keep
  -- other Sessions queued until that callback, so a new selector cannot toggle it.
  for index = #self.queue, 1, -1 do
    local decision = self.queue[index]
    if decision.session == session then
      table.remove(self.queue, index)
      if not decision.answered then
        decision.answered = true
        cancel_permission(self, session, decision.respond)
      end
    end
  end
  local active = self.active
  if active ~= nil and active.session == session then
    self.active = nil
    active.answered = true
    close_permission_details(active)
    cancel_permission(self, session, active.respond)
    self.diff:close()
  end
end

---Present queued work after the caller finishes changing the host view.
---@param self louiselm.ui.Decisions
function Decisions:present_next()
  pump_decisions(self)
end

---Report whether a decision owns authority, for competing command-picker refusal.
---@param self louiselm.ui.Decisions
---@return boolean active
function Decisions:is_active()
  return self.active ~= nil
end

---Dispose owned reviews/details and cancel queued requests; late picker choices grant nothing.
---@param self louiselm.ui.Decisions
---@return boolean disposed Always true.
function Decisions:dispose()
  if self.disposed then
    return true
  end
  self.disposed = true
  self.diff:dispose()
  if self.active ~= nil then
    close_permission_details(self.active)
  end
  self.active = nil
  pump_decisions(self)
  return true
end

---Construct a decision owner with explicit host callbacks; creates no editor resources.
---@param options louiselm.ui.DecisionsOptions
---@return louiselm.ui.Decisions decisions
function M.new(options)
  return setmetatable({
    is_live = options.is_live,
    report_error = options.report_error,
    resolved = options.resolved,
    diff = Diff.new(),
    queue = {},
    disposed = false,
  }, Decisions)
end

return M
