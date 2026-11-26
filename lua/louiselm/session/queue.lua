---@diagnostic disable-next-line: undefined-global -- Neovim owns scheduling and identities.
local nvim = vim
local Routing = require("louiselm.routing.routing")

---@class louiselm.session.QueueEntry
---@field id string Immutable admission identity.
---@field prompt string|table Frozen ACP content.
---@field display_text string Visible submitted text.
---@field contexts table[] Frozen presentation context.
---@field metadata? louiselm.routing.SubmissionMetadata Frozen routing correlation.

---@class louiselm.session.QueueReplacement
---@field entry louiselm.session.QueueEntry Frozen replacement payload.
---@field target_turn_id string Exact turn targeted by cancellation.
---@field cancellation "requesting"|"requested"|"acknowledged"|"completed"|"not_sent"|"unknown" Evidence about the target turn, separate from replacement delivery.
---@field status "cancelling"|"acknowledged"|"admitting"|"dispatched"|"unsent"|"uncertain" Evidence for the replacement, never a promise of completion.
---@field turn_id? string Replacement admission identity, when known.
---@field reason? string Why the replacement is unavailable.

---@class louiselm.session.QueueState
---@field revision integer Revision of the committed queue.
---@field pending? boolean A local admission or edit awaits acknowledgement.
---@field entries louiselm.session.QueueEntry[] Waiting entries in acceptance order.
---@field active? louiselm.session.QueueEntry Entry whose dispatch has started.
---@field active_status? "admitting"|"dispatched"|"not_sent"|"uncertain" Distinct local dispatch evidence for the active entry.
---@field active_turn_id? string Stable admission identity, not proof of peer receipt.
---@field replacement? louiselm.session.QueueReplacement Reserved Stop-and-send instruction, separate from ordinary waiting places.
---@field paused boolean Explicit review is needed before draining.
---@field reason? string Why draining is unavailable or paused.
---@field uncertain boolean A storage or delivery outcome is unresolved.
---@field uncertain_proposal? { entries: louiselm.session.QueueEntry[], active?: louiselm.session.QueueEntry, active_status?: string, replacement?: louiselm.session.QueueReplacement } Proposed state whose durable write was not acknowledged.
---@field disposed? boolean Explicit Disposal fenced this queue.

---@class louiselm.session.Queue
---@field session louiselm.session.Session
---@field store louiselm.session.QueueStore
---@field id string
---@field state louiselm.session.QueueState
---@field operations { run: fun(), cancel?: fun(error_message: string) }[]
---@field busy boolean
---@field stopped boolean
---@field pause_requested boolean
---@field recovered? boolean A saved queue awaits identity and outcome reconciliation.
---@field abandoned? boolean Editor exit retains the saved queue for review.
---@field current_turn_id? string Active queued admission in this live Session.
---@field replacement_target? string Target reserved by a live Stop-and-send operation.
---@field replacement_turn_id? string Live replacement admission, if started.
---@field replacement_starting? boolean The replacement prompt call is reentrantly publishing its new turn.
---@field replacement_dispatched? boolean Local transport accepted the replacement prompt.
---@field replacement_cancel_requested? boolean The target's cancellation was requested by this operation.
---@field replacement_vetoed? boolean A plain Stop won before replacement dispatch.
---@field disposal_callback? fun(error_message?: string)
---@field inspect fun(self: louiselm.session.Queue): louiselm.session.QueueState
---@field enqueue fun(self: louiselm.session.Queue, prompt: string|table, metadata: louiselm.routing.SubmissionMetadata?, display_text: string, contexts: table[], callback: fun(id: string?, error_message?: string)): boolean, string?
---@field cancel fun(self: louiselm.session.Queue, id: string, revision: integer, callback: fun(changed: boolean, error_message?: string)): boolean, string?
---@field move fun(self: louiselm.session.Queue, id: string, position: integer, revision: integer, callback: fun(changed: boolean, error_message?: string)): boolean, string?
---@field replace fun(self: louiselm.session.Queue, id: string, revision: integer, prompt: string|table, metadata: louiselm.routing.SubmissionMetadata?, display_text: string, contexts: table[], callback: fun(changed: boolean, error_message?: string)): boolean, string?
---@field continue fun(self: louiselm.session.Queue, revision: integer, callback: fun(changed: boolean, error_message?: string)): boolean, string?
---@field pause fun(self: louiselm.session.Queue, reason: string)
---@field observe fun(self: louiselm.session.Queue, event: louiselm.session.Event)
---@field reconcile fun(self: louiselm.session.Queue, snapshot: table, callback: fun())
---@field dispose fun(self: louiselm.session.Queue, callback?: fun(error_message?: string))
---@field abandon fun(self: louiselm.session.Queue)
---@field entry_for_turn fun(self: louiselm.session.Queue, turn_id: string): louiselm.session.QueueEntry?
---@field stop_and_send fun(self: louiselm.session.Queue, target: string, prompt: string|table, metadata: louiselm.routing.SubmissionMetadata?, display_text: string, contexts: table[], callback: fun(id: string?, error_message?: string), cancel: fun(): boolean, string?): boolean, string?
---@field veto_replacement fun(self: louiselm.session.Queue, target: string): boolean
---@field target_result fun(self: louiselm.session.Queue, target: string, cancelled: boolean)
---@field replacement_authorized fun(self: louiselm.session.Queue, turn_id: string): boolean

