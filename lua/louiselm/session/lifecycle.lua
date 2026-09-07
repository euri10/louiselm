local Acp = require("louiselm.acp")
local Permission = require("louiselm.permission")
local Events = require("louiselm.session.events")
local Validation = require("louiselm.session.validation")
local Provider = require("louiselm.agent.provider")

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

---@alias louiselm.session.Status "starting"|"ready"|"configuring"|"preparing"|"prompting"|"waiting_permission"|"cancelling"|"error"|"disposed"
---@alias louiselm.session.Prompt string|table

---@class louiselm.session.TurnIdentity
---@field agent string Configured Agent at prompt start.
---@field provider string Resolved access/quota service at prompt start.
---@field model? string|boolean Advertised Model value, when present; never inferred from its name.
---@field options table<string, string|boolean> Complete supported option tuple at prompt start.

---@class louiselm.session.State
---@field id string Local session identifier.
---@field name string User-facing session name.
---@field source "new"|"loaded" Whether the session was created or restored.
---@field agent string Named agent definition.
---@field acp_session_id? string Agent-side persistent conversation identifier, once available.
---@field status louiselm.session.Status Lifecycle state.
---@field working_dir string ACP working directory.
---@field current_turn integer Accepted prompt attempts in this local Session, including failed admission; not durable identity.
---@field turn_identity? louiselm.session.TurnIdentity Owned identity for the latest accepted attempt; later option changes never rewrite it.
---@field turn_options_changed boolean Whether the current/last attempt had confirmed value changes; unsuitable for fixed-option comparisons.
---@field turn_id? string Durable identity of the latest accepted prompt attempt; independent of local turn ordinal.
---@field recording_error? louiselm.session.RecordingError Storage failure or unresolved current Provider; subsequent dispatch requires recovery.
---@field recording_pending boolean Whether the registry has unacknowledged writes.
---@field config_options louiselm.session.ConfigOption[] Supported agent-advertised options in priority order.
---@field context? louiselm.session.ContextUsage Latest agent-reported context state.
---@field cost? louiselm.session.Cost Latest agent-reported cumulative cost.
---@field usage? louiselm.session.TurnUsage Latest agent-reported completed-turn usage.
---@field activity? string Current generic tool activity.
---@field session_failure? louiselm.session.SessionFailure Latest Agent-provided Session failure status.
---@field skills_policy louiselm.skills.Policy Effective session-static Agent Skills policy.
---@field embedded_context boolean Whether the Agent accepts embedded resource prompt context.
---@field commands louiselm.session.AvailableCommand[] Latest agent-advertised commands, replaced wholesale on each update.

---@class louiselm.session.AvailableCommand
---@field name string Command name as advertised by the agent.
---@field description string Human-readable command description.

---@class louiselm.session.Options
---@field cwd? string Working directory for the ACP session.
---@field env? table<string, string> Per-Session Agent process environment overrides.
---@field name? string User-facing session name.
---@field on_event? louiselm.session.EventCallback Initial event listener.
---@field permission_policy? louiselm.permission.Policy Policy for agent-requested operations.
---@field permission_store? louiselm.permission.Store Remembered-permission owner.
---@field schedule? fun(delay_ms: integer, callback: fun()) Testable scheduling boundary; defaults to `vim.defer_fn`.
---@field start_timeout_ms? integer Milliseconds to wait for the ACP handshake before failing a Session stuck "starting"; defaults to 20000.

---@class louiselm.session.PermissionEntry
---@field data table ACP permission request parameters enriched with louiselm metadata.
---@field respond fun(result: unknown, rpc_error?: louiselm.acp.JsonRpcError): boolean, string? Raw ACP responder.
---@field answered boolean Whether a response was already written for this request.

---@class louiselm.session.Session
---@field state louiselm.session.State Internal mutable state.
---@field emitter louiselm.session.EventEmitter Event subscribers.
---@field client louiselm.acp.Client? ACP client.
---@field acp_session_id string? Agent-side session identifier.
---@field load_session_id string? Agent-side session identifier to load.
---@field prompt_callback? fun(result: unknown, error?: string) Current prompt completion callback.
---@field recording_turn? { id: string, sequence: integer, finished: boolean, dispatched: boolean } Active recording identity.
---@field option_observer_id string Random identity of this live observation stream.
---@field option_sequence integer Number of confirmed value transitions observed outside replay.
---@field attribution_error? louiselm.session.RecordingError Unresolved current Provider; cleared only by a confirmed correction.
---@field ready_callback? fun(session: louiselm.session.Session?, error?: string) Session startup callback.
---@field ready_callback_called boolean Whether startup callback ran.
---@field turn_done_turn integer? Turn for which the completion event was emitted.
---@field prompt_progress integer Meaningful updates observed during the active prompt.
---@field prompt_watchdog_revision integer Invalidates obsolete prompt timeout callbacks.
---@field owner louiselm.session.Registry Registry that owns this session.
---@field owner_run? louiselm.workflow.Run Run that supervised construction of this Session.
---@field definition louiselm.agent.Definition Agent process definition.
---@field options louiselm.session.Options Session options.
---@field permission_policy louiselm.permission.Policy Policy for agent-requested operations.
---@field permission_store louiselm.permission.Store Remembered-permission owner.
---@field schedule fun(delay_ms: integer, callback: fun()) Testable scheduling boundary.
---@field start_timeout_ms integer Milliseconds to wait for the ACP handshake before failing a Session stuck "starting".
---@field stderr_buffer string Recent stderr output from the agent process, most-recent-last.
---@field permission_active? louiselm.session.PermissionEntry Permission request published for a decision.
---@field permission_queue louiselm.session.PermissionEntry[] Permission requests waiting for the active one.
---@field start fun(self: louiselm.session.Session): boolean, string?
---@field on fun(self: louiselm.session.Session, callback: louiselm.session.EventCallback): fun()
---@field inspect fun(self: louiselm.session.Session): louiselm.session.State
---@field set_name fun(self: louiselm.session.Session, name: string): boolean, string? Rename the session.
---@field prompt fun(self: louiselm.session.Session, prompt: louiselm.session.Prompt, callback?: fun(result: unknown, error?: string)): string?, string?
---@field cancel fun(self: louiselm.session.Session): boolean, string?
---@field set_config_option fun(self: louiselm.session.Session, id: string, value: string|boolean, callback?: fun(options: louiselm.session.ConfigOption[]?, error?: string)): string|number?, string?
---@field dispose fun(self: louiselm.session.Session): boolean, string?

