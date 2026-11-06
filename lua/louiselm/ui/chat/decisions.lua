local Diff = require("louiselm.ui.diff")
local DecisionPrompt = require("louiselm.ui.decision_prompt")
local Gates = require("louiselm.permission.gates")
local Interactions = require("louiselm.ui.chat.interactions")

---@diagnostic disable-next-line: undefined-global -- Neovim injects its runtime API.
local nvim = vim

---@class louiselm.ui.DecisionsOptions
---@field is_live fun(session: louiselm.session.Session): boolean Whether this Session has a usable attached view.
---@field is_visible fun(session: louiselm.session.Session): boolean Whether the current host buffer belongs to this Session.
---@field report_error fun(session: louiselm.session.Session, message: string) Append an error to its transcript.
---@field resolved fun(session: louiselm.session.Session, request_id: unknown) Resolve its Attention item after a sent response.

---@class louiselm.ui.Decisions: louiselm.ui.DecisionsOptions
---@field package active? louiselm.ui.ChatDecision Currently presented request.
---@field package queue louiselm.ui.ChatDecision[] All pending decisions, including the active request.
---@field package disposed boolean Whether this owner has been disposed.
---@field package presenting boolean Prevent recursive presentation during synchronous cancellation.
---@field package scheduled boolean Whether a focus check is already queued.
---@field package suspended? table Opaque navigation hold; only its matching completion releases it.
---@field package observer? integer Owned focus observer.
---@field request fun(self: louiselm.ui.Decisions, session: louiselm.session.Session, data: unknown, respond?: (fun(result: unknown, error?: louiselm.acp.JsonRpcError): boolean, string?), defer?: boolean) Queue one request, optionally deferring presentation.
---@field request_interaction fun(self: louiselm.ui.Decisions, session: louiselm.session.Session, data: louiselm.session.InteractionData, respond: (fun(result: louiselm.session.InteractionResponse): boolean, string?), defer?: boolean) Queue a validated vendor interaction in the same slot.
---@field cancel fun(self: louiselm.ui.Decisions, session: louiselm.session.Session, request_ids: unknown) Retire Session-cancelled requests.
---@field retire fun(self: louiselm.ui.Decisions, session: louiselm.session.Session) Retire a closing Session before selecting the survivor.
---@field present_next fun(self: louiselm.ui.Decisions, suspension?: table) Reconcile pending work, releasing a matching navigation hold.
---@field suspend fun(self: louiselm.ui.Decisions): table Hide decisions until this navigation hold is released.
---@field is_active fun(self: louiselm.ui.Decisions): boolean Report active decision authority.
---@field active_session fun(self: louiselm.ui.Decisions): louiselm.session.Session? Session owning the currently displayed decision.
---@field dispose fun(self: louiselm.ui.Decisions): boolean Dispose owned resources.

local M = {}
local Decisions = {}
Decisions.__index = Decisions

---@param session louiselm.session.Session
---@return string label Single-line identity of the requester, independent of focus.
local function session_label(session)
  local state = session:inspect()
  local label = "[" .. state.agent .. " " .. state.id .. "]"
  if state.name ~= nil and state.name ~= state.id then
    label = label .. " " .. state.name
  end
  return (label:gsub("%c", " "))
end

---@param value unknown
---@param name string
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

---@class louiselm.ui.ChatDecision
---@field session louiselm.session.Session Session that asked for a decision.
---@field data table ACP permission or interaction request data.
---@field respond fun(result: unknown, error?: louiselm.acp.JsonRpcError): boolean, string? ACP responder.
---@field answered boolean Whether this decision was already answered or cancelled.
---@field ui? louiselm.ui.DecisionPrompt|louiselm.ui.Diff Presentation retained while its Session is hidden.
---@field interaction? louiselm.session.InteractionData Validated question or plan request.
---@field cancel_interaction? fun() Retire interaction state without responding.

---@type fun(self: louiselm.ui.Decisions)
local pump_decisions

