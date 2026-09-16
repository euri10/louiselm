---Park admission and cold-resume resources, independent of Chat presentation.
local Workflow = require("louiselm.workflow")
local ParkObserver = require("louiselm.workflow.park_observer")
local ResumeController = require("louiselm.workflow.resume_controller")
local RunClient = require("louiselm.workflow.run_client")
local WorkflowService = require("louiselm.workflow.service")
local Run = require("louiselm.workflow.run")

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

---@class louiselm.workflow.EventRelay
---@field events louiselm.session.Event[]? Events awaiting attachment; nil after delivery or disposal.
---@field deliver? fun(event: louiselm.session.Event) Event ingress of the attached consumer.

---@class louiselm.workflow.RecoveryOptions
---@field enabled? boolean Explicit workflows.enabled choice; defaults to false.
---@field attention? boolean Explicit attention.enabled prerequisite; defaults to false.
---@field beads? boolean Explicit beads.enabled prerequisite; defaults to false.
---@field load_session fun(agent: string, acp_session_id: string, options: louiselm.session.Options, callback: fun(session: louiselm.session.Session?, error_message?: string)): louiselm.session.Session?, string?
---@field find_run fun(id: string): louiselm.workflow.Run?
---@field is_live fun(session: louiselm.session.Session): boolean
---@field on_park fun(presentation: louiselm.workflow.ParkPresentation)
---@field on_error fun(message: string)

---@class louiselm.workflow.RecoveredSession
---@field session louiselm.session.Session
---@field relay? louiselm.workflow.EventRelay Replay owned by a newly loaded Session; absent for a retained Session.

---@class louiselm.workflow.Recovery
---@field options louiselm.workflow.RecoveryOptions
---@field resume_client? louiselm.workflow.RunClient
---@field resume_controller? louiselm.workflow.ResumeController
---@field park_observer? louiselm.workflow.ParkObserver
---@field resume_initializing boolean
---@field resume_waiters fun(controller: louiselm.workflow.ResumeController?, error_message?: string)[]
---@field resume_revisions table<string, integer>
---@field resume_summaries table<string, louiselm.workflow.ParkSummary>
---@field pending_resume_runs table<string, louiselm.workflow.Run>
---@field pending_resume_sessions table<string, louiselm.session.Session>
---@field pending_resume_relays table<string, louiselm.workflow.EventRelay>
---@field disposed boolean
---@field prerequisite_error? string Fixed configuration failure preventing new operations.
local Recovery = {}
Recovery.__index = Recovery
local M = {}

---Construct a resource owner; does not connect until list is requested.
---@param options louiselm.workflow.RecoveryOptions
---@return louiselm.workflow.Recovery recovery
function M.new(options)
  local prerequisite_error
  for _, choice in ipairs({ { "enabled", "workflows" }, { "attention", "attention" }, { "beads", "beads" } }) do
    if options[choice[1]] ~= true then
      prerequisite_error = "Run recovery is disabled; set "
        .. choice[2]
        .. ".enabled = true and run :checkhealth louiselm"
      break
    end
  end
  return setmetatable({
    options = options,
    prerequisite_error = prerequisite_error,
    resume_initializing = false,
    resume_waiters = {},
    resume_revisions = {},
    resume_summaries = {},
    pending_resume_runs = {},
    pending_resume_sessions = {},
    pending_resume_relays = {},
    disposed = false,
  }, Recovery)
end

---Stop buffered and future event delivery; safe to repeat.
---@param relay? louiselm.workflow.EventRelay
function M.discard_relay(relay)
  if relay ~= nil then
    relay.events = nil
    relay.deliver = nil
  end
end

local PARK_GENERATED_WORK_MAX = 1
local PARK_TTL_MS = 24 * 60 * 60 * 1000

---@return string run_id
local function park_run_id()
  local seed = table.concat({ tostring(nvim.uv.hrtime()), tostring(nvim.fn.getpid()), nvim.fn.tempname() }, ":")
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

