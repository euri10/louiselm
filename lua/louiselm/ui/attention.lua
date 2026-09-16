---Emit unseen Session turn completions to the durable typed Attention store.

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim
local AttentionClient = require("louiselm.workflow.attention_client")
local RunClient = require("louiselm.workflow.run_client")

---@class louiselm.ui.AttentionOptions
---@field socket_path? string Local Attention socket path.
---@field capability_path? string Operator capability path.
---@field schedule? fun(delay_ms: integer, callback: fun()) Testable scheduling boundary.
---@field now_ms? fun(): integer Testable epoch clock.
---@field on_error? fun(message: string) Operator-visible transport failure.

---@class louiselm.ui.Attention
---@field socket_path string
---@field capability_path string
---@field schedule fun(delay_ms: integer, callback: fun())
---@field now_ms fun(): integer
---@field on_error fun(message: string)
---@field client louiselm.workflow.AttentionClient?
---@field connecting boolean
---@field queue table[] Pending socket operations.
---@field pending table<string, table> Unseen turn-ready entries by Session id.
---@field entries table<string, table> Unresolved Attention entries by typed key.
---@field activity_generation integer
---@field autocmd_group integer
---@field disposed boolean
---@field turn_done fun(self: louiselm.ui.Attention, state: table, seen: boolean)
---@field seen fun(self: louiselm.ui.Attention, session_id: string)
---@field prompt_started fun(self: louiselm.ui.Attention, session_id: string)
---@field permission_required fun(self: louiselm.ui.Attention, state: table, data: table)
---@field permission_resolved fun(self: louiselm.ui.Attention, session_id: string, request_id: string|number)
---@field permission_cancelled fun(self: louiselm.ui.Attention, session_id: string, request_ids: table)
---@field session_failed fun(self: louiselm.ui.Attention, state: table, linked_run_id?: string)
---@field session_resumed fun(self: louiselm.ui.Attention, session_id: string)
---@field run_parked fun(self: louiselm.ui.Attention, run_id: string)
---@field run_resumed fun(self: louiselm.ui.Attention, run_id: string)
---@field skill_approval_pending fun(self: louiselm.ui.Attention, projection: louiselm.ui.SkillAttentionProjection): boolean, string?
---@field skill_approval_resolved fun(self: louiselm.ui.Attention, projection: louiselm.ui.SkillAttentionProjection): boolean, string?
---@field skill_unverified fun(self: louiselm.ui.Attention, projection: louiselm.ui.SkillAttentionProjection, code: string): boolean, string?
---@field skill_verification_resolved fun(self: louiselm.ui.Attention, projection: louiselm.ui.SkillAttentionProjection): boolean, string?
---@field session_disposed fun(self: louiselm.ui.Attention, session_id: string)
---@field activity fun(self: louiselm.ui.Attention)
---@field dispose fun(self: louiselm.ui.Attention): boolean

local M = {}
local Attention = {}
Attention.__index = Attention

local IDLE_DELAY_MS = 30 * 1000
local enqueue

---@class louiselm.ui.SkillAttentionProjection
---@field subject_kind "session"|"run" Trusted normalized subject kind.
---@field subject_id string Trusted normalized Session identifier or Run UUID.
---@field source_operation_id string Stable canonical operation UUID.
---@field linked_run_id? string Canonical Run UUID linked to a Session condition.

local SKILL_FAILURE_CODES = {
  root_trust_failed = true,
  signature_invalid = true,
  witness_missing = true,
  native_supply_uncertain = true,
  runtime_drift = true,
  isolation_failed = true,
  broker_unavailable = true,
  audit_persistence_unavailable = true,
  provider_disclosure_missing = true,
  evidence_missing = true,
  unknown_failure = true,
}

local SKILL_PROJECTION_FIELDS = {
  subject_kind = true,
  subject_id = true,
  source_operation_id = true,
  linked_run_id = true,
}

local function valid_uuid(value)
  local compact = type(value) == "string" and value:gsub("-", "") or ""
  return type(value) == "string"
    and #value == 36
    and #compact == 32
    and value:sub(9, 9) == "-"
    and value:sub(14, 14) == "-"
    and value:sub(19, 19) == "-"
    and value:sub(24, 24) == "-"
    and compact:match("^[0-9a-fA-F]+$") ~= nil
end

local function valid_canonical_uuid(value)
  return valid_uuid(value) and value == value:lower()
end

