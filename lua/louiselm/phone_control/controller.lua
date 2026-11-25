---Session authority for an authenticated local phone-control connection.

---@diagnostic disable-next-line: undefined-global -- Neovim owns the process and filesystem boundaries.
local nvim = vim
local Projection = require("louiselm.phone_control.projection")
local Discovery = require("louiselm.phone_control.discovery")
local Scope = require("louiselm.phone_control.scope")
local Action = require("louiselm.phone_control.action")
local ActionJournal = require("louiselm.phone_control.action_journal")
local M = {}
local Controller = {}
Controller.__index = Controller

local MAX_LINE = 262144
local MAX_INTEGER = 9007199254740991
local codes = {
  unauthenticated = true,
  expired = true,
  revoked = true,
  out_of_scope = true,
  stale_subject = true,
  unavailable = true,
  malformed = true,
  unsupported = true,
}

local function error_result(code)
  return { v = 1, type = "error", code = code }
end

local function hex_id(value)
  return type(value) == "string" and #value == 32 and value:match("^[0-9a-f]+$") ~= nil
end

local function random_id()
  local bytes, err = nvim.uv.random(16)
  if bytes == nil then
    return nil, err
  end
  return (bytes:gsub(".", function(byte)
    return string.format("%02x", byte:byte())
  end))
end

local contains = Scope.contains
local within_project = Scope.within_project

local function parse_payload(payload)
  if type(payload) ~= "string" or #payload ~= 20 or payload:match("^[0-9a-f]+$") == nil then
    return nil
  end
  local before = tonumber(payload:sub(1, 16), 16)
  local limit = tonumber(payload:sub(17), 16)
  if before == nil or before > MAX_INTEGER or limit == nil or limit < 1 or limit > 50 then
    return nil
  end
  return before, limit
end

local function parse_send_payload(request)
  local payload = request.payload
  if
    type(payload) ~= "string"
    or #payload < 4
    or #payload > 8192
    or #payload % 2 ~= 0
    or payload:match("^[0-9a-f]+$") == nil
    or payload:sub(1, 2) ~= "01"
    or request.interaction ~= 1
    or type(request.payload_hash) ~= "string"
  then
    return nil
  end
  local bytes = payload:gsub("..", function(pair)
    return string.char(tonumber(pair, 16))
  end)
  local text = bytes:sub(2)
  if text:find("\0", 1, true) ~= nil or nvim.fn.sha256(bytes) ~= request.payload_hash then
    return nil
  end
  return text
end

local function current(self, subject_id)
  local subject = self.subjects[subject_id]
  if subject == nil then
    return nil
  end
  local session = subject.session
  local state = session:inspect()
  if state.status == "disposed" or session.owner.sessions[state.id] ~= session then
    return nil
  end
  return subject, state
end