local M = {}
local Queue = {}
Queue.__index = Queue

local function identity()
  local bytes, err = nvim.uv.random(16)
  if bytes == nil then
    return nil, "could not allocate queue identity: " .. tostring(err)
  end
  return (bytes:gsub(".", function(byte)
    return string.format("%02x", byte:byte())
  end))
end

---@param session louiselm.session.Session
---@param store louiselm.session.QueueStore
---@return louiselm.session.Queue? queue
---@return string? error_message
function M.new(session, store)
  local id, err = identity()
  if id == nil then
    return nil, err
  end
  local queue = setmetatable({
    session = session,
    store = store,
    id = id,
    state = { revision = 0, entries = {}, paused = false, uncertain = false },
    operations = {},
    busy = false,
    stopped = false,
    pause_requested = false,
  }, Queue)
  ---@cast queue louiselm.session.Queue
  return queue
end

---Restore a saved queue for review only. ACP load alone cannot prove whether
---the former owner delivered an in-flight instruction or was disposed.
---@param session louiselm.session.Session
---@param store louiselm.session.QueueStore
---@param records table[]?
---@param error_message? string
---@return louiselm.session.Queue? queue
---@return string? error_message
function M.restore(session, store, records, error_message)
  if error_message == nil and records ~= nil and #records == 0 then
    return nil
  end
  local queue, err = M.new(session, store)
  if queue == nil then
    return nil, err
  end
  queue.recovered = true
  if error_message == nil and records ~= nil and #records == 1 then
    local saved = records[1]
    local state = saved.state
    local valid = type(state.entries) == "table"
      and nvim.islist(state.entries)
      and #state.entries <= 20
      and type(state.paused) == "boolean"
      and type(state.uncertain) == "boolean"
    local seen = {}
    local function valid_entry(entry)
      return type(entry) == "table"
        and type(entry.id) == "string"
        and #entry.id == 32
        and entry.id:match("^%x+$") ~= nil
        and not seen[entry.id]
        and (type(entry.prompt) == "string" or type(entry.prompt) == "table")
        and type(entry.display_text) == "string"
        and type(entry.contexts) == "table"
        and nvim.islist(entry.contexts)
        and Routing.valid_metadata(entry.metadata)
    end
    for _, entry in ipairs(valid and state.entries or {}) do
      valid = valid and valid_entry(entry)
      if valid then
        seen[entry.id] = true
      end
    end
    if state.active ~= nil then
      valid = valid
        and valid_entry(state.active)
        and (state.active_status == "admitting" or state.active_status == "dispatched" or state.active_status == "not_sent" or state.active_status == "uncertain")
        and (state.active_turn_id == nil or type(state.active_turn_id) == "string")
      if valid then
        seen[state.active.id] = true
      end
    else
      valid = valid and state.active_status == nil and state.active_turn_id == nil
    end
    if state.replacement ~= nil then
      local replacement = state.replacement
      valid = valid
        and type(replacement) == "table"
        and valid_entry(replacement.entry)
        and type(replacement.target_turn_id) == "string"
        and replacement.target_turn_id ~= ""
        and ({
          requesting = true,
          requested = true,
          acknowledged = true,
          completed = true,
          not_sent = true,
          unknown = true,
        })[replacement.cancellation]
        and ({
          cancelling = true,
          acknowledged = true,
          admitting = true,
          dispatched = true,
          unsent = true,
          uncertain = true,
        })[replacement.status]
        and (replacement.turn_id == nil or type(replacement.turn_id) == "string")
    end
    if valid then
      queue.id = saved.queue_id
      queue.state = nvim.deepcopy(state)
      local replacement = queue.state.replacement
      if replacement ~= nil then
        if replacement.cancellation == "requesting" or replacement.cancellation == "requested" then
          replacement.cancellation = "unknown"
        end
        if replacement.status == "cancelling" or replacement.status == "acknowledged" then
          replacement.status = "unsent"
          replacement.reason = "replacement was not dispatched before restart"
        elseif replacement.status == "admitting" or replacement.status == "dispatched" then
          replacement.status = "uncertain"
          replacement.reason = "replacement dispatch outcome requires reconciliation"
        end
      end
    else
      error_message = "saved queue is malformed"
    end
  elseif error_message == nil then
    error_message = "several saved queues match this Session"
  end
  queue.state.paused = true
  queue.state.uncertain = true
  queue.state.reason = error_message or "saved queue requires Session and outcome reconciliation"
  queue.pause_requested = true
  return queue, error_message
