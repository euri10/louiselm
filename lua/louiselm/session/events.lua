---@alias louiselm.session.EventType
---| "chunk"
---| "user_chunk"
---| "thought_chunk"
---| "tool_call_started"
---| "tool_call_finished"
---| "permission_requested"
---| "permission_cancelled"
---| "config_options_changed"
---| "commands_changed"
---| "usage_updated"
---| "compaction_updated"
---| "recording_changed"
---| "prompt_rejected"
---| "state_changed"
---| "turn_done"
---| "error"

---@class louiselm.session.EventBase
---@field session_id string Local session identifier.

---@class louiselm.session.StateChangedData
---@field status louiselm.session.Status Current lifecycle state.
---@field previous_status? louiselm.session.Status Previous state when the lifecycle changed, absent for refreshes.
---@field activity? string Current generic tool activity.

---@class louiselm.session.StateChangedEvent: louiselm.session.EventBase
---@field type "state_changed"
---@field data louiselm.session.StateChangedData

---@class louiselm.session.ConfigOptionsChangedEvent: louiselm.session.EventBase
---@field type "config_options_changed"
---@field data louiselm.session.ConfigOption[] Complete supported option state in agent order. Confirmed value changes are queued for persistence before publication; request attribution lives in option_events.

---@class louiselm.session.CommandsChangedData
---@field commands louiselm.session.AvailableCommand[] Complete supported command state in agent order.
---@field diagnostics string[] Messages for advertised entries skipped as malformed.

---@class louiselm.session.CommandsChangedEvent: louiselm.session.EventBase
---@field type "commands_changed"
---@field data louiselm.session.CommandsChangedData

---@class louiselm.session.UsageUpdatedData
---@field context louiselm.session.ContextUsage Current context usage.
---@field cost? louiselm.session.Cost Current cumulative cost, when reported.

---@class louiselm.session.UsageUpdatedEvent: louiselm.session.EventBase
---@field type "usage_updated"
---@field data louiselm.session.UsageUpdatedData

---@class louiselm.session.CompactionUpdatedEvent: louiselm.session.EventBase
---@field type "compaction_updated"
---@field data louiselm.session.Compaction Complete owned snapshot, with stable first-seen placement by ID.

---@class louiselm.session.RecordingChangedData
---@field error? louiselm.session.RecordingError Storage failure or unresolved Session Provider; active work continues, new dispatch requires correction/recovery.
---@field pending boolean Whether this registry has unacknowledged writes.

---@class louiselm.session.RecordingChangedEvent: louiselm.session.EventBase
---@field type "recording_changed"
---@field data louiselm.session.RecordingChangedData

---@class louiselm.session.PromptRejectedEvent: louiselm.session.EventBase
---@field type "prompt_rejected"
---@field data { turn_id: string, message: string } Asynchronous admission failure; no ACP prompt was sent.

---@class louiselm.session.PermissionData
---@field request_id string|number ACP request identifier.
---@field operation louiselm.permission.Request Normalized requested operation.
---@field policy_decision louiselm.permission.Decision Evaluated policy decision.
---@field remembered_decision? "allow"|"deny" A matching rule that could not be replayed through the offered option kinds.
---@field permission_error? string Non-fatal remembered-permission lookup failure.
---@field options? unknown[] Agent-advertised response options.
---@field toolCall? table Agent tool call payload.

---@class louiselm.session.PermissionEvent: louiselm.session.EventBase
---@field type "permission_requested"
---@field data louiselm.session.PermissionData
---@field respond fun(result: unknown, error?: louiselm.acp.JsonRpcError): boolean, string? Permission response callback.

---@class louiselm.session.PermissionCancelledData
---@field request_ids (string|number)[] ACP requests the session answered on the consumer's behalf.

---@class louiselm.session.PermissionCancelledEvent: louiselm.session.EventBase
---@field type "permission_cancelled"
---@field data louiselm.session.PermissionCancelledData

---@class louiselm.session.GenericEvent: louiselm.session.EventBase
---@field type "chunk"|"user_chunk"|"thought_chunk"|"tool_call_started"|"tool_call_finished"|"turn_done"|"error"
---@field data unknown Event-specific payload. For "chunk"/"user_chunk"/"thought_chunk" this is
---the raw ACP `agent_message_chunk`/`user_message_chunk`/`agent_thought_chunk` update;
---"user_chunk" only arrives while replaying a resumed session's history via session/load, never
---for a live turn, while "thought_chunk" carries the agent's reasoning text (live or replayed).

---@alias louiselm.session.Event louiselm.session.StateChangedEvent|louiselm.session.ConfigOptionsChangedEvent|louiselm.session.CommandsChangedEvent|louiselm.session.UsageUpdatedEvent|louiselm.session.CompactionUpdatedEvent|louiselm.session.RecordingChangedEvent|louiselm.session.PromptRejectedEvent|louiselm.session.PermissionEvent|louiselm.session.PermissionCancelledEvent|louiselm.session.GenericEvent

---@alias louiselm.session.EventCallback fun(event: louiselm.session.Event)

---@class louiselm.session.EventEmitter
---@field listeners table<integer, louiselm.session.EventCallback>
---@field order integer[] Listener registration order.
---@field next_id integer Next listener identifier.
---@field on fun(self: louiselm.session.EventEmitter, callback: louiselm.session.EventCallback): fun() Remove the listener.
---@field emit fun(self: louiselm.session.EventEmitter, event: louiselm.session.Event)
---@field clear fun(self: louiselm.session.EventEmitter)

local M = {}
local Emitter = {}
Emitter.__index = Emitter

---Create an event emitter with deterministic listener ordering.
---@return louiselm.session.EventEmitter emitter
function M.new()
  return setmetatable({ listeners = {}, order = {}, next_id = 1 }, Emitter)
end

---Subscribe to session events.
---@param self louiselm.session.EventEmitter
---@param callback louiselm.session.EventCallback Event callback.
---@return fun() unsubscribe Idempotent listener removal function.
function Emitter:on(callback)
  if type(callback) ~= "function" then
    error("session event callback must be a function")
  end
  local id = self.next_id
  self.next_id = id + 1
  self.listeners[id] = callback
  self.order[#self.order + 1] = id
  local active = true
  return function()
    if active then
      active = false
      self.listeners[id] = nil
    end
  end
end

---Emit one event to all currently subscribed listeners.
---@param self louiselm.session.EventEmitter
---@param event louiselm.session.Event Event to deliver.
function Emitter:emit(event)
  for _, id in ipairs(self.order) do
    local callback = self.listeners[id]
    if callback ~= nil then
      callback(event)
    end
  end
end

---Remove all event listeners.
---@param self louiselm.session.EventEmitter
function Emitter:clear()
  self.listeners = {}
  self.order = {}
end

return M