local function resume_paths()
  local root = capture_state_root()
  local workflow = nvim.fs.joinpath(root, "louiselm", "workflow")
  return nvim.fs.joinpath(workflow, "run.sock"), nvim.fs.joinpath(workflow, "operator-capability")
end

---@param session_id string Agent-scoped Session identity.
---@param cwd string Beads workspace directory.
---@param callback fun(claims: string[], error_message?: string)
---@return boolean started
---@return string? error_message
local function live_claims(session_id, cwd, callback)
  local started = pcall(nvim.system, { "br", "list", "--assignee", session_id, "--status", "in_progress", "--json" }, {
    text = true,
    cwd = cwd,
  }, function(result)
    nvim.schedule(function()
      if result.code ~= 0 then
        callback({}, result.stderr ~= "" and result.stderr or "could not query live Beads claims")
        return
      end
      local decoded_ok, decoded = pcall(nvim.json.decode, result.stdout)
      if not decoded_ok or type(decoded) ~= "table" or type(decoded.issues) ~= "table" then
        callback({}, "br returned malformed claim data")
        return
      end
      local claims = {}
      for _, issue in ipairs(decoded.issues) do
        if type(issue) ~= "table" or type(issue.id) ~= "string" or issue.id == "" then
          callback({}, "br returned malformed claim data")
          return
        end
        claims[#claims + 1] = issue.id
      end
      callback(claims)
    end)
  end)
  if not started then
    return false, "could not start Beads claim query"
  end
  return true
end

---@param self louiselm.workflow.Recovery
---@param id string
---@return louiselm.workflow.Run?
local function resume_run(self, id)
  return self.pending_resume_runs[id] or self.options.find_run(id)
end

---@param self louiselm.workflow.Recovery
---@param run louiselm.workflow.RunView
---@param callback fun(worker: louiselm.workflow.RunWorker?, error_message?: string)
local function load_cold_run(self, run, callback)
  local summary = self.resume_summaries[run.id]
  if summary == nil then
    callback(nil, "cold Run metadata is unavailable")
    return
  end
  ---@type louiselm.workflow.EventRelay
  local event_relay = { events = {} }
  self.pending_resume_relays[run.id] = event_relay
  local loading_session
  local completed, failed = false, false
  local function fail(loaded_session, error_message)
    completed, failed = true, true
    self.pending_resume_relays[run.id] = nil
    self.pending_resume_sessions[run.id] = nil
    M.discard_relay(event_relay)
    local session = loaded_session or loading_session
    if session ~= nil then
      session:dispose()
    end
    callback(nil, error_message)
  end
  local load_error
  loading_session, load_error = self.options.load_session(summary.agent, summary.acp_session_id, {
    cwd = summary.cwd,
    name = summary.acp_session_id,
    on_event = function(event)
      if event_relay.deliver ~= nil then
        event_relay.deliver(event)
        return
      end
      local events = event_relay.events
      if events ~= nil then
        events[#events + 1] = event
      end
    end,
  }, function(loaded_session, ready_error)
    if completed then
      return
    end
    if self.pending_resume_relays[run.id] ~= event_relay then
      fail(loaded_session, "cold Park load was cancelled")
      return
    end
    if ready_error ~= nil or loaded_session == nil then
      fail(loaded_session, ready_error or "cold Park load returned no Session")
      return
    end
    local local_run, run_error = Workflow.new_run({
      id = summary.id,
      claims = summary.claims,
      generated_work = summary.generated_work,
    })
    if local_run == nil then
      fail(loaded_session, run_error or "could not reconstruct resumed Run")
      return
    end
    local adopted, adopt_error = local_run:adopt_session(loaded_session)
    if not adopted then
      fail(loaded_session, adopt_error or "could not reconstruct resumed Run")
      return
    end
    completed = true
    self.pending_resume_runs[run.id] = local_run
    self.pending_resume_sessions[run.id] = loaded_session
    ---@diagnostic disable-next-line: param-type-mismatch -- Session is the production RunWorker implementation.
    callback(loaded_session)
  end)
  if not completed then
    if loading_session == nil then
      fail(nil, load_error or "could not load cold Park")
    else
      self.pending_resume_sessions[run.id] = loading_session
    end
  elseif failed and loading_session ~= nil then
    loading_session:dispose()
  end
end

---@param self louiselm.workflow.Recovery
---@param callback fun(controller: louiselm.workflow.ResumeController?, error_message?: string)
---@return boolean started
---@return string? error_message
local function ensure_resume_controller(self, callback)
  local client = self.resume_client
  if
    not self.resume_initializing
    and self.resume_controller ~= nil
    and client ~= nil
    and not client.disposed
    and client.pipe ~= nil
    and not client.pipe:is_closing()
  then
    callback(self.resume_controller)
    return true
  end
  self.resume_waiters[#self.resume_waiters + 1] = callback
  if self.resume_initializing then
    return true
  end
  self.resume_initializing = true
  if self.park_observer ~= nil then
    self.park_observer:dispose()
    self.park_observer = nil
  end
  if client ~= nil then
    client:dispose()
    self.resume_client = nil
  end
  self.resume_revisions = {}
  local settled = false
  local function finish(controller, error_message)
    if settled then
      return
    end
    settled = true
    self.resume_initializing = false
    if controller == nil then
      if self.resume_client ~= nil then
        self.resume_client:dispose()
        self.resume_client = nil
      end
      if self.park_observer ~= nil then
        self.park_observer:dispose()
        self.park_observer = nil
      end
    end
    local waiters = self.resume_waiters
    self.resume_waiters = {}
    for _, waiter in ipairs(waiters) do
      waiter(controller, error_message)
    end
  end
  local observer, observer_error = ParkObserver.new({
    find_run = function(id)
      return resume_run(self, id)
    end,
    on_park = function(presentation)
      self.options.on_park(presentation)
    end,
  })
  if observer == nil then
    finish(nil, observer_error or "could not create Park observer")
    return false, observer_error
  end
  self.park_observer = observer
  local socket_path, capability_path = resume_paths()
  local capability_started, capability_error = RunClient.read_operator_capability(
    capability_path,
    function(capability, read_error)
      if self.disposed or settled then
        return
      end
      if read_error ~= nil or capability == nil then
        finish(nil, read_error or "could not read operator capability")
        return
      end
      local connect_client, connect_error
      connect_client, connect_error = RunClient.connect(socket_path, function(runs)
        if self.disposed or self.park_observer ~= observer then
          return
        end
        local observed, observe_error = observer:observe(runs)
        if not observed then
          if not settled then
            finish(nil, observe_error or "could not reconcile Park snapshot")
          else
            self.options.on_error(observe_error or "could not reconcile Park snapshot")
          end
          return
        end
        self.resume_revisions = {}
        for _, run in ipairs(runs) do
          self.resume_revisions[run.id] = run.revision
        end
        if settled then
          return
        end
        if connect_client == nil then
          finish(nil, "Run service connected without a client")
          return
        end
        local controller, controller_error = self.resume_controller, nil
        if controller == nil then
          controller, controller_error = ResumeController.new({
            client = connect_client,
            find_run = function(id)
              return resume_run(self, id)
            end,
            load_cold = function(run, load_callback)
              load_cold_run(self, run, load_callback)
            end,
          })
        end
        if controller == nil then
          finish(nil, controller_error or "could not create resume controller")
          return
        end
        -- Keep in-flight load callbacks owned by this controller; only replace transport.
        controller.client = connect_client
        self.resume_client = connect_client
        self.resume_controller = controller
        finish(controller)
      end, {
        operator_capability = capability,
        on_error = function(message)
          if self.disposed or self.park_observer ~= observer then
            return
          end
          if not settled then
            finish(nil, message)
          elseif not self.disposed then
            self.options.on_error(message)
          end
        end,
      })
      if connect_client == nil then
        finish(nil, connect_error or "could not connect to Run service")
      else
        self.resume_client = connect_client
      end
    end
  )
  if not capability_started then
    self.resume_waiters = {}
    finish(nil, capability_error)
    return false, capability_error
  end
  return true
end

---Initialize the operator client and list durable cold Parks asynchronously.
---@param self louiselm.workflow.Recovery
---@param callback fun(runs: louiselm.workflow.ParkSummary[]?, error_message?: string)
---@return boolean started
---@return string? error_message
function Recovery:list(callback)
  if self.disposed then
    return false, "Run recovery is disposed"
  end
  if self.prerequisite_error ~= nil then
    return false, self.prerequisite_error
  end
  return ensure_resume_controller(self, function(controller, error_message)
    if self.disposed then
      return
    end
    if controller == nil then
      callback(nil, error_message or "could not connect to Run service")
      return
    end
    local started, list_error = WorkflowService.list(function(runs, err)
      if not self.disposed then
        callback(runs, err)
      end
    end)
    if not started then
      callback(nil, list_error)
    end
  end)
end

---Finalize a selected Run using its retained Session or a new load with replay.
---@param self louiselm.workflow.Recovery
---@param selected louiselm.workflow.ParkSummary
---@param callback fun(result: louiselm.workflow.RecoveredSession?, error_message?: string)
---@return boolean started
---@return string? error_message
function Recovery:resume(selected, callback)
  if self.disposed then
    return false, "Run recovery is disposed"
  end
  if self.prerequisite_error ~= nil then
    return false, self.prerequisite_error
  end
  local controller = self.resume_controller
  if controller == nil then
    return false, "Run recovery is not initialized"
  end
  local revision = self.resume_revisions[selected.id]
  if revision == nil then
    return false, "selected Run revision is unavailable"
  end
  self.resume_summaries[selected.id] = selected
  local resume_view = {
    id = selected.id,
    revision = revision,
    state = selected.state,
    generated_work_ceiling = selected.generated_work.ceiling,
    generated_work_consumed = selected.generated_work.consumed,
    generated_work_reserved = selected.generated_work.reserved,
    pending_mutation_ids = {},
    park_expires_at_ms = selected.expires_at_ms,
  }
  local started, resume_error = controller:resume(resume_view, function(_, error_message)
    local session = self.pending_resume_sessions[selected.id]
    local relay = self.pending_resume_relays[selected.id]
    self.pending_resume_sessions[selected.id] = nil
    self.pending_resume_relays[selected.id] = nil
    self.pending_resume_runs[selected.id] = nil
    self.resume_summaries[selected.id] = nil
    if error_message ~= nil then
      M.discard_relay(relay)
      callback(nil, error_message)
      return
    end
    if session == nil then
      M.discard_relay(relay)
      local live = self.options.find_run(selected.id)
      local retained = live ~= nil and #live.workers == 1 and live.workers[1] or nil
      -- Recovery only admits ACP Sessions; the consumer verifies attachment below.
      ---@diagnostic disable-next-line: cast-type-mismatch -- RunWorker erases Session-specific fields at the shared Run boundary.
      ---@cast retained louiselm.session.Session?
      if retained ~= nil and self.options.is_live(retained) then
        local state = retained:inspect()
        if state.agent == selected.agent and state.acp_session_id == selected.acp_session_id then
          callback({ session = retained })
          return
        end
      end
      callback(nil, "resumed Run has no loaded Session")
      return
    end
    if relay == nil or relay.events == nil then
      session:dispose()
      callback(nil, "resumed Run has no Session event relay")
      return
    end
    callback({ session = session, relay = relay })
  end)
  if not started then
    self.resume_summaries[selected.id] = nil
  end
  return started, resume_error
end

---Cold-Park an attached Session, preserving an already admitted Run's identity.
---@param self louiselm.workflow.Recovery
---@param session louiselm.session.Session
---@param callback fun(run_id: string?, error_message?: string)
---@return boolean started
---@return string? error_message
function Recovery:park(session, callback)
  if self.disposed then
    return false, "Run recovery is disposed"
  end
  if self.prerequisite_error ~= nil then
    return false, self.prerequisite_error
  end
  local allowed, prerequisite_error = Run.check_cold_park(session)
  if not allowed then
    return false, prerequisite_error
  end
  local state = session:inspect()
  if state.acp_session_id == nil then
    return false, "current Session has no ACP Session id yet"
  end
  local session_id = state.agent .. "/" .. state.acp_session_id
  return ensure_resume_controller(self, function(controller, readiness_error)
    if self.disposed or not self.options.is_live(session) then
      return
    end
    if controller == nil then
      callback(nil, readiness_error or "Run service is unavailable")
      return
    end
    local started, claims_error = live_claims(session_id, state.working_dir, function(claims, claim_error)
      if self.disposed or not self.options.is_live(session) then
        return
      end
      if claim_error ~= nil then
        callback(nil, claim_error)
        return
      end
      local run = session.owner_run
      local run_id = run and run.id or park_run_id()
      if run == nil then
        local run_error
        run, run_error = Workflow.new_run({ id = run_id })
        if run == nil then
          callback(nil, run_error)
          return
        end
      end
      local function cold_park()
        local started, park_error = run:park_cold({ id = run_id, claims = claims }, function(ok, error_message)
          callback(ok and run_id or nil, error_message)
        end)
        if not started then
          callback(nil, park_error or "could not cold-Park Session")
        end
      end
      if session.owner_run ~= nil then
        cold_park()
        return
      end
      local admitted, admission_error = WorkflowService.admit({
        id = run_id,
        generated_work_max = PARK_GENERATED_WORK_MAX,
        park_ttl_ms = PARK_TTL_MS,
      }, function(token, error_message)
        if self.disposed or not self.options.is_live(session) then
          return
        end
        if token == nil then
          callback(nil, error_message or "could not admit Run")
          return
        end
        local attached, attach_error = WorkflowService.attach({
          id = run_id,
          session_id = session_id,
          agent = state.agent,
          acp_session_id = state.acp_session_id,
          cwd = state.working_dir,
          load_session = true,
        }, function(ok, error_message)
          if not ok then
            callback(nil, error_message or "could not attach Session to Run")
            return
          end
          local adopted, adopt_error = run:adopt_session(session)
          if not adopted then
            callback(nil, adopt_error or "could not attach Session to Run")
            return
          end
          cold_park()
        end)
        if not attached then
          callback(nil, attach_error or "could not attach Session to Run")
        end
      end)
      if not admitted then
        callback(nil, admission_error or "could not admit Run")
      end
    end)
    if not started then
      callback(nil, claims_error)
    end
  end)
end

---Release owned connections and pending loads; attached Sessions belong to the consumer.
---@param self louiselm.workflow.Recovery
---@return boolean disposed
function Recovery:dispose()
  if self.disposed then
    return true
  end
  self.disposed = true
  self.resume_waiters = {}
  self.resume_initializing = false
  if self.park_observer ~= nil then
    self.park_observer:dispose()
  end
  if self.resume_controller ~= nil then
    self.resume_controller:dispose()
  end
  if self.resume_client ~= nil then
    self.resume_client:dispose()
  end
  for _, event_relay in pairs(self.pending_resume_relays) do
    M.discard_relay(event_relay)
  end
  self.pending_resume_relays = {}
  for _, session in pairs(self.pending_resume_sessions) do
    session:dispose()
  end
  self.pending_resume_sessions = {}
  self.pending_resume_runs = {}
  self.resume_summaries = {}
  return true
end

return M