-- ACP `RequestError.resourceNotFound`: the agent-side session store no longer
-- has this session id, distinct from any other session/load failure mode.
local ACP_RESOURCE_NOT_FOUND = -32002

-- A hung agent process (or a proxy wrapping one) never exits and never speaks
-- ACP: without a bound, a Session stuck "starting" stays there forever with
-- no error (louiselm-8hau).
local DEFAULT_START_TIMEOUT_MS = 20000

-- A provider failure can leave an ACP peer alive without resolving the
-- session/prompt request (as OpenCode Go does when its usage limit is reached).
-- Bound inactivity without killing a long turn that is still making progress
-- (louiselm-5zhl, louiselm-2p53).
local DEFAULT_PROMPT_TIMEOUT_MS = 300000

-- Keep only the most recent stderr output so an agent that prints
-- continuously cannot grow this without bound; the failure text that matters
-- is almost always the last thing printed before exit.
local STDERR_BUFFER_LIMIT = 4096

local M = {}
local Session = {}
Session.__index = Session

---@return string? id
---@return string? error_message
local function random_id()
  local bytes, err = nvim.uv.random(16)
  if bytes == nil then
    return nil, "could not allocate recording identity: " .. tostring(err)
  end
  return (bytes:gsub(".", function(byte)
    return string.format("%02x", byte:byte())
  end))
end

-- Claude's SDK default for recent models streams signature-only "thinking"
-- blocks (`display = "omitted"`, empty text), so a Claude Session never shows
-- a `[thinking]` fold unless something requests summarized display.
--
-- Sent to every Agent, not just a Claude-shaped one. `_meta` is ACP's
-- extensibility namespace: the schema types it as an open record of arbitrary
-- keys and the protocol requires that "implementations MUST NOT make
-- assumptions about values at these keys", so a vendor-namespaced
-- `claudeCode` entry is exactly what a non-Claude Agent is obliged to ignore.
--
-- Gating this on `transcript_layout == "claude"` was tried first and shipped
-- inert (louiselm-5tuq): that field is an optional Provenance hint whose
-- absence `session/locator.lua` treats as "search every layout", so the
-- ordinary configuration that omits it silently lost the feature with no
-- diagnostic. Keying off the agent's config name is worse still, since those
-- are arbitrary user labels rather than identity (louiselm-h00g).
local CLAUDE_THINKING_DEFAULT = { type = "adaptive", display = "summarized" }

---@param definition louiselm.agent.Definition
---@return table? meta
local function session_meta(definition)
  local options = definition.options
  local meta = type(options) == "table" and options._meta or nil
  local claude_code = type(meta) == "table" and meta.claudeCode or nil
  local claude_options = type(claude_code) == "table" and claude_code.options or nil
  if type(claude_options) == "table" and claude_options.thinking ~= nil then
    return meta
  end
  local result = nvim.deepcopy(meta) or {}
  result.claudeCode = result.claudeCode or {}
  result.claudeCode.options = result.claudeCode.options or {}
  result.claudeCode.options.thinking = CLAUDE_THINKING_DEFAULT
  return result
end

---@param error_value louiselm.acp.JsonRpcError|string|nil
---@return string
local function error_message(error_value)
  if type(error_value) == "table" then
    local data = error_value.data
    local details = type(data) == "table" and data.details or nil
    if
      error_value.message == "Internal error"
      and type(details) == "string"
      and details:match("^thread %S+ already has an active writer$") ~= nil
    then
      return "session is already open in another client; close it there before resuming"
    end
    return error_value.message
  end
  return tostring(error_value or "ACP request failed")
end

---@param self louiselm.session.Session
---@param event_type louiselm.session.EventType
---@param data unknown
---@param respond? fun(result: unknown, error?: louiselm.acp.JsonRpcError): boolean, string?
local function emit(self, event_type, data, respond)
  if self.state.status == "disposed" then
    return
  end
  if respond ~= nil then
    self.emitter:emit({ type = event_type, session_id = self.state.id, data = data, respond = respond })
  else
    self.emitter:emit({ type = event_type, session_id = self.state.id, data = data })
  end
end