end

---@param self louiselm.session.Queue
---@return louiselm.session.QueueState
function Queue:inspect()
  local state = nvim.deepcopy(self.state)
  state.pending = self.busy or #self.operations > 0
  return state
end

---Return the frozen entry associated with this exact live turn, when queued.
---@param self louiselm.session.Queue
---@param turn_id string
---@return louiselm.session.QueueEntry? entry
function Queue:entry_for_turn(turn_id)
  if self.current_turn_id == turn_id and self.state.active ~= nil then
    return nvim.deepcopy(self.state.active)
  end
  if self.replacement_turn_id == turn_id and self.state.replacement ~= nil then
    return nvim.deepcopy(self.state.replacement.entry)
  end
end

local function publish(self)
  self.session.emitter:emit({ type = "queue_changed", session_id = self.session.state.id, data = self:inspect() })
end

---Fence the dead former owner and reconcile recorded turn outcomes. Failure
---leaves the saved payload visible and execution unavailable.
---@param self louiselm.session.Queue
---@param snapshot table
---@param callback fun()
function Queue:reconcile(snapshot, callback)
  self.store:owner_dead(snapshot.owner, function(dead)
    if not dead or self.stopped then
      callback()
      return
    end
    if type(snapshot.agent_running) ~= "boolean" or snapshot.agent_running then
      self.state.reason = "former Agent activity has no settled outcome"
      callback()
      return
    end
    local state = self:inspect()
    if state.replacement ~= nil then
      if
        state.replacement.status ~= "cancelling"
        and state.replacement.status ~= "acknowledged"
        and state.replacement.status ~= "unsent"
      then
        self.state.reason = "replacement dispatch outcome requires reconciliation"
        callback()
        return
      end
      state.replacement.status = "unsent"
      state.replacement.reason = "replacement was not dispatched before restart"
    end
    if state.active ~= nil and state.active_status ~= "not_sent" and state.active_turn_id == nil then
      self.state.reason = "queued dispatch outcome requires reconciliation"
      callback()
      return
    end
    local session = self.session:inspect()
    self.session.owner.recording:queue_reconciliation(
      session.agent,
      session.acp_session_id,
      state.active_turn_id,
      function(unresolved, outcome, err)
        if self.stopped then
          callback()
          return
        end
        if
          err ~= nil
          or unresolved ~= false
          or (state.active ~= nil and state.active_status ~= "not_sent" and outcome == nil)
        then
          self.state.reason = "saved queue has unresolved turn outcomes"
          callback()
          return
        end
        if
          outcome ~= nil
          and outcome ~= "not_sent"
          and outcome ~= "completed"
          and outcome ~= "cancelled"
          and outcome ~= "failed"
        then
          self.state.reason = "saved queue has an unsupported turn outcome"
          callback()
          return
        end
        if state.active ~= nil and state.active_status ~= "not_sent" then
          if outcome == "not_sent" then
            state.active_status = "not_sent"
          else
            state.active, state.active_status, state.active_turn_id = nil, nil, nil
          end
        end
        state.paused, state.uncertain = true, false
        state.reason = "restored queue requires review and Continue"
        state.revision = state.revision + 1
        self.store:save(self.id, self.state.revision, {
          queue_id = self.id,
          agent = session.agent,
          acp_session_id = session.acp_session_id,
          working_dir = session.working_dir,
          agent_running = false,
          state = state,
        }, function(save_error)
          if save_error == nil and not self.stopped then
            self.state = state
            self.recovered = false
            self.pause_requested = true
          elseif save_error ~= nil then
            self.state.reason = "former queue ownership could not be fenced"
          end
          callback()
        end)
      end
    )
  end)
end

local pump
local persist_disposal

local function mark_uncertain(self, state, err)
  self.state.paused = true
  self.state.uncertain = true
  self.state.reason = err
  self.state.uncertain_proposal = {
    entries = nvim.deepcopy(state.entries),
    active = nvim.deepcopy(state.active),
    active_status = state.active_status,
    replacement = nvim.deepcopy(state.replacement),
  }
end

local function next_operation(self)
  if self.busy or self.stopped then
    return
  end
  local operation = table.remove(self.operations, 1)
  if operation ~= nil then
    operation.run()
  else
    pump(self)
  end
end