local function validate_skill_projection(projection)
  if type(projection) ~= "table" then
    return false, "skill Attention projection must be a table"
  end
  for field in pairs(projection) do
    if SKILL_PROJECTION_FIELDS[field] ~= true then
      return false, "skill Attention projection has unknown fields"
    end
  end
  if projection.subject_kind ~= "session" and projection.subject_kind ~= "run" then
    return false, "skill Attention subject kind is invalid"
  end
  local valid_subject = type(projection.subject_id) == "string"
    and #projection.subject_id > 0
    and #projection.subject_id <= 256
    and projection.subject_id:match("^[%w_.:-]+$") ~= nil
  if projection.subject_kind == "run" then
    valid_subject = valid_canonical_uuid(projection.subject_id)
  end
  if not valid_subject or not valid_canonical_uuid(projection.source_operation_id) then
    return false, "skill Attention identifiers are invalid"
  end
  if
    projection.linked_run_id ~= nil
    and (projection.subject_kind ~= "session" or not valid_canonical_uuid(projection.linked_run_id))
  then
    return false, "skill Attention linked Run is invalid"
  end
  return true
end

local function entry_id(key)
  return table.concat({ key.subject_kind, key.subject_id, key.kind, key.source_operation_id }, "\0")
end

local function capture_state_root()
  local root = nvim.env.LOUISELM_CAPTURE_STATE_DIR
  if root == nil or root == "" then
    root = nvim.env.XDG_STATE_HOME
  end
  if root == nil or root == "" then
    root = nvim.fs.joinpath(nvim.fn.expand("~"), ".local", "state")
  end
  return root
end

local function uuid_from_seed(seed)
  local hex = nvim.fn.sha256(seed)
  local variant = string.format("%x", 8 + (tonumber(hex:sub(17, 17), 16) % 4))
  return table.concat({
    hex:sub(1, 8),
    hex:sub(9, 12),
    "4" .. hex:sub(14, 16),
    variant .. hex:sub(18, 20),
    hex:sub(21, 32),
  }, "-")
end

local function operation_id(state)
  return uuid_from_seed(table.concat({ state.agent, state.acp_session_id, tostring(state.current_turn) }, ":"))
end

local function key_for(state)
  return {
    subject_kind = "session",
    subject_id = state.acp_session_id,
    kind = "turn_ready",
    source_operation_id = operation_id(state),
  }
end

local function notify(self, message)
  if not self.disposed then
    self.on_error(message)
  end
end

local function schedule_eligibility(self, entry)
  local generation = self.activity_generation
  entry.timer_generation = generation
  self.schedule(IDLE_DELAY_MS, function()
    if self.disposed or self.entries[entry_id(entry.key)] ~= entry then
      return
    end
    if entry.timer_generation ~= generation or self.activity_generation ~= generation then
      schedule_eligibility(self, entry)
      return
    end
    enqueue(self, {
      send = function(client, callback)
        return client:set_eligible(entry.key, true, callback)
      end,
    })
  end)
end

local function flush(self)
  if self.disposed or self.client == nil or not self.client.ready then
    return
  end
  while #self.queue > 0 do
    local action = table.remove(self.queue, 1)
    local sent, send_error = action.send(self.client, function(_, error_message)
      if self.disposed then
        return
      end
      if error_message ~= nil then
        table.insert(self.queue, 1, action)
        self.client = nil
        self.connecting = false
        notify(self, error_message)
        return
      end
      if action.after ~= nil then
        action.after()
      end
    end)
    if not sent then
      table.insert(self.queue, 1, action)
      self.client = nil
      self.connecting = false
      notify(self, send_error or "Attention socket is not connected")
      return
    end
  end
end

local function ensure_connection(self)
  if self.disposed or self.connecting or (self.client ~= nil and not self.client.disposed) then
    flush(self)
    return
  end
  self.connecting = true
  local started, read_error = RunClient.read_operator_capability(
    self.capability_path,
    function(capability, error_message)
      if self.disposed then
        return
      end
      if error_message ~= nil or capability == nil then
        self.connecting = false
        notify(self, error_message or "could not read operator capability")
        return
      end
      local client, connect_error = AttentionClient.connect(self.socket_path, function()
        flush(self)
      end, {
        operator_capability = capability,
        on_error = function(message)
          if self.disposed then
            return
          end
          self.client = nil
          self.connecting = false
          notify(self, message)
        end,
      })
      if client == nil then
        self.connecting = false
        notify(self, connect_error or "could not connect to Attention socket")
        return
      end
      self.client = client
    end
  )
  if not started then
    self.connecting = false
    notify(self, read_error or "could not read operator capability")
  end
end