---@param self louiselm.session.Session
---@param data string Raw stderr chunk from the agent process.
local function buffer_stderr(self, data)
  local combined = self.stderr_buffer .. data
  if #combined > STDERR_BUFFER_LIMIT then
    combined = combined:sub(#combined - STDERR_BUFFER_LIMIT + 1)
  end
  self.stderr_buffer = combined
end

---@param self louiselm.session.Session
---@param status louiselm.session.Status
local function set_status(self, status)
  if self.state.status == status then
    return
  end
  self.state.status = status
  emit(self, "state_changed", { status = status, activity = self.state.activity })
end

---@param self louiselm.session.Session
---@return boolean cleared
local function clear_session_failure(self)
  if self.state.session_failure == nil then
    return false
  end
  self.state.session_failure = nil
  return true
end

---@param self louiselm.session.Session
---@param kind "dispatch"|"cost"|"cancel_requested"|"outcome"
---@param data table Normalized metadata only.
local function record_observation(self, kind, data)
  local turn = self.recording_turn
  if turn == nil or turn.finished then
    return
  end
  turn.sequence = turn.sequence + 1
  if kind == "outcome" then
    turn.finished = true
  end
  self.state.recording_pending = true
  self.owner.recording:append({
    turn_id = turn.id,
    sequence = turn.sequence,
    kind = kind,
    observed_at = tostring(os.date("!%Y-%m-%dT%H:%M:%SZ")),
    data = data,
  })
end

---@param self louiselm.session.Session
---@param message string
---@param peer_response? boolean Whether an ACP prompt response was actually observed.
local function fail(self, message, peer_response)
  if self.state.status == "disposed" or self.state.status == "error" then
    return
  end
  record_observation(self, "outcome", { outcome = "failed", peer_response = peer_response == true })
  clear_session_failure(self)
  set_status(self, "error")
  self.permission_active = nil
  self.permission_queue = {}
  local client = self.client
  if client ~= nil then
    local closed, close_error = client:close()
    if not closed then
      message = message .. "; ACP cleanup failed: " .. (close_error or "unknown error")
    end
  end
  local prompt_callback = self.prompt_callback
  self.prompt_callback = nil
  emit(self, "error", { message = message })
  if prompt_callback ~= nil then
    prompt_callback(nil, message)
  end
  if not self.ready_callback_called then
    self.ready_callback_called = true
    local callback = self.ready_callback
    if callback ~= nil then
      callback(nil, message)
    end
  end
end

---@param self louiselm.session.Session
---@return boolean active
local function prompt_active(self)
  return self.state.status == "prompting"
    or self.state.status == "waiting_permission"
    or self.state.status == "cancelling"
end

---@param self louiselm.session.Session
local function schedule_prompt_timeout(self)
  self.prompt_watchdog_revision = self.prompt_watchdog_revision + 1
  local revision = self.prompt_watchdog_revision
  local turn = self.state.current_turn
  local progress = self.prompt_progress
  self.schedule(DEFAULT_PROMPT_TIMEOUT_MS, function()
    if self.prompt_watchdog_revision ~= revision or self.state.current_turn ~= turn or not prompt_active(self) then
      return
    end
    if self.permission_active ~= nil or #self.permission_queue > 0 then
      return
    end
    if self.prompt_progress ~= progress then
      schedule_prompt_timeout(self)
      return
    end
    fail(self, "ACP session/prompt made no progress during a " .. DEFAULT_PROMPT_TIMEOUT_MS .. "ms watchdog interval")
  end)
end

---@param self louiselm.session.Session
---@param result unknown
local function complete_turn(self, result)
  if self.state.status == "disposed" or self.state.status == "error" then
    return
  end
  if self.turn_done_turn ~= self.state.current_turn then
    self.turn_done_turn = self.state.current_turn
    clear_session_failure(self)
    if self.permission_active == nil and #self.permission_queue == 0 then
      set_status(self, "ready")
    else
      set_status(self, "waiting_permission")
    end
    emit(self, "turn_done", result)
  end
  local callback = self.prompt_callback
  self.prompt_callback = nil
  if callback ~= nil then
    callback(result)
  end
end

---@param self louiselm.session.Session
---@param message string Sanitized admission error.
local function reject_prompt(self, message)
  record_observation(self, "outcome", { outcome = "not_sent", peer_response = false })
  local completion = self.prompt_callback
  local turn_id = self.state.turn_id
  self.prompt_callback = nil
  set_status(self, "ready")
  emit(self, "prompt_rejected", { turn_id = turn_id, message = message })
  if completion then
    completion(nil, message)
  end
end

---@param options louiselm.session.ConfigOption[]
---@return table<string, string|boolean>
local function option_values(options)
  local values = {}
  for _, option in ipairs(options) do
    values[option.id] = option.current_value
  end
  return values
end

-- Queue before publishing accepted state: reentrant observers may submit,
-- Cancel, Dispose, or receive another update, and must not overtake this fact.
---@param self louiselm.session.Session
---@param options louiselm.session.ConfigOption[] Validated complete replacement.
---@param request? louiselm.session.OptionRequest Matched response only.
local function accept_options(self, options, request)
  local previous = option_values(self.state.config_options)
  local values = option_values(options)
  local previous_model = Validation.model_value(self.state.config_options)
  self.state.config_options = options
  if self.state.context ~= nil and previous_model ~= Validation.model_value(options) then
    self.state.context.stale = true
  end
  if self.state.status == "starting" or nvim.deep_equal(previous, values) then
    return
  end
  local provider, provider_error = Provider.resolve(self.definition.provider, values)
  self.attribution_error = provider == nil
      and {
        code = "attribution",
        message = "agents." .. self.state.agent .. ".provider: " .. provider_error,
      }
    or nil
  self.state.recording_error = nvim.deepcopy(self.owner.recording.error or self.attribution_error)
  self.state.recording_pending = true
  self.option_sequence = self.option_sequence + 1
  local turn = self.recording_turn
  local turn_id = turn ~= nil and not turn.finished and turn.id or nil
  if turn_id ~= nil then
    self.state.turn_options_changed = true
  end
  local observed_at = os.date("!%Y-%m-%dT%H:%M:%SZ")
  ---@cast observed_at string -- This date format returns text, never a date table.
  self.owner.recording:append({
    kind = "options",
    id = self.option_observer_id .. ":" .. self.option_sequence,
    observer_id = self.option_observer_id,
    sequence = self.option_sequence,
    agent = self.state.agent,
    acp_session_id = self.acp_session_id,
    observed_at = observed_at,
    previous_options = previous,
    options = values,
    source = request ~= nil and "response" or "notification",
    request = request,
    turn_id = turn_id,
  })
end

---@param self louiselm.session.Session
---@param message louiselm.acp.JsonRpcNotification
local function handle_notification(self, message)
  if self.state.status == "disposed" then
    return
  end
  self.owner:handle_agent_notification(self, message)
  if message.method ~= "session/update" or type(message.params) ~= "table" then
    return
  end
  local params = message.params
  ---@cast params table
  if params.sessionId ~= self.acp_session_id then
    return
  end
  local update = params.update
  if type(update) ~= "table" or type(update.sessionUpdate) ~= "string" then
    fail(self, "malformed ACP session/update notification")
    return
  end

  local update_type = update.sessionUpdate
  local is_agent_progress = update_type == "agent_message_chunk"
    or update_type == "agent_thought_chunk"
    or update_type == "tool_call"
    or update_type == "tool_call_update"
  if prompt_active(self) and is_agent_progress then
    self.prompt_progress = self.prompt_progress + 1
  end
  if is_agent_progress and clear_session_failure(self) then
    emit(self, "state_changed", { status = self.state.status, activity = self.state.activity })
  end
  if update_type == "agent_message_chunk" then
    emit(self, "chunk", update)
  elseif update_type == "user_message_chunk" then
    -- A few Agents echo the live prompt as a user message. The local client
    -- already records it; only forward user messages outside an active turn,
    -- where they represent session/load replay history.
    if not prompt_active(self) then
      emit(self, "user_chunk", update)
    end
  elseif update_type == "agent_thought_chunk" then
    -- Agent reasoning text (Codex `reasoning` summary deltas, Claude thinking blocks).
    -- Same content shape as agent_message_chunk, but it is not part of the answer
    -- itself: consumers fold or export it separately instead of streaming it as prose.
    emit(self, "thought_chunk", update)
  elseif update_type == "tool_call" or update_type == "tool_call_update" then
    local status = update.status
    if status == "completed" or status == "failed" or status == "cancelled" then
      self.state.activity = nil
      emit(self, "tool_call_finished", update)
    else
      self.state.activity = type(update.title) == "string" and update.title or "tool"
      emit(self, "tool_call_started", update)
    end
    emit(self, "state_changed", { status = self.state.status, activity = self.state.activity })
  elseif update_type == "config_option_update" then
    local options = Validation.config_options(update.configOptions)
    if options == nil then
      fail(self, "malformed ACP config_option_update notification")
      return
    end
    accept_options(self, options)
    emit(self, "config_options_changed", nvim.deepcopy(options))
  elseif update_type == "usage_update" then
    local context, cost, cost_present = Validation.usage_update(update)
    if context == nil then
      -- Telemetry only: nothing in the prompt lifecycle or permission handling
      -- depends on this frame, so an unparseable one marks the existing context
      -- stale (if any) rather than destroying an otherwise healthy Session.
      if self.state.context ~= nil then
        self.state.context.stale = true
        emit(self, "usage_updated", nvim.deepcopy({ context = self.state.context, cost = self.state.cost }))
      end
      return
    end
    self.state.context = context
    if cost_present then
      self.state.cost = cost
      record_observation(self, "cost", { cost = cost or nvim.NIL })
    end
    emit(self, "usage_updated", nvim.deepcopy({ context = context, cost = self.state.cost }))
  elseif update_type == "available_commands_update" then
    local commands, diagnostics = Validation.available_commands(update.availableCommands)
    self.state.commands = commands
    emit(self, "commands_changed", { commands = nvim.deepcopy(commands), diagnostics = diagnostics })
  elseif update_type == "session_info_update" then
    local failure = Validation.session_failure(update._meta)
    if failure == nil then
      return
    end
    local current = self.state.session_failure
    if current ~= nil and current.id == failure.id and failure.revision <= current.revision then
      return
    end
    self.state.session_failure = failure
    if prompt_active(self) then
      self.prompt_progress = self.prompt_progress + 1
    end
    emit(self, "state_changed", { status = self.state.status, activity = self.state.activity })
  end
end

---@type fun(self: louiselm.session.Session)
local pump_permissions

---Write one ACP permission response and publish the next queued request.
---@param self louiselm.session.Session
---@param entry louiselm.session.PermissionEntry Request being answered.
---@param result unknown ACP permission result.
---@param rpc_error? louiselm.acp.JsonRpcError Protocol error returned instead of a result.
---@return boolean sent
---@return string? error_message ACP write error; the request stays outstanding.
local function send_permission(self, entry, result, rpc_error)
  local sent, send_error = entry.respond(result, rpc_error)
  if not sent then
    return false, send_error
  end
  entry.answered = true
  if self.permission_active == entry then
    self.permission_active = nil
  end
  pump_permissions(self)
  if self.permission_active == nil and self.state.status == "waiting_permission" then
    local status = self.turn_done_turn == self.state.current_turn and "ready" or "prompting"
    set_status(self, status)
    if status == "prompting" then
      schedule_prompt_timeout(self)
    end
  end
  return true
end

---Answer one published permission request, remembering an explicit lifetime choice first.
---@param self louiselm.session.Session
---@param entry louiselm.session.PermissionEntry Request being answered.
---@param result unknown ACP permission result.
---@param rpc_error? louiselm.acp.JsonRpcError Protocol error returned instead of a result.
---@return boolean sent
---@return string? error_message Duplicate response, remembered-permission, or ACP write failure.
local function respond_permission(self, entry, result, rpc_error)
  if entry.answered then
    return false, "permission request was already answered"
  end
  local data = entry.data
  local selected_decision, lifetime = Permission.gates.remembered_choice(data, result)
  if selected_decision ~= nil and lifetime ~= nil then
    local _, remember_error = self.permission_store:remember({
      session_id = self.state.id,
      agent = self.state.agent,
      adapter = { command = self.definition.command, args = self.definition.args },
      workspace = self.state.working_dir,
    }, data.operation, selected_decision, lifetime)
    if remember_error ~= nil then
      local cancelled, cancel_error = send_permission(self, entry, { outcome = { outcome = "cancelled" } })
      local message = "permission choice was cancelled because it could not be remembered: " .. remember_error
      if not cancelled then
        message = message .. "; cancellation failed: " .. (cancel_error or "unknown error")
      end
      return false, message
    end
  end
  return send_permission(self, entry, result, rpc_error)
end

---Answer a queued request from remembered permissions, or annotate it for a human decision.
---Remembered rules are read here rather than on arrival so that a choice made for one
---request also covers the parallel requests still queued behind it.
---@param self louiselm.session.Session
---@param entry louiselm.session.PermissionEntry Queued request.
---@return boolean answered Whether the request was answered without asking.
local function apply_remembered_permission(self, entry)
  local data = entry.data
  if data.policy_decision ~= "ask" then
    return false
  end
  local remembered, remembered_error = self.permission_store:evaluate({
    session_id = self.state.id,
    agent = self.state.agent,
    adapter = { command = self.definition.command, args = self.definition.args },
    workspace = self.state.working_dir,
  }, data.operation)
  if remembered == nil then
    data.permission_error = remembered_error or "remembered permission lookup failed"
    return false
  end
  if remembered == "ask" then
    return false
  end
  local response = Permission.gates.remembered_response(data, remembered)
  if response == nil then
    data.remembered_decision = remembered
    return false
  end
  local sent = entry.respond(response)
  entry.answered = sent
  return sent
end

---Publish the next permission request that still needs a human decision.
---Agents may ask for several parallel tool calls at once, but a decision UI can host
---only one choice, and a request the client never answers blocks the agent forever.
---@param self louiselm.session.Session
pump_permissions = function(self)
  while self.permission_active == nil do
    if self.state.status == "disposed" or self.state.status == "error" then
      return
    end
    local entry = table.remove(self.permission_queue, 1)
    if entry == nil then
      return
    end
    if not apply_remembered_permission(self, entry) then
      self.permission_active = entry
      self.prompt_watchdog_revision = self.prompt_watchdog_revision + 1
      set_status(self, "waiting_permission")
      emit(self, "permission_requested", entry.data, function(result, rpc_error)
        return respond_permission(self, entry, result, rpc_error)
      end)
    end
  end
end

---Answer every outstanding permission request with the ACP cancelled outcome and tell
---consumers which requests they no longer own, so a decision UI can close itself.
---Write failures stay unreported: the caller cancels through the same transport and
---already surfaces its own write error.
---@param self louiselm.session.Session
local function cancel_permissions(self)
  local outstanding = self.permission_queue
  local active = self.permission_active
  self.permission_queue = {}
  self.permission_active = nil
  if active ~= nil then
    table.insert(outstanding, 1, active)
  end
  local request_ids = {}
  for _, entry in ipairs(outstanding) do
    if not entry.answered then
      entry.answered = true
      entry.respond({ outcome = { outcome = "cancelled" } })
      request_ids[#request_ids + 1] = entry.data.request_id
    end
  end
  if #request_ids > 0 then
    emit(self, "permission_cancelled", { request_ids = request_ids })
  end
end

---@param self louiselm.session.Session
---@param request louiselm.acp.JsonRpcRequest
---@param respond fun(result: unknown, error?: louiselm.acp.JsonRpcError): boolean, string?
local function handle_request(self, request, respond)
  if request.method ~= "session/request_permission" then
    respond(nil, { code = -32601, message = "Method not found" })
    return
  end
  if type(request.params) ~= "table" or request.params.sessionId ~= self.acp_session_id then
    respond(nil, { code = -32602, message = "malformed ACP permission request" })
    return
  end
  local data = {}
  for key, value in pairs(request.params) do
    data[key] = value
  end
  data.request_id = request.id
  data.operation = Permission.gates.from_acp(data)
  local decision, policy_error = Permission.gates.check(self.permission_policy, data.operation)
  if decision == nil then
    respond(nil, {
      code = -32602,
      message = "invalid ACP permission request: " .. (policy_error or "permission policy failed"),
    })
    return
  end
  data.policy_decision = decision
  if decision ~= "ask" then
    local automatic_response = Permission.gates.response(data, decision)
    if automatic_response ~= nil then
      respond(automatic_response)
      return
    end
  end
  self.permission_queue[#self.permission_queue + 1] = { data = data, respond = respond, answered = false }
  pump_permissions(self)
end

---@param self louiselm.session.Session
---@param result louiselm.agent.ProcessResult
local function handle_exit(self, result)
  if self.state.status == "disposed" then
    return
  end
  local message
  if result.signal ~= nil and result.signal ~= 0 then
    message = "agent process exited with signal " .. result.signal
  else
    message = "agent process exited with code " .. tostring(result.code)
  end
  local stderr = self.stderr_buffer:match("^%s*(.-)%s*$")
  if stderr ~= "" then
    message = message .. ": " .. stderr
  end
  fail(self, message)
end

---@param self louiselm.session.Session
---@param result unknown
---@param rpc_error? louiselm.acp.JsonRpcError
local function handle_initialized(self, result, rpc_error)
  if self.state.status == "disposed" then
    return
  end
  if rpc_error ~= nil then
    fail(self, "ACP initialize failed: " .. error_message(rpc_error))
    return
  end
  if type(result) ~= "table" then
    fail(self, "ACP initialize returned a malformed result")
    return
  end
  local client = self.client
  if client == nil then
    fail(self, "ACP client disappeared during initialization")
    return
  end
  local prompt_capabilities = client.agent_capabilities.promptCapabilities
  self.state.embedded_context = type(prompt_capabilities) == "table" and prompt_capabilities.embeddedContext == true
  local method = self.load_session_id == nil and "new" or "load"
  local request_id, request_error
  local on_session_ready = function(session_result, session_error)
    if self.state.status == "disposed" then
      return
    end
    if session_error ~= nil then
      if method == "load" and type(session_error) == "table" and session_error.code == ACP_RESOURCE_NOT_FOUND then
        fail(
          self,
          "the Agent no longer has this Park's session; it may have expired, been evicted from the Agent's"
            .. " session store, or the Agent was reinstalled or upgraded"
        )
        return
      end
      fail(self, "ACP session/" .. method .. " failed: " .. error_message(session_error))
      return
    end
    if self.load_session_id == nil then
      if
        type(session_result) ~= "table"
        or type(session_result.sessionId) ~= "string"
        or session_result.sessionId == ""
      then
        fail(self, "ACP session/new returned a malformed result")
        return
      end
      self.acp_session_id = session_result.sessionId
    elseif type(session_result) ~= "table" or session_result == nvim.NIL then
      fail(self, "ACP session/load returned a malformed result")
      return
    end
    local options, options_error
    if self.load_session_id ~= nil and session_result.configOptions == nil then
      options = nvim.deepcopy(self.state.config_options)
    else
      local config_options
      if type(session_result) == "table" and session_result ~= nvim.NIL then
        config_options = session_result.configOptions
      end
      options, options_error = Validation.config_options(config_options)
    end
    if options == nil then
      fail(
        self,
        "ACP session/" .. method .. " returned malformed configOptions: " .. (options_error or "invalid options")
      )
      return
    end
    self.state.config_options = options
    set_status(self, "ready")
    if not self.ready_callback_called then
      self.ready_callback_called = true
      local callback = self.ready_callback
      if callback ~= nil then
        callback(self)
      end
    end
  end
  local meta = session_meta(self.definition)
  if self.load_session_id == nil then
    local params = { cwd = self.state.working_dir, mcpServers = {} }
    if meta ~= nil then
      params._meta = meta
    end
    request_id, request_error = client:new_session(params, on_session_ready)
  else
    self.acp_session_id = self.load_session_id
    local params = { sessionId = self.load_session_id, cwd = self.state.working_dir, mcpServers = {} }
    if meta ~= nil then
      params._meta = meta
    end
    request_id, request_error = client:load_session(params, on_session_ready)
  end
  if request_id == nil then
    fail(self, "ACP session/" .. method .. " failed: " .. (request_error or "request could not be sent"))
  end
end

---@param self louiselm.session.Session
---@param result unknown
---@param rpc_error? louiselm.acp.JsonRpcError
local function handle_prompt_result(self, result, rpc_error)
  if self.state.status == "disposed" or self.state.status == "error" then
    return
  end
  if rpc_error ~= nil then
    local message = "ACP session/prompt failed: " .. error_message(rpc_error)
    fail(self, message, true)
    return
  end
  if type(result) ~= "table" then
    fail(self, "ACP session/prompt returned a malformed result", true)
    return
  end
  local usage, usage_valid = Validation.turn_usage(result.usage)
  if not usage_valid then
    fail(self, "ACP session/prompt returned malformed usage", true)
    return
  end
  self.state.usage = usage
  record_observation(self, "outcome", {
    outcome = result.stopReason == "cancelled" and "cancelled" or "completed",
    peer_response = true,
    usage = usage,
  })
  complete_turn(self, result)
end

---@param owner louiselm.session.Registry
---@param id string
---@param agent_name string
---@param definition louiselm.agent.Definition
---@param options louiselm.session.Options
---@param ready_callback? fun(session: louiselm.session.Session?, error?: string)
---@param load_session_id? string Existing ACP session identifier to load.
---@return louiselm.session.Session
function M.new(owner, id, agent_name, definition, options, ready_callback, load_session_id)
  ---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
  local working_dir = options.cwd or vim.fn.getcwd()
  local session = setmetatable({
    state = {
      id = id,
      name = options.name or id,
      source = load_session_id == nil and "new" or "loaded",
      agent = agent_name,
      status = "starting",
      working_dir = working_dir,
      current_turn = 0,
      turn_options_changed = false,
      recording_pending = #owner.recording.queue > 0,
      recording_error = nvim.deepcopy(owner.recording.error),
      config_options = {},
      commands = {},
      skills_policy = definition.skills.policy,
      embedded_context = false,
    },
    emitter = Events.new(),
    owner = owner,
    definition = definition,
    options = options,
    acp_session_id = load_session_id,
    load_session_id = load_session_id,
    permission_policy = options.permission_policy or Permission.policy(),
    permission_store = options.permission_store or Permission.store(),
    permission_queue = {},
    ready_callback = ready_callback,
    ready_callback_called = false,
    turn_done_turn = nil,
    prompt_progress = 0,
    option_observer_id = "",
    option_sequence = 0,
    prompt_watchdog_revision = 0,
    schedule = options.schedule or function(delay_ms, callback)
      nvim.defer_fn(callback, delay_ms)
    end,
    start_timeout_ms = options.start_timeout_ms or DEFAULT_START_TIMEOUT_MS,
    stderr_buffer = "",
  }, Session)
  if options.on_event ~= nil then
    session:on(options.on_event)
  end
  return session
end

---Start ACP initialization and creation or loading of the agent-side session.
---@param self louiselm.session.Session
---@return boolean started
---@return string? error_message Immediate connection or request error.
function Session:start()
  local observer_id, identity_error = random_id()
  if observer_id == nil then
    fail(self, identity_error or "could not allocate option observation identity")
    return false, identity_error
  end
  self.option_observer_id = observer_id
  local client, connect_error = Acp.connect(self.definition, {
    cwd = self.state.working_dir,
    env = self.options.env,
    on_notification = function(message)
      handle_notification(self, message)
    end,
    on_request = function(request, respond)
      handle_request(self, request, respond)
    end,
    on_error = function(message)
      fail(self, "ACP transport error: " .. message)
    end,
    on_exit = function(result)
      handle_exit(self, result)
    end,
    on_stderr = function(data)
      buffer_stderr(self, data)
    end,
  })
  if client == nil then
    fail(self, connect_error or "could not connect to ACP agent")
    return false, connect_error
  end
  self.client = client
  local request_id, request_error = client:initialize(nil, function(result, rpc_error)
    handle_initialized(self, result, rpc_error)
  end)
  if request_id == nil then
    fail(self, request_error or "ACP initialize request could not be sent")
    return false, request_error
  end
  self.schedule(self.start_timeout_ms, function()
    if self.state.status ~= "starting" then
      return
    end
    fail(self, "agent did not respond within " .. self.start_timeout_ms .. "ms of starting")
  end)
  return true
end

---Subscribe to typed events from this session.
---@param self louiselm.session.Session
---@param callback louiselm.session.EventCallback Event callback.
---@return fun() unsubscribe Idempotent listener removal function.
function Session:on(callback)
  return self.emitter:on(callback)
end

---Return a snapshot of inspectable session state.
---@param self louiselm.session.Session
---@return louiselm.session.State state Copy of current state.
function Session:inspect()
  local state = nvim.deepcopy(self.state)
  state.acp_session_id = self.acp_session_id
  ---@cast state louiselm.session.State
  return state
end

---Set the user-facing name without changing the ACP session.
---@param self louiselm.session.Session
---@param name string New non-empty session name.
---@return boolean renamed
---@return string? error_message
function Session:set_name(name)
  if self.state.status == "disposed" then
    return false, "session is disposed"
  end
  if type(name) ~= "string" or name == "" then
    return false, "session name must be a non-empty string"
  end
  self.state.name = name
  emit(self, "state_changed", { status = self.state.status, activity = self.state.activity })
  return true
end

---Admit one prompt asynchronously, committing attribution before ACP dispatch.
---@param self louiselm.session.Session
---@param prompt louiselm.session.Prompt Text or ACP prompt content table.
---@param callback? fun(result: unknown, error?: string) Called once on completion or failure; suppressed after Disposal. Admission errors leave the Session ready to retry.
---@return string? turn_id Stable local turn ID, NOT an ACP request ID or proof of peer receipt.
---@return string? error_message Immediate validation, state, Provider, or random identity error; no prompt is sent.
function Session:prompt(prompt, callback)
  if self.state.status ~= "ready" then
    return nil, "session is not ready"
  end
  if type(prompt) == "string" then
    prompt = { { type = "text", text = prompt } }
  elseif type(prompt) ~= "table" then
    return nil, "prompt must be a string or table"
  end
  local client = self.client
  if client == nil then
    return nil, "session has no ACP client"
  end
  local values = option_values(self.state.config_options)
  local provider, provider_error = Provider.resolve(self.definition.provider, values)
  if provider == nil then
    return nil, "agents." .. self.state.agent .. ".provider: " .. provider_error
  end
  local turn_id, identity_error = random_id()
  if turn_id == nil then
    return nil, identity_error
  end
  prompt = nvim.deepcopy(prompt)
  clear_session_failure(self)
  self.state.current_turn = self.state.current_turn + 1
  self.state.turn_id = turn_id
  self.state.turn_options_changed = false
  self.state.usage = nil
  self.state.turn_identity = {
    agent = self.state.agent,
    provider = provider,
    model = Validation.model_value(self.state.config_options),
    options = values,
  }
  self.turn_done_turn = nil
  self.prompt_progress = 0
  self.prompt_callback = callback
  self.recording_turn = { id = turn_id, sequence = 0, finished = false, dispatched = false }
  local prepared = nvim.deepcopy(self.state.turn_identity)
  prepared.id, prepared.acp_session_id = turn_id, self.acp_session_id
  prepared.prepared_at = os.date("!%Y-%m-%dT%H:%M:%SZ")
  prepared.cost_baseline = nvim.deepcopy(self.state.cost)
  self.state.recording_pending = true
  -- Queue the initial fact immediately, so Cancellation/Disposal observations
  -- cannot overtake it. flush also retries any earlier failed registry writes.
  self.owner.recording:append(prepared)
  set_status(self, "preparing")
  self.owner.recording:flush(function(err)
    if self.state.status ~= "preparing" or self.state.turn_id ~= turn_id then
      return
    end
    if err ~= nil then
      reject_prompt(self, err.message)
      return
    end
    local turn = self.recording_turn
    if turn == nil or turn.finished then
      return
    end
    set_status(self, "prompting")
    -- A state observer may dispose/cancel synchronously before the write.
    if self.state.status ~= "prompting" then
      return
    end
    local current_values = option_values(self.state.config_options)
    local current_provider = Provider.resolve(self.definition.provider, current_values)
    if
      not nvim.deep_equal(current_values, prepared.options)
      or Validation.model_value(self.state.config_options) ~= prepared.model
      or current_provider ~= prepared.provider
      or not nvim.deep_equal(self.state.cost, prepared.cost_baseline)
    then
      reject_prompt(self, "Session attribution changed while preparing; submit the prompt again")
      return
    end
    local request_id, request_error = client:prompt(
      { sessionId = self.acp_session_id, prompt = prompt },
      function(result, rpc_error)
        handle_prompt_result(self, result, rpc_error)
      end
    )
    if request_id == nil then
      record_observation(self, "outcome", { outcome = "not_sent", peer_response = false })
      fail(self, request_error or "ACP prompt request could not be sent")
      return
    end
    turn.dispatched = true
    record_observation(self, "dispatch", { request_id = request_id })
    schedule_prompt_timeout(self)
  end)
  return turn_id
end

---Request cancellation of the active prompt turn.
---@param self louiselm.session.Session
---@return boolean sent
---@return string? error_message ACP write or state error.
function Session:cancel()
  if
    self.state.status == "preparing"
    or (self.state.status == "prompting" and self.recording_turn ~= nil and not self.recording_turn.dispatched)
  then
    record_observation(self, "outcome", { outcome = "not_sent", peer_response = false })
    complete_turn(self, { stopReason = "cancelled" })
    return true
  end
  if
    self.state.status ~= "prompting"
    and self.state.status ~= "waiting_permission"
    and self.state.status ~= "cancelling"
  then
    return false, "session is not prompting"
  end
  local client = self.client
  if client == nil then
    return false, "session has no ACP client"
  end
  local sent, send_error = client:cancel({ sessionId = self.acp_session_id })
  if not sent then
    local message = send_error or "ACP cancel request could not be sent"
    fail(self, message)
    return false, message
  end
  record_observation(self, "cancel_requested", {})
  local permission_waiting = self.permission_active ~= nil or #self.permission_queue > 0
  cancel_permissions(self)
  clear_session_failure(self)
  set_status(self, "cancelling")
  if permission_waiting then
    schedule_prompt_timeout(self)
  end
  return true
end

---Change a supported option while the session is idle and replace all local options from the response.
---@param self louiselm.session.Session
---@param id string Option identifier.
---@param value string|boolean New value matching the advertised option kind.
---@param callback? fun(options: louiselm.session.ConfigOption[]?, error?: string) Called once with the complete replacement list.
---@return string|number? request_id ACP request identifier.
---@return string? error_message Validation, lifecycle, or write error.
function Session:set_config_option(id, value, callback)
  if self.state.status ~= "ready" then
    return nil, "session is not idle"
  end
  if type(id) ~= "string" or id == "" then
    return nil, "config option id must be a non-empty string"
  end
  local option = Validation.find_option(self.state.config_options, id)
  if option == nil then
    return nil, "unknown config option '" .. id .. "'"
  end
  if option.type == "boolean" then
    if type(value) ~= "boolean" then
      return nil, "boolean config option requires a boolean value"
    end
  else
    if type(value) ~= "string" then
      return nil, "select config option requires a string value"
    end
    local available = false
    for _, item in ipairs(option.options or {}) do
      if item.value == value then
        available = true
        break
      end
    end
    if not available then
      return nil, "config option value is not available"
    end
  end
  local client = self.client
  if client == nil then
    return nil, "session has no ACP client"
  end
  local params = { sessionId = self.acp_session_id, configId = id, value = value }
  if option.type == "boolean" then
    params.type = "boolean"
  end
  set_status(self, "configuring")
  local request_id, request_error
  request_id, request_error = client:set_config_option(params, function(result, rpc_error)
    if self.state.status == "disposed" then
      return
    end
    if rpc_error ~= nil then
      local message = "ACP session/set_config_option failed: " .. error_message(rpc_error)
      set_status(self, "ready")
      if callback ~= nil then
        callback(nil, message)
      end
      return
    end
    local options, options_error = Validation.config_options(type(result) == "table" and result.configOptions)
    if options == nil then
      local message = "ACP session/set_config_option returned malformed configOptions: "
        .. (options_error or "invalid options")
      fail(self, message)
      if callback ~= nil then
        callback(nil, message)
      end
      return
    end
    ---@cast request_id string|number -- ACP returns the ID before receiving the asynchronous response.
    accept_options(self, options, { id = request_id, option = id, value = value })
    set_status(self, "ready")
    emit(self, "config_options_changed", nvim.deepcopy(options))
    if callback ~= nil then
      callback(nvim.deepcopy(options))
    end
  end)
  if request_id == nil then
    set_status(self, "ready")
    return nil, request_error or "ACP config option request could not be sent"
  end
  return request_id
end

---Dispose this session, terminate its ACP process, and remove it from its registry.
---@param self louiselm.session.Session
---@return boolean disposed
---@return string? error_message Process close error, if any.
function Session:dispose()
  if self.state.status == "disposed" then
    return true
  end
  record_observation(self, "outcome", { outcome = "disposed", peer_response = false })
  clear_session_failure(self)
  set_status(self, "disposed")
  self.permission_store:clear_session(self.state.id)
  self.permission_active = nil
  self.permission_queue = {}
  self.prompt_callback = nil
  local client = self.client
  local closed, close_error = true, nil
  if client ~= nil then
    closed, close_error = client:close()
  end
  self.emitter:clear()
  self.owner:remove_session(self.state.id)
  return closed, close_error
end

return M