-- Persist before publishing admission, edits, or dispatch. A lost write reply
-- is unknown: do not retry it or dispatch any later entry.
local function commit(self, state, callback)
  self.busy = true
  local session = self.session:inspect()
  local snapshot = {
    queue_id = self.id,
    agent = session.agent,
    acp_session_id = session.acp_session_id,
    working_dir = session.working_dir,
    agent_running = self.session.agent_running == true,
    state = state,
  }
  self.store:save(self.id, self.state.revision, snapshot, function(err)
    self.busy = false
    if self.stopped then
      if self.abandoned then
        return
      end
      if err == nil then
        self.state = state
        persist_disposal(self)
      elseif self.disposal_callback ~= nil then
        mark_uncertain(self, state, err)
        local finish = self.disposal_callback
        self.disposal_callback = nil
        finish(err)
      end
      return
    end
    if err ~= nil then
      mark_uncertain(self, state, err)
      local pending = self.operations
      self.operations = {}
      for _, operation in ipairs(pending) do
        if operation.cancel ~= nil then
          operation.cancel(err)
        end
      end
    else
      self.state = state
    end
    publish(self)
    if self.stopped then
      return
    end
    if callback ~= nil then
      callback(err)
    end
    next_operation(self)
  end)
end

persist_disposal = function(self)
  if self.busy then
    return
  end
  local function finish(err)
    local callback = self.disposal_callback
    self.disposal_callback = nil
    if callback ~= nil then
      callback(err)
    end
  end
  if self.recovered then
    nvim.schedule(function()
      finish("saved queue ownership must be reconciled before Disposal can be confirmed")
    end)
    return
  end
  if self.state.uncertain then
    nvim.schedule(function()
      finish("queue outcome requires reconciliation before Disposal can be confirmed")
    end)
    return
  end
  if self.state.revision == 0 then
    nvim.schedule(function()
      finish()
    end)
    return
  end
  local state = self:inspect()
  state.disposed, state.paused = true, true
  if state.replacement ~= nil then
    local replacement = state.replacement
    if replacement.cancellation == "requesting" or replacement.cancellation == "requested" then
      replacement.cancellation = "unknown"
    end
    if self.replacement_dispatched or replacement.status == "dispatched" or replacement.status == "uncertain" then
      replacement.status = "uncertain"
      replacement.reason = "Session disposed after replacement dispatch; outcome is unknown"
      state.uncertain = true
    else
      replacement.status = "unsent"
      replacement.reason = "Session disposed before replacement dispatch"
    end
    state.reason = replacement.reason
  end
  state.revision = state.revision + 1
  local session = self.session:inspect()
  self.busy = true
  self.store:save(self.id, self.state.revision, {
    queue_id = self.id,
    agent = session.agent,
    acp_session_id = session.acp_session_id,
    working_dir = session.working_dir,
    agent_running = self.session.agent_running == true,
    state = state,
  }, function(err)
    self.busy = false
    if err == nil then
      self.state = state
    else
      self.state.paused = true
      self.state.uncertain = true
      self.state.reason = err
    end
    finish(err)
  end)
end