---@param self louiselm.ui.Decisions
---@param defer? boolean
local function present_pending(self, defer)
  if not defer and not nvim.in_fast_event() then
    pump_decisions(self)
    return
  end
  if self.scheduled then
    return
  end
  self.scheduled = true
  nvim.schedule(function()
    self.scheduled = false
    pump_decisions(self)
  end)
end

---@param self louiselm.ui.Decisions
---@param decision louiselm.ui.ChatDecision
---@return boolean visible
local function is_visible(self, decision)
  return not decision.answered
    and not self.disposed
    and self.is_live(decision.session)
    and (self.is_visible(decision.session) or (decision.ui ~= nil and decision.ui:contains_current()))
end

---@param decision louiselm.ui.ChatDecision
---@return table result
local function cancellation(decision)
  if decision.interaction ~= nil then
    return { outcome = "cancelled" }
  end
  return { outcome = { outcome = "cancelled" } }
end

---@param decision louiselm.ui.ChatDecision
local function close_ui(decision)
  if decision.cancel_interaction ~= nil then
    decision.cancel_interaction()
    decision.cancel_interaction = nil
  end
  if decision.ui ~= nil then
    decision.ui:dispose()
    decision.ui = nil
  end
end

---Release authority before responding so cancellation, errors and reentrant events settle once.
---@param self louiselm.ui.Decisions
---@param decision louiselm.ui.ChatDecision
---@param result table
---@param rpc_error? louiselm.acp.JsonRpcError
---@return boolean sent
---@return string? error_message Sanitized response failure.
local function settle(self, decision, result, rpc_error)
  if decision.answered then
    return false, "decision was already answered"
  end
  decision.answered = true
  if self.active == decision then
    self.active = nil
  end
  close_ui(decision)
  local call_ok, sent, send_error = pcall(decision.respond, result, rpc_error)
  if not call_ok or sent ~= true then
    -- Responders can throw or return payload-bearing errors; preserve only failure kind in the UI.
    local message = not call_ok and "decision response raised an error"
      or (send_error ~= nil and "decision response failed" or "decision response could not be sent")
    self.report_error(decision.session, message)
    return false, message
  end
  if decision.interaction == nil then
    self.resolved(decision.session, decision.data.request_id)
  end
  return true
end

---@param self louiselm.ui.Decisions
---@param decision louiselm.ui.ChatDecision
---@return fun(result: unknown, error?: louiselm.acp.JsonRpcError): boolean, string? respond
local function decision_responder(self, decision)
  return function(result, rpc_error)
    if decision.answered then
      return false, "decision was already answered"
    end
    if self.active ~= decision or not is_visible(self, decision) then
      return false, "decision is not visible"
    end
    local sent, err = settle(self, decision, result, rpc_error)
    pump_decisions(self)
    return sent, err
  end
end

---@param self louiselm.ui.Decisions
---@param decision louiselm.ui.ChatDecision
local function presentation_failed(self, decision)
  if decision.answered then
    return
  end
  self.report_error(decision.session, decision.interaction ~= nil and "interaction UI failed" or "permission UI failed")
  settle(self, decision, cancellation(decision))
  pump_decisions(self)
end