enqueue = function(self, action)
  if self.disposed then
    return
  end
  self.queue[#self.queue + 1] = action
  ensure_connection(self)
end

local function forget_entry(self, entry)
  local id = entry_id(entry.key)
  if self.entries[id] ~= entry then
    return false
  end
  self.entries[id] = nil
  if entry.session_id ~= nil and self.pending[entry.session_id] == entry then
    self.pending[entry.session_id] = nil
  end
  return true
end

local function clear_entry(self, entry)
  if not forget_entry(self, entry) then
    return
  end
  enqueue(self, {
    send = function(client, callback)
      return client:clear(entry.key, callback)
    end,
  })
end

local function enqueue_entry(self, entry)
  local id = entry_id(entry.key)
  if self.entries[id] ~= nil then
    return
  end
  self.entries[id] = entry
  if entry.kind == "turn_ready" then
    self.pending[entry.session_id] = entry
  end
  enqueue(self, {
    send = function(client, callback)
      return client:upsert(entry.draft, callback)
    end,
    after = function()
      if self.entries[id] == entry then
        schedule_eligibility(self, entry)
      end
    end,
  })
end

local function clear_entries(self, predicate)
  local entries = {}
  for _, entry in pairs(self.entries) do
    if predicate(entry) then
      entries[#entries + 1] = entry
    end
  end
  for _, entry in ipairs(entries) do
    clear_entry(self, entry)
  end
end

---Create a controller and observe activity across Neovim.
---@param options? louiselm.ui.AttentionOptions
---@return louiselm.ui.Attention attention
function M.new(options)
  options = options or {}
  local root = capture_state_root()
  local workflow = nvim.fs.joinpath(root, "louiselm", "workflow")
  local attention = setmetatable({
    socket_path = options.socket_path or nvim.fs.joinpath(workflow, "attention.sock"),
    capability_path = options.capability_path or nvim.fs.joinpath(workflow, "operator-capability"),
    schedule = options.schedule or function(delay_ms, callback)
      nvim.defer_fn(callback, delay_ms)
    end,
    now_ms = options.now_ms or function()
      return os.time() * 1000
    end,
    on_error = options.on_error or function(message)
      nvim.notify("louiselm: " .. message, nvim.log.levels.ERROR)
    end,
    client = nil,
    connecting = false,
    queue = {},
    pending = {},
    entries = {},
    activity_generation = 0,
    autocmd_group = 0,
    disposed = false,
  }, Attention)
  attention.autocmd_group =
    nvim.api.nvim_create_augroup("louiselm.attention." .. tostring(nvim.uv.hrtime()), { clear = true })
  nvim.api.nvim_create_autocmd({
    "CmdlineChanged",
    "CmdlineEnter",
    "CursorMoved",
    "CursorMovedI",
    "FocusGained",
    "InsertCharPre",
    "TextChanged",
    "TextChangedI",
    "TextChangedP",
  }, {
    group = attention.autocmd_group,
    callback = function()
      attention:activity()
    end,
    desc = "Track Neovim activity for LouiseLM Attention",
  })
  return attention
end

---Record a completed, currently unseen ready turn.
---@param self louiselm.ui.Attention
---@param state table Session state.
---@param seen boolean Whether the source Session buffer is currently viewed.
function Attention:turn_done(state, seen)
  if
    self.disposed
    or state.status ~= "ready"
    or type(state.acp_session_id) ~= "string"
    or state.acp_session_id == ""
  then
    return
  end
  if seen then
    self:seen(state.acp_session_id)
    return
  end
  self:prompt_started(state.acp_session_id)
  local entry = {
    session_id = state.acp_session_id,
    key = key_for(state),
    kind = "turn_ready",
    draft = {
      subject_kind = "session",
      subject_id = state.acp_session_id,
      kind = "turn_ready",
      source_operation_id = operation_id(state),
      created_at_ms = self.now_ms(),
      linked_run_id = nil,
      stage = nil,
    },
  }
  enqueue_entry(self, entry)
end

---Mark a Session condition seen and clear it durably.
---@param self louiselm.ui.Attention
---@param session_id string Agent-side Session identifier.
function Attention:seen(session_id)
  if self.disposed then
    return
  end
  local entries = {}
  for _, entry in pairs(self.entries) do
    if entry.session_id == session_id and entry.kind == "turn_ready" then
      entries[#entries + 1] = entry
    end
  end
  for _, entry in ipairs(entries) do
    forget_entry(self, entry)
  end
  enqueue(self, {
    send = function(client, callback)
      return client:clear_session_kind(session_id, "turn_ready", callback)
    end,
  })
end

---Clear a ready condition when a new prompt starts.
---@param self louiselm.ui.Attention
---@param session_id string Agent-side Session identifier.
function Attention:prompt_started(session_id)
  self:seen(session_id)
  clear_entries(self, function(entry)
    return entry.kind == "session_failed" and entry.key.subject_id == session_id
  end)
end

---Clear a terminal failure when a Session is successfully resumed.
---@param self louiselm.ui.Attention
---@param session_id string Agent-side Session identifier.
function Attention:session_resumed(session_id)
  if self.disposed then
    return
  end
  clear_entries(self, function(entry)
    return entry.kind == "session_failed" and entry.key.subject_id == session_id
  end)
end

---Record an explicit ACP permission request.
---@param self louiselm.ui.Attention
---@param state table Session state.
---@param data table Normalized permission request data.
function Attention:permission_required(state, data)
  if
    self.disposed
    or type(state.acp_session_id) ~= "string"
    or state.acp_session_id == ""
    or type(data.request_id) ~= "string" and type(data.request_id) ~= "number"
  then
    return
  end
  local request_id = tostring(data.request_id)
  local key = {
    subject_kind = "session",
    subject_id = state.acp_session_id,
    kind = "permission_required",
    source_operation_id = uuid_from_seed(
      table.concat({ "permission_required", state.acp_session_id, tostring(state.current_turn), request_id }, ":")
    ),
  }
  enqueue_entry(self, {
    session_id = state.acp_session_id,
    request_id = data.request_id,
    key = key,
    kind = key.kind,
    draft = {
      subject_kind = key.subject_kind,
      subject_id = key.subject_id,
      kind = key.kind,
      source_operation_id = key.source_operation_id,
      created_at_ms = self.now_ms(),
      linked_run_id = nil,
      stage = nil,
    },
  })
end

---Clear one resolved ACP permission request.
---@param self louiselm.ui.Attention
---@param session_id string Agent-side Session identifier.
---@param request_id string|number ACP request identifier.
function Attention:permission_resolved(session_id, request_id)
  if self.disposed then
    return
  end
  clear_entries(self, function(entry)
    return entry.kind == "permission_required"
      and entry.session_id == session_id
      and tostring(entry.request_id) == tostring(request_id)
  end)
end

---Clear ACP permission requests cancelled by the Session lifecycle.
---@param self louiselm.ui.Attention
---@param session_id string Agent-side Session identifier.
---@param request_ids table ACP request identifiers.
function Attention:permission_cancelled(session_id, request_ids)
  if self.disposed or type(request_ids) ~= "table" then
    return
  end
  local cancelled = {}
  for _, request_id in ipairs(request_ids) do
    cancelled[tostring(request_id)] = true
  end
  clear_entries(self, function(entry)
    return entry.kind == "permission_required"
      and entry.session_id == session_id
      and cancelled[tostring(entry.request_id)] == true
  end)
end

---Record a terminal Session failure with an optional linked workflow Run.
---@param self louiselm.ui.Attention
---@param state table Session state.
---@param linked_run_id? string Durable workflow Run UUID.
function Attention:session_failed(state, linked_run_id)
  if self.disposed or type(state.acp_session_id) ~= "string" or state.acp_session_id == "" then
    return
  end
  local key = {
    subject_kind = "session",
    subject_id = state.acp_session_id,
    kind = "session_failed",
    source_operation_id = uuid_from_seed(
      table.concat({ "session_failed", state.acp_session_id, tostring(state.current_turn) }, ":")
    ),
  }
  enqueue_entry(self, {
    session_id = state.acp_session_id,
    key = key,
    kind = key.kind,
    draft = {
      subject_kind = key.subject_kind,
      subject_id = key.subject_id,
      kind = key.kind,
      source_operation_id = key.source_operation_id,
      created_at_ms = self.now_ms(),
      linked_run_id = valid_uuid(linked_run_id) and linked_run_id or nil,
      stage = nil,
    },
  })
end

---Record an authoritative durable workflow Run Park.
---@param self louiselm.ui.Attention
---@param run_id string Durable Run UUID.
function Attention:run_parked(run_id)
  if self.disposed or not valid_uuid(run_id) then
    return
  end
  local key = {
    subject_kind = "run",
    subject_id = run_id,
    kind = "run_parked",
    source_operation_id = uuid_from_seed("run_parked:" .. run_id),
  }
  enqueue_entry(self, {
    key = key,
    kind = key.kind,
    draft = {
      subject_kind = key.subject_kind,
      subject_id = key.subject_id,
      kind = key.kind,
      source_operation_id = key.source_operation_id,
      created_at_ms = self.now_ms(),
      linked_run_id = nil,
      stage = nil,
    },
  })
end

---Clear an authoritative durable workflow Run Park.
---@param self louiselm.ui.Attention
---@param run_id string Durable Run UUID.
function Attention:run_resumed(run_id)
  if self.disposed then
    return
  end
  clear_entries(self, function(entry)
    return entry.kind == "run_parked" and entry.key.subject_id == run_id
  end)
end

local function skill_key(projection, kind)
  local valid, error_message = validate_skill_projection(projection)
  if not valid then
    return nil, error_message
  end
  return {
    subject_kind = projection.subject_kind,
    subject_id = projection.subject_id,
    kind = kind,
    source_operation_id = projection.source_operation_id,
  }
end

local function enqueue_skill_condition(self, projection, kind, code)
  if self.disposed then
    return false, "Attention controller is disposed"
  end
  local key, error_message = skill_key(projection, kind)
  if key == nil then
    return false, error_message
  end
  enqueue_entry(self, {
    session_id = projection.subject_kind == "session" and projection.subject_id or nil,
    key = key,
    kind = kind,
    draft = {
      subject_kind = key.subject_kind,
      subject_id = key.subject_id,
      kind = key.kind,
      source_operation_id = key.source_operation_id,
      created_at_ms = self.now_ms(),
      linked_run_id = projection.linked_run_id,
      code = code,
    },
  })
  return true
end

local function clear_skill_condition(self, projection, kind)
  if self.disposed then
    return false, "Attention controller is disposed"
  end
  local key, error_message = skill_key(projection, kind)
  if key == nil then
    return false, error_message
  end
  local entry = self.entries[entry_id(key)]
  if entry ~= nil then
    forget_entry(self, entry)
  end
  enqueue(self, {
    send = function(client, callback)
      return client:clear(key, callback)
    end,
  })
  return true
end

---Record a Skill candidate awaiting the local admission ceremony.
---@param self louiselm.ui.Attention
---@param projection louiselm.ui.SkillAttentionProjection Trusted normalized identity only.
---@return boolean accepted
---@return string? error_message
function Attention:skill_approval_pending(projection)
  return enqueue_skill_condition(self, projection, "skill_approval_pending", "admission_required")
end

---Clear one Skill approval condition after its authoritative resolution.
---@param self louiselm.ui.Attention
---@param projection louiselm.ui.SkillAttentionProjection Trusted normalized identity only.
---@return boolean accepted
---@return string? error_message
function Attention:skill_approval_resolved(projection)
  return clear_skill_condition(self, projection, "skill_approval_pending")
end

---Record one failed normalized Verified-posture operation.
---@param self louiselm.ui.Attention
---@param projection louiselm.ui.SkillAttentionProjection Trusted normalized identity only.
---@param code string Closed trusted failure code.
---@return boolean accepted
---@return string? error_message
function Attention:skill_unverified(projection, code)
  if self.disposed then
    return false, "Attention controller is disposed"
  end
  if SKILL_FAILURE_CODES[code] ~= true then
    return false, "skill Attention code is invalid"
  end
  return enqueue_skill_condition(self, projection, "skill_unverified", code)
end

---Clear one failed posture operation after trusted evidence resolves it.
---@param self louiselm.ui.Attention
---@param projection louiselm.ui.SkillAttentionProjection Trusted normalized identity only.
---@return boolean accepted
---@return string? error_message
function Attention:skill_verification_resolved(projection)
  return clear_skill_condition(self, projection, "skill_unverified")
end

---Clear failure and permission conditions when a Session ends.
---@param self louiselm.ui.Attention
---@param session_id string Agent-side Session identifier.
function Attention:session_disposed(session_id)
  if self.disposed then
    return
  end
  clear_entries(self, function(entry)
    return entry.session_id == session_id and (entry.kind == "session_failed" or entry.kind == "permission_required")
  end)
end

---Record one piece of editor activity and delay all pending eligibility.
---@param self louiselm.ui.Attention
function Attention:activity()
  if self.disposed then
    return
  end
  self.activity_generation = self.activity_generation + 1
  ensure_connection(self)
  for _, entry in pairs(self.pending) do
    schedule_eligibility(self, entry)
  end
end

---Dispose timers, the activity observer, transport, and queued operations.
---@param self louiselm.ui.Attention
---@return boolean disposed
function Attention:dispose()
  if self.disposed then
    return true
  end
  self.disposed = true
  nvim.api.nvim_del_augroup_by_id(self.autocmd_group)
  self.pending = {}
  self.queue = {}
  self.entries = {}
  if self.client ~= nil then
    self.client:dispose()
  end
  self.client = nil
  return true
end

return M