local function enqueue_operation(self, operation, cancel)
  self.operations[#self.operations + 1] = { run = operation, cancel = cancel }
  nvim.schedule(function()
    next_operation(self)
  end)
end

local function valid_entry(prompt, metadata, display_text, contexts)
  if type(prompt) ~= "string" and type(prompt) ~= "table" then
    return false
  end
  return type(display_text) == "string"
    and type(contexts) == "table"
    and nvim.islist(contexts)
    and Routing.valid_metadata(metadata)
end

---Admit one frozen instruction asynchronously. Its callback reports the committed
---entry ID, or an error; unknown storage outcomes never authorize a retry.
---@param self louiselm.session.Queue
---@param prompt string|table
---@param metadata? louiselm.routing.SubmissionMetadata
---@param display_text string
---@param contexts table[]
---@param callback fun(id: string?, error_message?: string)
---@return boolean started
---@return string? error_message
function Queue:enqueue(prompt, metadata, display_text, contexts, callback)
  if self.stopped or self.session.state.status == "disposed" then
    return false, "Session is disposed"
  end
  if self.state.uncertain then
    return false, self.state.reason or "queue outcome is uncertain"
  end
  if not valid_entry(prompt, metadata, display_text, contexts) or type(callback) ~= "function" then
    return false, "invalid queue submission"
  end
  local id, err = identity()
  if id == nil then
    return false, err
  end
  local entry =
    nvim.deepcopy({ id = id, prompt = prompt, metadata = metadata, display_text = display_text, contexts = contexts })
  enqueue_operation(self, function()
    if self.state.uncertain then
      callback(nil, self.state.reason)
      return
    end
    if #self.state.entries >= 20 then
      callback(nil, "Session queue is full (20 waiting instructions)")
      next_operation(self)
      return
    end
    local state = self:inspect()
    state.revision = state.revision + 1
    state.entries[#state.entries + 1] = entry
    commit(self, state, function(error_message)
      callback(error_message == nil and id or nil, error_message)
    end)
  end, function(error_message)
    callback(nil, error_message)
  end)
  return true
end

local function replacement_unsent(self, target, reason, cancellation)
  enqueue_operation(self, function()
    local state = self:inspect()
    if state.replacement == nil or state.replacement.target_turn_id ~= target then
      next_operation(self)
      return
    end
    state.replacement.status, state.replacement.reason = "unsent", reason
    if
      cancellation ~= nil
      and (
        cancellation ~= "unknown"
        or state.replacement.cancellation == "requesting"
        or state.replacement.cancellation == "requested"
      )
    then
      state.replacement.cancellation = cancellation
    end
    state.paused, state.reason = true, reason
    state.revision = state.revision + 1
    commit(self, state)
  end)
end

local function cancellation_requested(self, target)
  enqueue_operation(self, function()
    local state = self:inspect()
    if
      state.replacement == nil
      or state.replacement.target_turn_id ~= target
      or state.replacement.cancellation ~= "requesting"
    then
      next_operation(self)
      return
    end
    state.replacement.cancellation = "requested"
    state.revision = state.revision + 1
    commit(self, state)
  end)
end

---Reserve one frozen replacement independently of ordinary queue capacity.
---The cancellation request follows durable admission, never the reverse.
---@param self louiselm.session.Queue
---@param target string
---@param prompt string|table
---@param metadata? louiselm.routing.SubmissionMetadata
---@param display_text string
---@param contexts table[]
---@param callback fun(id: string?, error_message?: string)
---@param cancel fun(): boolean, string?
---@return boolean started
---@return string? error_message
function Queue:stop_and_send(target, prompt, metadata, display_text, contexts, callback, cancel)
  if self.stopped or self.state.uncertain then
    return false, self.state.reason or "queue is unavailable"
  end
  if self.replacement_target ~= nil or self.state.replacement ~= nil then
    return false, "a replacement is already reserved"
  end
  if
    type(target) ~= "string"
    or target == ""
    or not valid_entry(prompt, metadata, display_text, contexts)
    or type(callback) ~= "function"
  then
    return false, "invalid replacement"
  end
  local id, err = identity()
  if id == nil then
    return false, err
  end
  local entry =
    nvim.deepcopy({ id = id, prompt = prompt, metadata = metadata, display_text = display_text, contexts = contexts })
  self.replacement_target, self.replacement_vetoed = target, false
  self.replacement_cancel_requested = false
  self.replacement_dispatched = false
  self.pause_requested = true
  enqueue_operation(self, function()
    if
      self.session.state.turn_id ~= target
      or self.session.turn_done_turn == self.session.state.current_turn
      or self.replacement_vetoed
    then
      self.replacement_target = nil
      callback(nil, "target turn changed or was stopped; review again")
      next_operation(self)
      return
    end
    local state = self:inspect()
    state.paused, state.reason = true, "stopping turn; review the queue"
    state.replacement = { entry = entry, target_turn_id = target, status = "cancelling", cancellation = "requesting" }
    state.revision = state.revision + 1
    commit(self, state, function(save_error)
      if save_error ~= nil then
        callback(nil, save_error)
        return
      end
      if self.replacement_vetoed then
        replacement_unsent(self, target, "replacement stopped before dispatch")
      elseif self.session.state.turn_id ~= target or self.session.turn_done_turn == self.session.state.current_turn then
        replacement_unsent(self, target, "target completed before cancellation", "unknown")
      else
        local turn = self.session.recording_turn
        self.replacement_cancel_requested = turn ~= nil and turn.dispatched
        local sent, cancel_error = cancel()
        if not sent then
          self.replacement_cancel_requested = false
          replacement_unsent(self, target, cancel_error or "cancellation request failed", "unknown")
        elseif not self.replacement_cancel_requested then
          replacement_unsent(self, target, "target was not sent; cancellation was not acknowledged", "not_sent")
        else
          cancellation_requested(self, target)
        end
      end
      callback(id)
    end)
  end, function(error_message)
    callback(nil, error_message)
  end)
  return true
end

---Fence a reserved replacement before its actual transport write.
---@param self louiselm.session.Queue
---@param target string
---@return boolean vetoed
function Queue:veto_replacement(target)
  local replacement = self.state.replacement
  if self.replacement_target ~= target and (replacement == nil or replacement.target_turn_id ~= target) then
    return false
  end
  local current = self.session.state.turn_id
  if current ~= target then
    if self.replacement_turn_id == nil then
      if not self.replacement_starting then
        return false
      end
    elseif current ~= self.replacement_turn_id then
      return false
    end
  end
  local turn = self.session.recording_turn
  if current ~= target and turn ~= nil and turn.dispatched then
    return false
  end
  self.replacement_vetoed = true
  self.pause_requested = true
  replacement_unsent(self, target, "replacement stopped before dispatch")
  return true
end

---Record the actual prompt response for the exact targeted turn.
---@param self louiselm.session.Queue
---@param target string
---@param cancelled boolean
function Queue:target_result(target, cancelled)
  if self.replacement_target ~= target then
    return
  end
  enqueue_operation(self, function()
    local state = self:inspect()
    if
      state.replacement == nil
      or state.replacement.target_turn_id ~= target
      or (state.replacement.status ~= "cancelling" and state.replacement.status ~= "unsent")
    then
      next_operation(self)
      return
    end
    state.replacement.cancellation = cancelled and "acknowledged" or "completed"
    if
      cancelled
      and self.replacement_cancel_requested
      and not self.replacement_vetoed
      and state.replacement.status == "cancelling"
    then
      state.replacement.status = "acknowledged"
      state.reason = "cancellation acknowledged; ordinary queue remains paused"
    else
      state.replacement.status = "unsent"
      state.replacement.reason = cancelled and "replacement stopped before dispatch"
        or "target did not acknowledge cancellation"
      state.reason = state.replacement.reason
    end
    state.paused = true
    state.revision = state.revision + 1
    commit(self, state)
  end)
end

---Recheck the live desktop authorization immediately before the transport write.
---@param self louiselm.session.Queue
---@param turn_id string
---@return boolean authorized
function Queue:replacement_authorized(turn_id)
  local replacement = self.state.replacement
  return self.replacement_turn_id == turn_id
    and replacement ~= nil
    and replacement.status == "admitting"
    and not self.replacement_vetoed
    and not self.state.uncertain
    and not self.stopped
    and self.session.state.turn_id == turn_id
    and self.session.permission_active == nil
    and #self.session.permission_queue == 0
    and #self.session.interactions == 0
end

local function change(self, revision, callback, apply)
  if self.stopped or self.state.uncertain then
    return false, self.state.reason or "queue is unavailable"
  end
  if type(revision) ~= "number" or revision % 1 ~= 0 or type(callback) ~= "function" then
    return false, "invalid queue revision or callback"
  end
  enqueue_operation(self, function()
    if revision ~= self.state.revision then
      callback(false, "queue changed; review it again")
      next_operation(self)
      return
    end
    local state = self:inspect()
    local ok, err = apply(state)
    if not ok then
      callback(false, err)
      next_operation(self)
      return
    end
    state.revision = state.revision + 1
    commit(self, state, function(error_message)
      callback(error_message == nil, error_message)
    end)
  end, function(error_message)
    callback(false, error_message)
  end)
  return true
end

local function find(entries, id)
  for index, entry in ipairs(entries) do
    if entry.id == id then
      return index
    end
  end
end

---Cancel one exact waiting entry at the reviewed revision.
---@param self louiselm.session.Queue
---@param id string
---@param revision integer
---@param callback fun(changed: boolean, error_message?: string)
---@return boolean started
---@return string? error_message
function Queue:cancel(id, revision, callback)
  return change(self, revision, function(changed, err)
    if changed and self.state.replacement == nil then
      self.replacement_target, self.replacement_turn_id = nil, nil
    end
    callback(changed, err)
  end, function(state)
    if state.replacement ~= nil and state.replacement.entry.id == id and state.replacement.status == "unsent" then
      state.replacement = nil
      return true
    end
    local index = find(state.entries, id)
    if index == nil then
      if state.active ~= nil and state.active.id == id and state.active_status == "not_sent" then
        state.active, state.active_status, state.active_turn_id = nil, nil, nil
        return true
      end
      return false, "waiting entry no longer exists"
    end
    table.remove(state.entries, index)
    return true
  end)
end

---Move one exact waiting entry to a one-based position at the reviewed revision.
---@param self louiselm.session.Queue
---@param id string
---@param position integer
---@param revision integer
---@param callback fun(changed: boolean, error_message?: string)
---@return boolean started
---@return string? error_message
function Queue:move(id, position, revision, callback)
  return change(self, revision, callback, function(state)
    local index = find(state.entries, id)
    if index == nil then
      return false, "waiting entry no longer exists"
    end
    if type(position) ~= "number" or position % 1 ~= 0 or position < 1 or position > #state.entries then
      return false, "invalid queue position"
    end
    table.insert(state.entries, position, table.remove(state.entries, index))
    return true
  end)
end

---Replace a waiting entry with a new frozen identity in the same position.
---@param self louiselm.session.Queue
---@param id string
---@param revision integer
---@param prompt string|table
---@param metadata? louiselm.routing.SubmissionMetadata
---@param display_text string
---@param contexts table[]
---@param callback fun(changed: boolean, error_message?: string)
---@return boolean started
---@return string? error_message
function Queue:replace(id, revision, prompt, metadata, display_text, contexts, callback)
  if not valid_entry(prompt, metadata, display_text, contexts) then
    return false, "invalid queue submission"
  end
  local new_id, err = identity()
  if new_id == nil then
    return false, err
  end
  local replacement = nvim.deepcopy({
    id = new_id,
    prompt = prompt,
    metadata = metadata,
    display_text = display_text,
    contexts = contexts,
  })
  return change(self, revision, callback, function(state)
    local index = find(state.entries, id)
    if index == nil then
      if state.active ~= nil and state.active.id == id and state.active_status == "not_sent" then
        if #state.entries >= 20 then
          return false, "Session queue is full (20 waiting instructions)"
        end
        table.insert(state.entries, 1, replacement)
        state.active, state.active_status, state.active_turn_id = nil, nil, nil
        return true
      end
      return false, "waiting entry no longer exists"
    end
    state.entries[index] = replacement
    return true
  end)
end

---Pause draining immediately and persist the pause in operation order.
---@param self louiselm.session.Queue
---@param reason string
function Queue:pause(reason)
  if self.stopped then
    return
  end
  self.pause_requested = true
  enqueue_operation(self, function()
    if self.state.paused or self.state.uncertain or self.state.revision == 0 then
      self.state.paused = true
      self.state.reason = reason
      publish(self)
      next_operation(self)
      return
    end
    local state = self:inspect()
    state.paused, state.reason = true, reason
    state.revision = state.revision + 1
    commit(self, state)
  end)
end

---Continue only a reviewed, known queue at its current revision.
---@param self louiselm.session.Queue
---@param revision integer
---@param callback fun(changed: boolean, error_message?: string)
---@return boolean started
---@return string? error_message
function Queue:continue(revision, callback)
  return change(self, revision, callback, function(state)
    if state.uncertain then
      return false, "queue outcome requires reconciliation"
    end
    if state.active ~= nil then
      return false, "active queue entry has not settled"
    end
    if state.replacement ~= nil then
      return false, "replacement requires review"
    end
    state.paused, state.reason = false, nil
    self.pause_requested = false
    return true
  end)
end

local function dispatch_replacement(self)
  local state = self:inspect()
  local replacement = state.replacement
  if replacement == nil or replacement.status ~= "acknowledged" then
    return false
  end
  if self.replacement_vetoed or self.session.state.turn_id ~= replacement.target_turn_id then
    replacement_unsent(self, replacement.target_turn_id, "replacement no longer authorized")
    return true
  end
  if
    self.session.state.status ~= "ready"
    or self.state.active ~= nil
    or self.session.permission_active ~= nil
    or #self.session.permission_queue > 0
    or #self.session.interactions > 0
  then
    return false
  end
  replacement.status = "admitting"
  state.revision = state.revision + 1
  commit(self, state, function(err)
    if err ~= nil or self.stopped then
      return
    end
    if
      self.replacement_vetoed
      or self.session.state.turn_id ~= replacement.target_turn_id
      or self.session.state.status ~= "ready"
    then
      replacement_unsent(self, replacement.target_turn_id, "replacement no longer authorized")
      return
    end
    local entry = replacement.entry
    self.replacement_starting = true
    local turn_id, prompt_error = self.session:prompt(entry.prompt, function(result, turn_error)
      if self.stopped then
        return
      end
      local dispatched = self.replacement_dispatched == true
      local delivery_unknown = turn_error ~= nil and dispatched and self.session.state.status == "error"
      enqueue_operation(self, function()
        local completed = self:inspect()
        if completed.replacement == nil or completed.replacement.entry.id ~= entry.id then
          next_operation(self)
          return
        end
        if delivery_unknown then
          completed.replacement.status = "uncertain"
          completed.replacement.reason = "replacement delivery outcome is unknown"
          completed.uncertain = true
        elseif not dispatched then
          completed.replacement.status = "unsent"
          completed.replacement.reason = turn_error or "replacement stopped before dispatch"
        else
          completed.replacement = nil
          self.replacement_target, self.replacement_turn_id = nil, nil
        end
        completed.paused = true
        completed.reason = turn_error or "replacement settled; review the queue"
        completed.revision = completed.revision + 1
        commit(self, completed)
      end)
    end, entry.metadata, entry.display_text)
    self.replacement_starting = false
    if turn_id == nil then
      replacement_unsent(self, replacement.target_turn_id, prompt_error or "replacement was not sent")
      return
    end
    self.replacement_turn_id = turn_id
    enqueue_operation(self, function()
      local admitting = self:inspect()
      if admitting.replacement == nil or admitting.replacement.entry.id ~= entry.id then
        next_operation(self)
        return
      end
      admitting.replacement.turn_id = turn_id
      admitting.revision = admitting.revision + 1
      commit(self, admitting)
    end)
  end)
  return true
end

pump = function(self)
  local session = self.session
  if not self.stopped and not self.busy and not self.state.uncertain and dispatch_replacement(self) then
    return
  end
  if
    self.stopped
    or self.busy
    or self.pause_requested
    or self.state.paused
    or self.state.uncertain
    or self.state.active ~= nil
    or #self.state.entries == 0
    or session.state.status ~= "ready"
    or session.permission_active ~= nil
    or #session.permission_queue > 0
    or #session.interactions > 0
  then
    return
  end
  local state = self:inspect()
  state.active = table.remove(state.entries, 1)
  state.active_status = "admitting"
  state.revision = state.revision + 1
  commit(self, state, function(err)
    if err ~= nil or self.stopped then
      return
    end
    if self.pause_requested then
      local paused = self:inspect()
      if #paused.entries < 20 then
        table.insert(paused.entries, 1, paused.active)
        paused.active, paused.active_status = nil, nil
      else
        paused.active_status = "not_sent"
      end
      paused.paused = true
      paused.reason = paused.reason or "queue stopped before dispatch"
      paused.revision = paused.revision + 1
      commit(self, paused)
      return
    end
    local entry = self.state.active
    local turn_id, prompt_error = session:prompt(entry.prompt, function(result, turn_error)
      if self.stopped then
        return
      end
      enqueue_operation(self, function()
        local completed = self:inspect()
        local delivery_unknown = completed.active_status == "uncertain"
          or (
            turn_error ~= nil
            and session.state.status == "error"
            and session.recording_turn ~= nil
            and session.recording_turn.dispatched
          )
        local not_sent = turn_error ~= nil
          and not delivery_unknown
          and (session.recording_turn == nil or not session.recording_turn.dispatched)
        if delivery_unknown or not_sent then
          completed.active_status = delivery_unknown and "uncertain" or "not_sent"
          completed.uncertain = delivery_unknown
        else
          completed.active, completed.active_status, completed.active_turn_id = nil, nil, nil
          self.current_turn_id = nil
        end
        if turn_error ~= nil or (type(result) == "table" and result.stopReason == "cancelled") then
          completed.paused = true
          completed.reason = turn_error or "turn cancelled"
          self.pause_requested = true
        end
        completed.revision = completed.revision + 1
        commit(self, completed)
      end)
    end, entry.metadata, entry.display_text)
    if turn_id == nil then
      local unsent = self:inspect()
      unsent.active_status, unsent.paused = "not_sent", true
      unsent.reason = prompt_error or "queued prompt was not sent"
      unsent.revision = unsent.revision + 1
      self.pause_requested = true
      commit(self, unsent)
    else
      self.current_turn_id = turn_id
      enqueue_operation(self, function()
        if self.state.active == nil or self.state.active.id ~= entry.id then
          next_operation(self)
          return
        end
        local admitting = self:inspect()
        admitting.active_turn_id = turn_id
        admitting.revision = admitting.revision + 1
        commit(self, admitting)
      end)
    end
  end)
end

---Observe Session lifecycle events without answering typed requests.
---@param self louiselm.session.Queue
---@param event louiselm.session.Event
function Queue:observe(event)
  if event.type == "error" or event.type == "prompt_rejected" then
    self:pause("turn failed; review the queue")
    if self.replacement_target ~= nil and self.replacement_turn_id == nil then
      replacement_unsent(
        self,
        self.replacement_target,
        "target cancellation outcome is unknown; replacement was not sent",
        "unknown"
      )
    end
  elseif event.type == "turn_done" and type(event.data) == "table" and event.data.stopReason == "cancelled" then
    self:pause("turn cancelled; review the queue")
  elseif event.type == "admission_settled" and self.replacement_turn_id == event.data.turn_id then
    local data = event.data
    if data.result == "dispatched" then
      self.replacement_dispatched = true
    end
    enqueue_operation(self, function()
      local state = self:inspect()
      if state.replacement == nil or state.replacement.turn_id ~= data.turn_id then
        next_operation(self)
        return
      end
      state.replacement.status = data.result == "dispatched" and "dispatched"
        or (data.result == "uncertain" and "uncertain" or "unsent")
      state.revision = state.revision + 1
      commit(self, state)
    end)
  elseif event.type == "admission_settled" and self.state.active ~= nil then
    local data = event.data
    enqueue_operation(self, function()
      if self.state.active == nil or self.state.active_turn_id ~= data.turn_id then
        next_operation(self)
        return
      end
      local state = self:inspect()
      state.active_status = data.result == "dispatched" and "dispatched"
        or (data.result == "uncertain" and "uncertain" or "not_sent")
      state.revision = state.revision + 1
      commit(self, state)
    end)
  elseif event.type == "turn_done" or (event.type == "state_changed" and event.data.status == "ready") then
    nvim.schedule(function()
      if not self.stopped then
        next_operation(self)
      end
    end)
  end
end

---Stop future queue effects and durably fence saved work before acknowledging Disposal.
---@param self louiselm.session.Queue
---@param callback? fun(error_message?: string)
function Queue:dispose(callback)
  if self.stopped then
    if callback ~= nil then
      nvim.schedule(function()
        callback("queue Disposal is already pending or requires reconciliation")
      end)
    end
    return
  end
  self.stopped = true
  self.pause_requested = true
  self.disposal_callback = callback
  local pending = self.operations
  self.operations = {}
  for _, operation in ipairs(pending) do
    if operation.cancel ~= nil then
      operation.cancel("Session Disposal stopped queue admission")
    end
  end
  persist_disposal(self)
end

---Stop in-memory work on editor exit without fencing saved entries as disposed.
---@param self louiselm.session.Queue
function Queue:abandon()
  self.abandoned = true
  self.stopped = true
  self.pause_requested = true
  self.operations = {}
end

return M