local function action_scope(self, message, line, callback)
  local allowed = { v = true, type = true, controller_id = true, subject_id = true, operation_id = true }
  local seen, count = {}, 0
  if line:find("\\", 1, true) then
    nvim.schedule(function()
      callback(error_result("malformed"))
    end)
    return
  end
  for name in line:gmatch('"([a-z_]+)"%s*:') do
    if seen[name] then
      nvim.schedule(function()
        callback(error_result("malformed"))
      end)
      return
    end
    seen[name] = true
  end
  for name in pairs(message) do
    if not allowed[name] then
      nvim.schedule(function()
        callback(error_result("malformed"))
      end)
      return
    end
    count = count + 1
  end
  if
    count ~= 5
    or message.v ~= 1
    or not line:match('"v"%s*:%s*1%s*[,}]')
    or not hex_id(message.controller_id)
    or not hex_id(message.subject_id)
    or not hex_id(message.operation_id)
  then
    nvim.schedule(function()
      callback(error_result("malformed"))
    end)
    return
  end
  local subject, state = current(self, message.subject_id)
  if message.controller_id ~= self.controller_id or subject == nil or state == nil then
    nvim.schedule(function()
      callback(error_result("stale_subject"))
    end)
    return
  end
  if type(state.working_dir) ~= "string" or state.working_dir == "" or type(state.agent) ~= "string" then
    nvim.schedule(function()
      callback(error_result("unavailable"))
    end)
    return
  end
  self.pending = true
  local generation, controller_id = self.generation, self.controller_id
  local working_dir, agent = state.working_dir, state.agent
  nvim.uv.fs_realpath(working_dir, function(err, project)
    nvim.schedule(function()
      if self.disposed or self.generation ~= generation then
        return
      end
      self.pending = false
      local latest, now = current(self, message.subject_id)
      if
        self.controller_id ~= controller_id
        or latest ~= subject
        or now == nil
        or now.working_dir ~= working_dir
        or now.agent ~= agent
      then
        callback(error_result("stale_subject"))
      elseif err ~= nil or type(project) ~= "string" then
        callback(error_result("unavailable"))
      else
        callback({
          v = 1,
          type = "action_scope",
          controller_id = controller_id,
          subject_id = message.subject_id,
          operation_id = message.operation_id,
          project = project,
          agent = agent,
        })
      end
    end)
  end)
end

local function action_commit(self, message, line, callback)
  local pending = self.pending_action
  if pending == nil or pending.authorize == nil or pending.commit ~= nil then
    nvim.schedule(function()
      callback(error_result("unavailable"))
    end)
    return
  end
  local fields = { v = true, type = true, controller_id = true, operation_id = true, remaining_ms = true }
  local count, seen = 0, {}
  if line:find("\\", 1, true) then
    nvim.schedule(function()
      callback(error_result("malformed"))
    end)
    return
  end
  for name in line:gmatch('"([a-z_]+)"%s*:') do
    if seen[name] then
      nvim.schedule(function()
        callback(error_result("malformed"))
      end)
      return
    end
    seen[name] = true
  end
  for name in pairs(message) do
    if not fields[name] then
      nvim.schedule(function()
        callback(error_result("malformed"))
      end)
      return
    end
    count = count + 1
  end
  local remaining = message.remaining_ms
  if
    count ~= 5
    or message.v ~= 1
    or not line:match('"v"%s*:%s*1%s*[,}]')
    or message.controller_id ~= self.controller_id
    or message.operation_id ~= pending.operation_id
    or type(remaining) ~= "number"
    or remaining < 0
    or remaining > 3600000
    or remaining % 1 ~= 0
    or not line:match('"remaining_ms"%s*:%s*%d+%s*[,}]')
  then
    nvim.schedule(function()
      callback(error_result("malformed"))
    end)
    return
  end
  self.pending = true
  pending.commit = callback
  pending.authorize({
    valid = function()
      return (nvim.uv.hrtime() - pending.started) / 1000000 < remaining
    end,
    release = function() end,
  })
end

local function allocate(self, subject)
  local id, err = random_id()
  if id == nil or id == string.rep("0", 32) or self.seen_subjects[id] then
    return nil, err or "subject identity collision"
  end
  self.seen_subjects[id] = true
  subject.id = id
  self.subjects[id] = subject
  return id
end

---Attach a fresh receiver-issued controller identity to this connection.
---@param controller_id string Receiver-issued random identity.
---@return boolean attached
---@return string? error_message Invalid identity or subject allocation failure.
function Controller:attach(controller_id)
  if self.disposed or not hex_id(controller_id) then
    return false, "invalid controller identity"
  end
  if self.seen_controllers[controller_id] then
    return false, "controller identity was already used"
  end
  local detached, detach_error = self:detach()
  if not detached then
    return false, detach_error
  end
  self.seen_controllers[controller_id] = true
  self.controller_id = controller_id
  for _, subject in pairs(self.sessions) do
    local id, err = allocate(self, subject)
    if id == nil then
      self:detach()
      return false, err
    end
  end
  return true
