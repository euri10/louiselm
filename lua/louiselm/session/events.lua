---@alias louiselm.session.EventType
---| "chunk"
---| "tool_call_started"
---| "tool_call_finished"
---| "permission_requested"
---| "turn_done"
---| "error"

---@class louiselm.session.Event
---@field type louiselm.session.EventType Event kind.
---@field session_id string Local session identifier.
---@field data unknown Event-specific payload.
---@field respond? fun(result: unknown, error?: louiselm.acp.JsonRpcError): boolean, string? Permission response callback.

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
