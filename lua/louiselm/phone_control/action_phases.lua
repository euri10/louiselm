---Retain payload-free observations of one Session-owned prompt.

---@diagnostic disable-next-line: undefined-global -- Neovim owns scheduling and SHA-256.
local nvim = vim
local M = {}
local Phases = {}
Phases.__index = Phases

local function drain(self)
  if self.disposed or self.writing or #self.queue == 0 then
    return
  end
  self.writing = true
  local item = self.queue[1]
  self.journal:run({
    v = 1,
    type = "report",
    identity = self.identity,
    outcome = item.outcome,
    at_ms = os.time() * 1000,
  }, function(result)
    self.writing = false
    if self.disposed then
      return
    end
    if result.type ~= "recorded" then
      if item.callback ~= nil then
        local callback = item.callback
        item.callback = nil
        callback(nil)
      end
      return
    end
    table.remove(self.queue, 1)
    if item.callback ~= nil then
      item.callback(item.evidence)
    end
    if self.terminal and #self.queue == 0 and self.on_terminal ~= nil then
      local callback = self.on_terminal
      self.on_terminal = nil
      callback(self.terminal_outcome)
    end
    drain(self)
  end)
end

local function append(self, state, callback)
  local evidence =
    nvim.fn.sha256("louiselm-phone-owner-" .. state .. "-v1:" .. self.identity.operation_id .. ":" .. self.turn_id)
  local outcome = state == "unknown" and "unknown" or { [state] = { evidence = evidence } }
  self.queue[#self.queue + 1] = { outcome = outcome, evidence = evidence, callback = callback }
  drain(self)
end

local function dispatched(self, turn_id)
  if self.turn_id == turn_id and not self.dispatched and not self.terminal then
    self.dispatched = true
    append(self, "dispatched")
  end
end

---Bind the returned admission to its Session and persist acceptance first.
---@param turn_id string Exact Session admission identity.
---@param callback fun(evidence: string?) Called after durable acceptance or a storage refusal.
function Phases:admitted(turn_id, callback)
  self.turn_id = turn_id
  append(self, "accepted", callback)
end

---Record the prompt callback without interpreting a failure as completion.
---@param succeeded boolean True only for a successful Session callback.
function Phases:completed(succeeded)
  nvim.schedule(function()
    if self.disposed or self.terminal or self.turn_id == nil then
      return
    end
    self.terminal = true
    self.unsubscribe()
    self.terminal_outcome = succeeded and "completed" or "unknown"
    append(self, self.terminal_outcome)
  end)
end

---Retry an unconfirmed report after storage becomes available.
function Phases:retry()
  drain(self)
end

---Release the Session subscription and ignore outstanding journal callbacks.
function Phases:dispose()
  if not self.disposed then
    self.disposed = true
    self.unsubscribe()
  end
end

---Observe a Session before starting the prompt; construction performs no I/O.
---@param session louiselm.session.Session
---@param journal table Asynchronous native-owner journal.
---@param identity table Exact payload-free action identity.
---@param on_terminal? fun(outcome: string) Called after all terminal evidence is durable.
---@return table phases
function M.new(session, journal, identity, on_terminal)
  local self = setmetatable({
    journal = journal,
    identity = identity,
    queue = {},
    dispatched = false,
    terminal = false,
    on_terminal = on_terminal,
    writing = false,
    disposed = false,
  }, Phases)
  self.unsubscribe = session:on(function(event)
    if event.type == "prompt_dispatched" and type(event.data) == "table" and type(event.data.turn_id) == "string" then
      local turn_id = event.data.turn_id
      nvim.schedule(function()
        if not self.disposed then
          dispatched(self, turn_id)
        end
      end)
    end
  end)
  return self
end

return M