end

---Observe one Session for the lifetime of this controller owner.
---@param session louiselm.session.Session Exact registry-owned Session.
---@return string? subject_id Current connection's identity, if attached.
---@return string? error_message Random identity or disconnected controller.
function Controller:track(session)
  local subject = self.sessions[session]
  if subject == nil then
    subject =
      { session = session, projection = Projection.new(function()
        return session:inspect().status
      end) }
    self.sessions[session] = subject
    if type(session.on) == "function" then
      subject.unsubscribe = session:on(function(event)
        subject.projection:record(event)
      end)
    end
  end
  if self.controller_id == nil then
    return nil, "controller is not attached"
  end
  if subject.id == nil then
    return allocate(self, subject)
  end
  return subject.id
end

---Stop observing a Session after its registry removes it.
---@param session louiselm.session.Session Removed Session incarnation.
function Controller:untrack(session)
  local subject = self.sessions[session]
  if subject == nil then
    return
  end
  if subject.id ~= nil then
    self.subjects[subject.id] = nil
  end
  if subject.unsubscribe ~= nil then
    subject.unsubscribe()
  end
  self.sessions[session] = nil
end

---Verify and serve a single socket line asynchronously. No response carries raw errors.
---@param line string Original bounded JSON line, without its newline.
---@param callback fun(result: table) Called once on the main loop while connected.
function Controller:read(line, callback)
  if self.disposed or self.controller_id == nil or self.pending then
    nvim.schedule(function()
      callback(error_result("unavailable"))
    end)
    return
  end
  if type(line) ~= "string" or #line + 1 > MAX_LINE then
    nvim.schedule(function()
      callback(error_result("malformed"))
    end)
    return
  end
  local ok, message = pcall(nvim.json.decode, line)
  if not ok then
    self.last_error = { code = "malformed", cause = message }
    nvim.schedule(function()
      callback(error_result("malformed"))
    end)
    return
  end
  if type(message) == "table" and message.type == "action_scope" then
    action_scope(self, message, line, callback)
    return
  end
  if type(message) == "table" and message.type == "action_commit" then
    action_commit(self, message, line, callback)
    return
  end
  if type(message) ~= "table" or type(message.request) ~= "table" or type(message.grant) ~= "table" then
    nvim.schedule(function()
      callback(error_result("malformed"))
    end)
    return
  end
  local request, grant = message.request, message.grant
  local listing = message.type == "list_sessions" and request.action == 49
  local sending = message.type == "send_prompt" and request.action == 3
  if message.v ~= 1 or not (listing or sending or (message.type == "read_session" and request.action == 1)) then
    nvim.schedule(function()
      callback(error_result("unsupported"))
    end)
    return
  end
  local before, limit = parse_payload(request.payload)
  local send_text = sending and parse_send_payload(request) or nil
  if sending and send_text ~= nil then
    before, limit = 0, 1
  end
  if listing then
    before, limit = nil, nil
    local payload = request.payload
    if type(payload) == "string" and #payload == 34 and payload:match("^[0-9a-f]+$") then
      before, limit = 0, tonumber(payload:sub(33), 16)
      if
        request.subject_id ~= string.rep("0", 32)
        or request.expected_revision ~= 0
        or limit == nil
        or limit < 1
        or limit > 50
      then
        before = nil
      end
    end
  end
  local expected = request.expected_revision
  if
    before == nil
    or (sending and (send_text == nil or self.pending_action ~= nil))
    or not hex_id(request.subject_id)
    or not hex_id(request.operation_id)
    or type(expected) ~= "number"
    or expected < 0
    or expected > MAX_INTEGER
    or expected % 1 ~= 0
    or (sending and expected == 0)
  then
    nvim.schedule(function()
      callback(error_result("malformed"))
    end)
    return
  end
  local attached = self.controller_id
  self.pending = true
  local generation = self.generation
  local function finish(result)
    if self.disposed or self.generation ~= generation then
      return
    end
    self.pending = false
    self.process = nil
    callback(result)
  end
  local argv = nvim.list_extend(nvim.deepcopy(self.service), { "phone-control", "verify" })
  local env = {}
  for _, key in ipairs({
    "PATH",
    "HOME",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "XDG_CACHE_HOME",
    "LANG",
    "LOUISELM_CAPTURE_CONFIG_DIR",
    "LOUISELM_CAPTURE_DATA_DIR",
    "LOUISELM_CAPTURE_STATE_DIR",
  }) do
    if nvim.env[key] ~= nil then
      env[key] = nvim.env[key]
    end
  end
  local spawned, process = pcall(nvim.system, argv, {
    stdin = line .. "\n",
    text = true,
    timeout = 5000,
    cwd = nvim.uv.cwd(),
    clear_env = true,
    env = env,
  }, function(result)
    nvim.schedule(function()
      if self.disposed or self.generation ~= generation then
        return
      end
      local output = type(result.stdout) == "string" and result.stdout:match("^%s*(.-)%s*$") or ""
      if result.code ~= 0 or #output > 512 then
        finish(error_result("unavailable"))
        return
      end
      if output ~= '{"v":1,"type":"verified"}' then
        local code = output:match('^%{"v":1,"type":"error","code":"([a-z_]+)"%}$')
        finish(error_result(codes[code] and code or "unavailable"))
        return
      end
      if listing then
        if self.controller_id ~= attached or message.controller_id ~= attached or request.controller_id ~= attached then
          finish(error_result("stale_subject"))
        elseif not contains(grant.groups, "read") then
          finish(error_result("out_of_scope"))
        else
          Discovery.list(self, grant, request, finish)
        end
        return
      end
      local subject, state = current(self, request.subject_id)
      if
        subject == nil
        or state == nil
        or self.controller_id ~= attached
        or message.controller_id ~= attached
        or request.controller_id ~= attached
      then
        finish(error_result("stale_subject"))
        return
      end
      if
        not contains(grant.groups, sending and "operate_sessions" or "read") or not contains(grant.agents, state.agent)
      then
        finish(error_result("out_of_scope"))
        return
      end
      local working_dir, agent = state.working_dir, state.agent
      if type(working_dir) ~= "string" or working_dir == "" then
        finish(error_result("unavailable"))
        return
      end
      nvim.uv.fs_realpath(working_dir, function(path_error, canonical)
        nvim.schedule(function()
          if self.disposed or self.generation ~= generation then
            return
          end
          local now, latest = current(self, request.subject_id)
          if
            now ~= subject
            or latest == nil
            or latest.working_dir ~= working_dir
            or latest.agent ~= agent
            or self.controller_id ~= attached
          then
            finish(error_result("stale_subject"))
          elseif path_error ~= nil or type(canonical) ~= "string" then
            finish(error_result("unavailable"))
          elseif not within_project(grant.projects, canonical) then
            finish(error_result("out_of_scope"))
          elseif sending then
            local page, page_error = subject.projection:page(0, 1, expected)
            if page == nil then
              finish(error_result(page_error or "unavailable"))
              return
            end
            local identity = {
              operation_id = request.operation_id,
              receiver_identity = request.receiver_identity,
              controller_id = attached,
              subject_id = request.subject_id,
              action = 3,
              payload_hash = request.payload_hash,
              grant_id = request.grant_id,
              grant_revision = request.grant_revision,
              period_id = request.period_id,
              expected_revision = expected,
              project = canonical,
              agent = agent,
              recorded_at_ms = os.time() * 1000,
            }
            local journal = ActionJournal.new({ service = self.service })
            local pending = { operation_id = request.operation_id, journal = journal }
            self.pending_action = pending
            local action = Action.new({
              journal = journal,
              session = subject.session,
              projection = subject.projection,
              current = function()
                return self.generation == generation
                  and self.controller_id == attached
                  and current(self, request.subject_id) == subject
              end,
              authorize = function(done)
                pending.authorize, pending.started = done, nvim.uv.hrtime()
                finish({
                  v = 1,
                  type = "action_prepared",
                  controller_id = attached,
                  subject_id = request.subject_id,
                  operation_id = request.operation_id,
                })
              end,
            })
            pending.action = action
            action:send(identity, send_text, function(outcome)
              if self.disposed or self.generation ~= generation then
                return
              end
              self.pending_action = nil
              action:dispose()
              local stopped, cause = journal:dispose()
              if not stopped then
                self.last_error = { code = "unavailable", cause = cause }
                outcome = error_result("unavailable")
              end
              if pending.commit ~= nil then
                self.pending = false
                pending.commit(outcome)
              else
                finish(outcome)
              end
            end)
          else
            local page, page_error = subject.projection:page(before, limit, expected)
            if page == nil then
              finish(error_result(page_error or "unavailable"))
              return
            end
            page.v, page.type = 1, "session_page"
            page.controller_id, page.subject_id, page.operation_id = attached, request.subject_id, request.operation_id
            local budget = MAX_LINE - 1024
            local first = 1
            for index = #page.entries, 1, -1 do
              budget = budget - #nvim.json.encode(page.entries[index]) - 1
              if budget < 0 then
                first = index + 1
                break
              end
            end
            local unfit = #page.entries > 0 and first > #page.entries
            if first > 1 and not unfit then
              page.next_before = page.entries[first].seq
              local retained = {}
              for index = first, #page.entries do
                retained[#retained + 1] = page.entries[index]
              end
              page.entries = retained
            end
            if unfit or #nvim.json.encode(page) + 1 > MAX_LINE then
              finish(error_result("unavailable"))
            else
              finish(page)
            end
          end
        end)
      end)
    end)
  end)
  if not spawned then
    self.last_error = { code = "unavailable", cause = process }
    nvim.schedule(function()
      finish(error_result("unavailable"))
    end)
  else
    self.process = process
  end
end

---Invalidate the connection and its subject identities; late callbacks are ignored.
---@return boolean detached
---@return string? error_message Cancellation failure, without process details.
function Controller:detach()
  self.generation = self.generation + 1
  self.controller_id = nil
  self.subjects = {}
  self.pending = false
  local failure
  if self.pending_action ~= nil then
    self.pending_action.action:dispose()
    local stopped, cause = self.pending_action.journal:dispose()
    self.pending_action = nil
    if not stopped then
      self.last_error = { code = "unavailable", cause = cause }
      failure = "could not stop action journal"
    end
  end
  for _, subject in pairs(self.sessions) do
    subject.id = nil
  end
  if self.process ~= nil then
    local process = self.process
    self.process = nil
    local stopped, cause = pcall(process.kill, process, 15)
    if not stopped then
      self.last_error = { code = "unavailable", cause = cause }
      failure = failure or "could not stop phone-control verifier"
    end
  end
  return failure == nil, failure
end

---Release all subscriptions and any in-flight verifier.
---@return boolean disposed
---@return string? error_message Cancellation failure, without process details.
function Controller:dispose()
  if self.disposed then
    return true
  end
  local detached, detach_error = self:detach()
  self.disposed = true
  for _, subject in pairs(self.sessions) do
    if subject.unsubscribe ~= nil then
      subject.unsubscribe()
    end
  end
  self.sessions = {}
  return detached, detach_error
end

---Create a disconnected controller with the configured capture-service argv prefix.
---@param options {service: string[]}
---@return table controller
function M.new(options)
  return setmetatable({
    service = nvim.deepcopy(options.service),
    sessions = {},
    subjects = {},
    seen_subjects = {},
    seen_controllers = {},
    generation = 0,
    disposed = false,
    pending = false,
  }, Controller)
end

return M