---@param self louiselm.ui.Decisions
---@param decision louiselm.ui.ChatDecision
---@param prompt louiselm.ui.DecisionPrompt
---@param respond fun(result: unknown, error?: louiselm.acp.JsonRpcError): boolean, string?
---@return boolean opened
---@return string? error_message
local function prompt_permission(self, decision, prompt, respond)
  local data = decision.data
  local options = permission_options(data.options)
  if options == nil or #options == 0 then
    respond(cancellation(decision))
    return true
  end
  local tool_call = data.toolCall or data.tool_call
  local details = {}
  if type(tool_call) == "table" then
    options[#options + 1] = details
  end
  return prompt:select(options, {
    prompt = session_label(decision.session) .. " · " .. permission_prompt(data),
    format_item = function(option)
      if option == details then
        return "View request details"
      end
      local _, label = permission_option(option)
      return label
    end,
  }, function(choice)
    if decision.answered or self.active ~= decision or not is_visible(self, decision) then
      return
    end
    if choice == details then
      local opened, err = prompt:inspect(
        nvim.split(nvim.inspect(tool_call), "\n", { plain = true }),
        session_label(decision.session) .. " · Request details · q/Esc: back to choices",
        function()
          if decision.answered or self.active ~= decision or not is_visible(self, decision) then
            return
          end
          local restored, restore_error = prompt_permission(self, decision, prompt, respond)
          if not restored then
            presentation_failed(self, decision)
          end
        end
      )
      if not opened then
        presentation_failed(self, decision)
      end
      return
    end
    respond(Gates.select_response(choice) or cancellation(decision))
  end)
end

---@param self louiselm.ui.Decisions
---@param decision louiselm.ui.ChatDecision
---@return boolean opened
---@return string? error_message
local function open_decision(self, decision)
  if decision.ui ~= nil then
    return decision.ui:show()
  end
  local respond = decision_responder(self, decision)
  local data = decision.data
  if decision.interaction == nil and type(data.operation) == "table" and data.operation.kind == "file_edit" then
    local diff = Diff.new(function()
      return is_visible(self, decision)
    end)
    decision.ui = diff
    return diff:open(data, respond, session_label(decision.session))
  end
  local prompt = DecisionPrompt.new(function()
    return is_visible(self, decision)
  end)
  decision.ui = prompt
  if decision.interaction ~= nil then
    local cancel = Interactions.open(decision.interaction, respond, function(message)
      self.report_error(decision.session, message)
    end, session_label(decision.session), prompt)
    if decision.answered then
      cancel()
    else
      decision.cancel_interaction = cancel
    end
    return true
  end
  return prompt_permission(self, decision, prompt, respond)
end

---Keep hidden requests pending and present only the currently focused Session's oldest request.
---@param self louiselm.ui.Decisions
pump_decisions = function(self)
  if self.presenting then
    return
  end
  self.presenting = true
  local active = self.active
  if active ~= nil and (self.suspended ~= nil or not is_visible(self, active)) then
    self.active = nil
    if active.ui ~= nil then
      active.ui:hide()
    end
  elseif active ~= nil and active.ui ~= nil then
    -- Closing a float externally leaves the request pending in its owning host.
    local call_ok, shown = pcall(active.ui.show, active.ui)
    if not call_ok or not shown then
      presentation_failed(self, active)
    end
  end
  local index = 1
  while index <= #self.queue do
    local decision = self.queue[index]
    if not decision.answered and (self.disposed or not self.is_live(decision.session)) then
      settle(self, decision, cancellation(decision))
    end
    if not decision.answered and self.active == nil and self.suspended == nil and self.is_visible(decision.session) then
      self.active = decision
      local call_ok, opened, open_error = pcall(open_decision, self, decision)
      if not call_ok or not opened then
        presentation_failed(self, decision)
      end
    end
    if decision.answered then
      close_ui(decision)
      table.remove(self.queue, index)
    else
      index = index + 1
    end
  end
  self.presenting = false
end

---Revoke cancelled authority immediately, scheduling editor cleanup when called from a fast event.
---Malformed ID lists do nothing.
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
      if self.active == decision then
        self.active = nil
      end
    end
  end
  present_pending(self)
end

---Queue a request for its visible Session; malformed requests are cancelled immediately.
---@param self louiselm.ui.Decisions
---@param session louiselm.session.Session Session that asked for a decision.
---@param data unknown ACP permission request data.
---@param respond? fun(result: unknown, error?: louiselm.acp.JsonRpcError): boolean, string? ACP responder.
---@param defer? boolean Record the request now but schedule presentation after its event observer returns.
function Decisions:request(session, data, respond, defer)
  if type(respond) ~= "function" then
    local function report()
      self.report_error(session, "permission request has no response callback")
    end
    if defer or nvim.in_fast_event() then
      nvim.schedule(report)
    else
      report()
    end
    return
  end
  local decision = {
    session = session,
    data = type(data) == "table" and data or {},
    respond = respond,
    answered = false,
  }
  if type(data) ~= "table" then
    local function cancel()
      settle(self, decision, cancellation(decision))
    end
    if defer or nvim.in_fast_event() then
      nvim.schedule(cancel)
    else
      cancel()
    end
    return
  end
  self.queue[#self.queue + 1] = decision
  present_pending(self, defer)
end

---Queue a validated interaction; retired hosts cancel without approving a plan.
---@param self louiselm.ui.Decisions
---@param session louiselm.session.Session
---@param data louiselm.session.InteractionData Validated event data.
---@param respond fun(result: louiselm.session.InteractionResponse): boolean, string? Consumer-owned answer.
---@param defer? boolean Record the request now but schedule presentation after its event observer returns.
function Decisions:request_interaction(session, data, respond, defer)
  self.queue[#self.queue + 1] =
    { session = session, data = data, interaction = data, respond = respond, answered = false }
  present_pending(self, defer)
end

---Retire a detached Session; call present_next after selecting its surviving host.
---@param self louiselm.ui.Decisions
---@param session louiselm.session.Session
function Decisions:retire(session)
  local retired = {}
  for index = #self.queue, 1, -1 do
    local decision = self.queue[index]
    if decision.session == session then
      table.remove(self.queue, index)
      retired[#retired + 1] = decision
    end
  end
  for _, decision in ipairs(retired) do
    if not decision.answered then
      settle(self, decision, cancellation(decision))
    else
      close_ui(decision)
    end
  end
end

---Hide without answering and hold presentation until this navigation finishes.
---@param self louiselm.ui.Decisions
---@return table suspension Opaque identity; pass it to present_next when navigation finishes.
function Decisions:suspend()
  local suspension = {}
  self.suspended = suspension
  local active = self.active
  self.active = nil
  if active ~= nil and active.ui ~= nil then
    active.ui:hide()
  end
  return suspension
end

---Reconcile queued work; only the matching navigation completion releases its hold.
---@param self louiselm.ui.Decisions
---@param suspension? table Hold returned by suspend; omission preserves a pending navigation.
function Decisions:present_next(suspension)
  if self.suspended == suspension then
    self.suspended = nil
  end
  pump_decisions(self)
end

---Report whether the current Session owns a visible decision.
---@param self louiselm.ui.Decisions
---@return boolean active
function Decisions:is_active()
  return self.active ~= nil and is_visible(self, self.active)
end

---Return the currently visible decision's Session, never a hidden request's owner.
---@param self louiselm.ui.Decisions
---@return louiselm.session.Session? session Nil when no decision is visible.
function Decisions:active_session()
  return self:is_active() and self.active.session or nil
end

---Dispose all owned presentations and cancel pending requests exactly once.
---@param self louiselm.ui.Decisions
---@return boolean disposed Always true.
function Decisions:dispose()
  if self.disposed then
    return true
  end
  self.disposed = true
  if self.observer ~= nil then
    nvim.api.nvim_del_autocmd(self.observer)
    self.observer = nil
  end
  pump_decisions(self)
  return true
end

---Construct a decision owner and its scheduled focus observer; dispose to release both.
---@param options louiselm.ui.DecisionsOptions
---@return louiselm.ui.Decisions decisions
function M.new(options)
  ---@type louiselm.ui.Decisions
  local owner = setmetatable({
    is_live = options.is_live,
    is_visible = options.is_visible,
    report_error = options.report_error,
    resolved = options.resolved,
    queue = {},
    disposed = false,
    presenting = false,
    scheduled = false,
  }, Decisions)
  owner.observer = nvim.api.nvim_create_autocmd({ "BufEnter", "WinEnter", "TabEnter", "WinClosed" }, {
    callback = function()
      if not owner.disposed then
        present_pending(owner, true)
      end
    end,
  })
  return owner
end

return M
