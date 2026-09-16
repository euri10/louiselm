local Agent = require("louiselm.agent")
local Acp = require("louiselm.acp")
local Permission = require("louiselm.permission")
local Lifecycle = require("louiselm.session.lifecycle")
local Limits = require("louiselm.session.limits")
local Validation = require("louiselm.session.validation")
local ForensicsStore = require("louiselm.forensics.store")
local Recording = require("louiselm.session.recording")
local Paths = require("louiselm.paths")

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

---@class louiselm.session.DiscoveryOptions
---@field cwd? string Exact ACP workspace filter; omit to discover every workspace.

---@class louiselm.session.DiscoveryError
---@field agent string Configured agent definition name.
---@field message string Sanitized discovery failure.

---@alias louiselm.session.DiscoveryCallback fun(sessions: louiselm.session.DiscoveredSession[], errors: louiselm.session.DiscoveryError[])

---@class louiselm.session.DiscoveryState
---@field id integer
---@field clients table<string, louiselm.acp.Client>
---@field finished table<string, boolean>
---@field remaining integer
---@field sessions louiselm.session.DiscoveredSession[]
---@field errors louiselm.session.DiscoveryError[]
---@field callback louiselm.session.DiscoveryCallback
---@field done boolean

---@class louiselm.session.Registry: louiselm.session.Api
---@field definitions louiselm.agent.Definitions Normalized named definitions.
---@field sessions table<string, louiselm.session.Session> Live sessions by local id.
---@field order string[] Session ids in creation order.
---@field next_id integer Next local session number.
---@field next_discovery_id integer Next discovery operation number.
---@field discoveries table<integer, louiselm.session.DiscoveryState> Active adapter discovery operations.
---@field agent_limits table<string, louiselm.session.LimitsState> Last observed account-limit state by Agent.
---@field agent_limits_listeners table<fun(state: louiselm.session.LimitsState), boolean> Agent-limit observers.
---@field permission_store louiselm.permission.Store Remembered rules owned by this registry.
---@field forensics_store louiselm.forensics.Store Immutable Session Forensics records.
---@field recording louiselm.session.RecordingStore Shared durable writer; failed writes gate subsequent prompts.
---@field disposed boolean Whether this registry is closed.
---@field create_session fun(self: louiselm.session.Registry, agent_name: string, options?: louiselm.session.Options, ready_callback?: fun(session: louiselm.session.Session?, error?: string)): louiselm.session.Session?, string?
---@field load_session fun(self: louiselm.session.Registry, agent_name: string, acp_session_id: string, options?: louiselm.session.Options, ready_callback?: fun(session: louiselm.session.Session?, error?: string)): louiselm.session.Session?, string?
---@field discover_sessions fun(self: louiselm.session.Registry, options: louiselm.session.DiscoveryOptions?, callback: louiselm.session.DiscoveryCallback): boolean, string?
---@field get_session fun(self: louiselm.session.Registry, id: string): louiselm.session.Session?
---@field list_sessions fun(self: louiselm.session.Registry): string[]
---@field inspect_agent_limits fun(self: louiselm.session.Registry, agent_name: string): louiselm.session.LimitsState?, string?
---@field refresh_agent_limits fun(self: louiselm.session.Registry, agent_name: string, callback: fun(state: louiselm.session.LimitsState, error?: string)): boolean, string?
---@field on_agent_limits fun(self: louiselm.session.Registry, callback: fun(state: louiselm.session.LimitsState)): fun()?, string?
---@field handle_agent_notification fun(self: louiselm.session.Registry, session: louiselm.session.Session, message: louiselm.acp.JsonRpcNotification)
---@field list_permissions fun(self: louiselm.session.Registry): louiselm.permission.Rule[]?, string?
---@field revoke_permission fun(self: louiselm.session.Registry, id: string): boolean, string?
---@field collect_forensics fun(self: louiselm.session.Registry, agent_name: string, acp_session_id: string, options?: louiselm.session.ForensicsOptions, callback?: louiselm.session.ForensicsCallback): boolean, string?
---@field dispose fun(self: louiselm.session.Registry): boolean, string?
---@field remove_session fun(self: louiselm.session.Registry, id: string)

local M = {}
local Registry = {}
Registry.__index = Registry
local registries = {} ---@type louiselm.session.Registry[]

---@class louiselm.session.ExitVerdict
---@field session louiselm.session.Session Live Session represented by this verdict.
---@field agent string Configured Agent name.
---@field acp_session_id? string Agent-side Session identifier, once initialized.
---@field recoverable boolean Whether the Agent advertises ACP session/load.
---@field turn_active boolean Whether a prompt or autonomous agent processing is still active.

---@param self louiselm.session.Registry
---@param agent_name string
---@param state louiselm.session.LimitsState
---@return louiselm.session.LimitsState state
local function set_agent_limits(self, agent_name, state)
  self.agent_limits[agent_name] = state
  for listener in pairs(self.agent_limits_listeners) do
    listener(nvim.deepcopy(state))
  end
  return state
end

---@param definitions unknown
---@param default_skills_policy? unknown
---@return louiselm.agent.Definitions? normalized
---@return louiselm.agent.ConfigError[] errors
local function normalize_definitions(definitions, default_skills_policy)
  return Agent.normalize(definitions, default_skills_policy)
end

---@param value unknown
---@return boolean
local function valid_permission_store(value)
  return type(value) == "table"
    and type(value.evaluate) == "function"
    and type(value.remember) == "function"
    and type(value.list) == "function"
    and type(value.revoke) == "function"
    and type(value.clear_session) == "function"
end

---@param value table
---@return boolean
local function has_only_permission_store(value)
  for key in pairs(value) do
    if key ~= "permission_store" and key ~= "forensics_directory" and key ~= "usage_directory" then
      return false
    end
  end
  return true
end

---@param options louiselm.session.ConfigOption[]
---@return table<string, string|boolean>
local function forensics_options(options)
  local result = {}
  for _, option in ipairs(options) do
    if option.type == "boolean" or option.type == "select" then
      result[option.id] = option.current_value
    end
  end
  return result
end

---@param output string
---@return string? branch
---@return string[] dirty_files
local function git_status(output)
  local branch
  local dirty_files = {}
  for line in output:gmatch("[^\n]+") do
    if line:sub(1, 2) == "##" then
      branch = line:match("^## ([^%.]+)") or line:match("^## (.+)")
    elseif #dirty_files < 100 and #line >= 4 then
      local path = line:sub(4)
      if path:sub(1, 1) == '"' then
        path = path:gsub('^"(.*)"$', "%1")
      end
      dirty_files[#dirty_files + 1] = path
    end
  end
  return branch, dirty_files
end

---@param value unknown
---@return boolean
local function valid_options(value)
  if value == nil then
    return true
  end
  if type(value) ~= "table" then
    return false
  end
  ---@cast value table
  for key in pairs(value) do
    if
      key ~= "cwd"
      and key ~= "env"
      and key ~= "name"
      and key ~= "on_event"
      and key ~= "permission_policy"
      and key ~= "schedule"
      and key ~= "start_timeout_ms"
    then
      return false
    end
  end
  if value.env ~= nil then
    if type(value.env) ~= "table" then
      return false
    end
    for key, item in pairs(value.env) do
      if type(key) ~= "string" or type(item) ~= "string" then
        return false
      end
    end
  end
  return (value.cwd == nil or type(value.cwd) == "string")
    and (value.name == nil or (type(value.name) == "string" and value.name ~= ""))
end

---Create a registry after strictly normalizing named agent definitions.
---@param definitions unknown Named agent definitions.
---@param default_skills_policy? unknown Global Agent Skills policy inherited by agents without an override.
---@param options? louiselm.session.ApiOptions Headless owner options.
---@return louiselm.session.Registry? registry
---@return louiselm.agent.ConfigError[] errors
function M.new(definitions, default_skills_policy, options)
  local normalized, errors = normalize_definitions(definitions, default_skills_policy)
  if normalized == nil then
    return nil, errors
  end
  if options ~= nil and (type(options) ~= "table" or not has_only_permission_store(options)) then
    return nil, { { path = "session", message = "unknown session API option" } }
  end
  local permission_store = options and options.permission_store or Permission.store()
  if not valid_permission_store(permission_store) then
    return nil, { { path = "session.permission_store", message = "permission_store is malformed" } }
  end
  local forensics_directory = options and options.forensics_directory or nvim.fs.joinpath(Paths.state(), "forensics")
  local forensics_store, forensics_error = ForensicsStore.new(forensics_directory)
  if forensics_store == nil then
    return nil, { { path = "session.forensics_directory", message = forensics_error or "invalid directory" } }
  end
  local registry = setmetatable({
    definitions = normalized,
    sessions = {},
    order = {},
    next_id = 1,
    next_discovery_id = 1,
    discoveries = {},
    agent_limits = {},
    agent_limits_listeners = {},
    permission_store = permission_store,
    forensics_store = forensics_store,
    disposed = false,
  }, Registry)
  local recording, recording_error = Recording.new(
    options and options.usage_directory or nvim.fs.joinpath(Paths.state(), "usage"),
    function(err, pending)
      for _, session in pairs(registry.sessions) do
        session.state.recording_error = nvim.deepcopy(err or session.attribution_error)
        session.state.recording_pending = pending
        session.emitter:emit({
          type = "recording_changed",
          session_id = session.state.id,
          data = { error = nvim.deepcopy(session.state.recording_error), pending = pending },
        })
      end
    end
  )
  if recording == nil then
    return nil,
      {
        {
          path = "session.usage_directory",
          message = recording_error and recording_error.message or "invalid recording directory",
        },
      }
  end
  registry.recording = recording
  registries[#registries + 1] = registry
  return registry, {}
end

---Acknowledge pending turn facts or retry failed writes without submitting work.
---May run after Disposal to drain final facts. Callback is asynchronous and fires once.
---@param self louiselm.session.Registry
---@param callback louiselm.session.RecordingCallback
function Registry:flush_recording(callback)
  self.recording:flush(callback)
end

---Visit every live Session of this Neovim process, in registry then creation order.
---Disposed and errored Sessions are not live: they answer for no caller and
---survive no editor exit.
---@param visit fun(session: louiselm.session.Session, state: louiselm.session.State)
local function each_live_session(visit)
  for _, registry in ipairs(registries) do
    for _, id in ipairs(registry.order) do
      local session = registry.sessions[id]
      local state = session and session:inspect() or nil
      if state ~= nil and state.status ~= "disposed" and state.status ~= "error" then
        visit(session, state)
      end
    end
  end
end

---Return a process-wide snapshot of live Sessions relevant to editor exit.
---@return louiselm.session.ExitVerdict[] verdict
function M.exit_verdict()
  local verdict = {}
  each_live_session(function(session, state)
    local capabilities = session.client and session.client.agent_capabilities or {}
    verdict[#verdict + 1] = {
      session = session,
      agent = state.agent,
      acp_session_id = state.acp_session_id,
      recoverable = capabilities.loadSession == true,
      turn_active = state.status == "preparing"
        or state.status == "prompting"
        or state.status == "running"
        or state.status == "waiting_permission"
        or state.status == "cancelling",
    }
  end)
  return verdict
end

---Resolve the durable identity of the live Session a caller is running inside.
---
---The caller supplies the ACP session id its own interaction already exposes;
---this matches it against live Sessions across every headless API in the
---process. The answer depends on nothing but that id, so concurrent Sessions
---each resolve to their own identity and a chat focus change cannot alias one
---onto another. An unknown or ambiguous id is an error, never a fallback to
---whichever Session is at hand (louiselm-hmmc).
---@param acp_session_id string ACP session id exposed by the calling interaction.
---@return string? session_id Agent-scoped identity, `<agent>/<acp session id>`.
---@return string? error_message Why no single live Session answered for this caller.
function M.identity(acp_session_id)
  if type(acp_session_id) ~= "string" or acp_session_id == "" then
    return nil, "acp_session_id must be a non-empty string"
  end
  local agents = {}
  each_live_session(function(_, state)
    if state.acp_session_id == acp_session_id then
      agents[#agents + 1] = state.agent
    end
  end)
  if #agents == 0 then
    return nil, "no live Session has ACP session id " .. acp_session_id
  end
  if #agents > 1 then
    return nil,
      string.format("%d live Sessions have ACP session id %s; ask which one is calling", #agents, acp_session_id)
  end
  return agents[1] .. "/" .. acp_session_id
end

---Dispose every Session registry created in this Neovim process.
---@return boolean disposed
---@return string? error_message First disposal failure, if any.
function M.dispose_all()
  local first_error
  local current = {}
  for index, registry in ipairs(registries) do
    current[index] = registry
  end
  for _, registry in ipairs(current) do
    local _, dispose_error = registry:dispose()
    if first_error == nil and dispose_error ~= nil then
      first_error = dispose_error
    end
  end
  return first_error == nil, first_error
end

---@param self louiselm.session.Registry
---@param agent_name string
---@param acp_session_id string
---@param options? louiselm.session.ForensicsOptions
---@param callback? louiselm.session.ForensicsCallback
---@return boolean started
---@return string? error_message
function Registry:collect_forensics(agent_name, acp_session_id, options, callback)
  if self.disposed then
    return false, "session registry is disposed"
  end
  if type(agent_name) ~= "string" or agent_name == "" or self.definitions[agent_name] == nil then
    return false, "unknown agent '" .. tostring(agent_name) .. "'"
  end
  if type(acp_session_id) ~= "string" or acp_session_id == "" then
    return false, "ACP session id must be a non-empty string"
  end
  if options ~= nil and type(options) ~= "table" then
    return false, "forensics options must be a table"
  end
  options = options or {}
  if
    options.diagnosing_session_id ~= nil
    and (type(options.diagnosing_session_id) ~= "string" or options.diagnosing_session_id == "")
  then
    return false, "diagnosing Session ID must be a non-empty string"
  end
  if callback ~= nil and type(callback) ~= "function" then
    return false, "forensics callback must be a function"
  end
  local subject
  for _, id in ipairs(self.order) do
    local session = self.sessions[id]
    local state = session and session:inspect() or nil
    if state ~= nil and state.agent == agent_name and state.acp_session_id == acp_session_id then
      subject = state
      break
    end
  end
  if subject == nil then
    return false, "ACP Session is not owned by this registry"
  end
  local client = self.sessions[subject.id] and self.sessions[subject.id].client or nil
  local capabilities = client and client.agent_capabilities or {}
  local nvim_version = nvim.version()
  local record = {
    -- `tostring()` on the raw hrtime() double renders in scientific notation
    -- (e.g. "2.0264597271177e+14") once it exceeds a handful of significant
    -- digits; %d forces plain-integer formatting instead.
    id = string.format("%d-%d", os.time(), nvim.uv.hrtime()),
    observed_at = os.time(),
    subject = { agent = agent_name, acp_session_id = acp_session_id },
    diagnosing_session = options.diagnosing_session_id,
    observations = {
      agent = agent_name,
      cwd = subject.working_dir,
      options = forensics_options(subject.config_options),
      model = Validation.model_value(subject.config_options),
      neovim_version = string.format("%d.%d.%d", nvim_version.major, nvim_version.minor, nvim_version.patch),
      capabilities = {
        load_session = capabilities.loadSession == true,
        list_sessions = type(capabilities.sessionCapabilities) == "table"
          and type(capabilities.sessionCapabilities.list) == "table",
        embedded_context = subject.embedded_context == true,
      },
      dirty_files = {},
    },
    evidence_sources = {
      { kind = "acp_log", state = "omitted", mutable = true, reason = "ACP adapter did not advertise a log path" },
      { kind = "git", state = "unsupported", mutable = true, reason = "Git status is unavailable" },
    },
  }
  local function finish()
    if self.disposed then
      return
    end
    local path, write_error = self.forensics_store:write(record)
    if callback ~= nil then
      callback(path, write_error)
    end
  end
  local cwd = subject.working_dir
  if type(cwd) ~= "string" or cwd == "" then
    nvim.schedule(finish)
  else
    nvim.system({ "git", "-C", cwd, "status", "--porcelain=v1", "--branch" }, { text = true }, function(result)
      if result.code == 0 then
        local branch, dirty_files = git_status(result.stdout or "")
        record.observations.git_branch = branch
        record.observations.dirty_files = dirty_files
        record.evidence_sources[2].state = "present"
        record.evidence_sources[2].reason = nil
        nvim.system({ "git", "-C", cwd, "rev-parse", "HEAD" }, { text = true }, function(commit_result)
          if commit_result.code == 0 then
            record.observations.git_commit = (commit_result.stdout or ""):match("^%s*(.-)%s*$")
          end
          nvim.schedule(finish)
        end)
      else
        nvim.schedule(finish)
      end
    end)
  end
  return true
end

---@param load_id string? Existing ACP session id to load.
---@param self louiselm.session.Registry
---@param agent_name string Named configured agent.
---@param options? louiselm.session.Options Working directory and initial listener.
---@param ready_callback? fun(session: louiselm.session.Session?, error?: string) Called once when initialization completes.
---@return louiselm.session.Session? session
---@return string? error_message
local function start_session(self, agent_name, options, ready_callback, load_id)
  if self.disposed then
    return nil, "session registry is disposed"
  end
  if type(agent_name) ~= "string" or agent_name == "" then
    return nil, "agent name must be a non-empty string"
  end
  local definition = self.definitions[agent_name]
  if definition == nil then
    return nil, "unknown agent '" .. agent_name .. "'"
  end
  if not valid_options(options) then
    return nil, "session options must contain only a non-empty name, string cwd, and optional on_event callback"
  end
  if options == nil then
    options = {}
  end
  if options.on_event ~= nil and type(options.on_event) ~= "function" then
    return nil, "session option on_event must be a function"
  end
  if options.schedule ~= nil and type(options.schedule) ~= "function" then
    return nil, "session option schedule must be a function"
  end
  if options.start_timeout_ms ~= nil then
    local value = options.start_timeout_ms
    if type(value) ~= "number" or value < 0 or value % 1 ~= 0 then
      return nil, "session option start_timeout_ms must be a non-negative integer"
    end
  end

  local permission_policy, policy_error = Permission.policy(options.permission_policy)
  if permission_policy == nil then
    return nil, "invalid session permission policy: " .. (policy_error or "invalid policy")
  end
  local session_options = {
    cwd = options.cwd,
    env = options.env,
    name = options.name,
    on_event = options.on_event,
    permission_policy = permission_policy,
    permission_store = self.permission_store,
    schedule = options.schedule,
    start_timeout_ms = options.start_timeout_ms,
  }

  local id = "session-" .. self.next_id
  self.next_id = self.next_id + 1
  local session = Lifecycle.new(self, id, agent_name, definition, session_options, ready_callback, load_id)
  self.sessions[id] = session
  self.order[#self.order + 1] = id
  local started, start_error = session:start()
  if not started then
    self.sessions[id] = nil
    table.remove(self.order)
    return nil, start_error
  end
  return session
end

---Create and asynchronously initialize a new session for a named agent.
---@param self louiselm.session.Registry
---@param agent_name string Named configured agent.
---@param options? louiselm.session.Options Working directory and initial listener.
---@param ready_callback? fun(session: louiselm.session.Session?, error?: string) Called once when initialization completes.
---@return louiselm.session.Session? session New session, or nil on immediate failure.
---@return string? error_message Validation or immediate startup error.
function Registry:create_session(agent_name, options, ready_callback)
  return start_session(self, agent_name, options, ready_callback, nil)
end

---Load and asynchronously initialize an existing ACP session.
---@param self louiselm.session.Registry
---@param agent_name string Named configured agent.
---@param acp_session_id string Agent-side session identifier to load.
---@param options? louiselm.session.Options Working directory and initial listener.
---@param ready_callback? fun(session: louiselm.session.Session?, error?: string) Called once when loading completes.
---@return louiselm.session.Session? session Loaded session, or nil on immediate failure.
---@return string? error_message Validation or immediate startup error.
function Registry:load_session(agent_name, acp_session_id, options, ready_callback)
  if type(acp_session_id) ~= "string" or acp_session_id == "" then
    return nil, "ACP session id must be a non-empty string"
  end
  return start_session(self, agent_name, options, ready_callback, acp_session_id)
end

---@param error_value louiselm.acp.JsonRpcError|string|nil
---@return string
local function error_message(error_value)
  if type(error_value) == "table" then
    return error_value.message
  end
  return tostring(error_value or "ACP request failed")
end

---@param result louiselm.agent.ProcessResult
---@return string
local function exit_message(result)
  if result.signal ~= nil and result.signal ~= 0 then
    return "ACP agent exited with signal " .. tostring(result.signal)
  end
  return "ACP agent exited with code " .. tostring(result.code)
end

---@param sessions louiselm.session.DiscoveredSession[]
local function sort_discovered_sessions(sessions)
  table.sort(sessions, function(left, right)
    local left_updated = left.updated_at or ""
    local right_updated = right.updated_at or ""
    if left_updated ~= right_updated then
      return left_updated > right_updated
    end
    if left.agent ~= right.agent then
      return left.agent < right.agent
    end
    if left.session_id ~= right.session_id then
      return left.session_id < right.session_id
    end
    return left.cwd < right.cwd
  end)
end

---Discover sessions from all configured agents, following each opaque pagination cursor.
---@param self louiselm.session.Registry
---@param options louiselm.session.DiscoveryOptions? Optional exact workspace filter.
---@param callback louiselm.session.DiscoveryCallback Called once with validated sessions and per-agent failures.
---@return boolean started
---@return string? error_message Validation or immediate startup error.
function Registry:discover_sessions(options, callback)
  if self.disposed then
    return false, "session registry is disposed"
  end
  if options ~= nil and type(options) ~= "table" then
    return false, "discovery options must be a table"
  end
  options = options or {}
  for key in pairs(options) do
    if key ~= "cwd" then
      return false, "unknown discovery option '" .. tostring(key) .. "'"
    end
  end
  if options.cwd ~= nil and not Validation.discovery_cwd(options.cwd) then
    return false, "discovery cwd must be an absolute path"
  end
  if type(callback) ~= "function" then
    return false, "discovery callback must be a function"
  end

  local names = {}
  for name in pairs(self.definitions) do
    names[#names + 1] = name
  end
  table.sort(names)

  local discovery = {
    id = self.next_discovery_id,
    clients = {},
    finished = {},
    remaining = #names,
    sessions = {},
    errors = {},
    callback = callback,
    done = false,
  }
  self.next_discovery_id = self.next_discovery_id + 1
  self.discoveries[discovery.id] = discovery

  local function complete()
    if discovery.done or discovery.remaining > 0 then
      return
    end
    discovery.done = true
    self.discoveries[discovery.id] = nil
    sort_discovered_sessions(discovery.sessions)
    table.sort(discovery.errors, function(left, right)
      return left.agent < right.agent
    end)
    discovery.callback(discovery.sessions, discovery.errors)
  end

  for _, name in ipairs(names) do
    local client ---@type louiselm.acp.Client?
    local listed = {}
    local cursors = {}
    local finish ---@type fun(message?: string)

    finish = function(message)
      if discovery.done or discovery.finished[name] then
        return
      end
      discovery.finished[name] = true
      local active_client = discovery.clients[name]
      discovery.clients[name] = nil
      if active_client ~= nil then
        local _, close_error = active_client:close()
        if message == nil and close_error ~= nil then
          message = "could not close ACP discovery agent: " .. close_error
        end
      end
      if message ~= nil then
        discovery.errors[#discovery.errors + 1] = { agent = name, message = message }
      else
        for _, session in ipairs(listed) do
          discovery.sessions[#discovery.sessions + 1] = session
        end
      end
      discovery.remaining = discovery.remaining - 1
      complete()
    end

    local function request_page(cursor)
      if discovery.done or discovery.finished[name] then
        return
      end
      local active_client = client
      if active_client == nil then
        finish("ACP client disappeared during session discovery")
        return
      end
      local params = nvim.empty_dict()
      if options.cwd ~= nil then
        params.cwd = options.cwd
      end
      if cursor ~= nil then
        params.cursor = cursor
      end
      local request_id, request_error = active_client:list_sessions(params, function(result, rpc_error)
        if discovery.done or discovery.finished[name] then
          return
        end
        if rpc_error ~= nil then
          finish("ACP session/list failed: " .. error_message(rpc_error))
          return
        end
        local page, next_cursor, validation_error = Validation.discovery_page(result, name, options.cwd)
        if page == nil then
          finish("ACP session/list returned malformed data: " .. (validation_error or "invalid response"))
          return
        end
        for _, session in ipairs(page) do
          listed[#listed + 1] = session
        end
        if next_cursor == nil then
          finish()
          return
        end
        if cursors[next_cursor] then
          finish("ACP session/list repeated a pagination cursor")
          return
        end
        cursors[next_cursor] = true
        request_page(next_cursor)
      end)
      if request_id == nil then
        finish(request_error or "ACP session/list request could not be sent")
      end
    end

    local connect_error
    client, connect_error = Acp.connect(self.definitions[name], {
      cwd = options.cwd,
      on_error = function(message)
        finish("ACP transport error: " .. message)
      end,
      on_exit = function(result)
        finish(exit_message(result))
      end,
    })
    if client == nil then
      finish(connect_error or "could not connect to ACP agent")
    elseif discovery.finished[name] then
      client:close()
    else
      discovery.clients[name] = client
      local request_id, request_error = client:initialize(nil, function(_, rpc_error)
        if discovery.done or discovery.finished[name] then
          return
        end
        if rpc_error ~= nil then
          finish("ACP initialize failed: " .. error_message(rpc_error))
          return
        end
        request_page(nil)
      end)
      if request_id == nil then
        finish(request_error or "ACP initialize request could not be sent")
      end
    end
  end
  complete()
  return true
end

---Look up a live session by its local id.
---@param self louiselm.session.Registry
---@param id string Local session identifier.
---@return louiselm.session.Session? session
function Registry:get_session(id)
  return self.sessions[id]
end

---Return live session ids in creation order.
---@param self louiselm.session.Registry
---@return string[] ids
function Registry:list_sessions()
  local ids = {}
  for _, id in ipairs(self.order) do
    if self.sessions[id] ~= nil then
      ids[#ids + 1] = id
    end
  end
  return ids
end

---@param snapshot louiselm.session.LimitsSnapshot
---@return louiselm.session.LimitsStatus
local function snapshot_status(snapshot)
  if snapshot.unlimited then
    return "unlimited"
  end
  if #snapshot.buckets == 0 then
    return "empty"
  end
  return "fresh"
end

---@param snapshot louiselm.session.LimitsSnapshot
---@return boolean
local function snapshot_expired(snapshot)
  local now = os.time()
  for _, bucket in ipairs(snapshot.buckets) do
    for _, window in ipairs(bucket.windows) do
      if window.resets_at <= now then
        return true
      end
    end
  end
  return false
end

---@param self louiselm.session.Registry
---@param agent_name string
---@return louiselm.session.Session? source
---@return louiselm.session.LimitsCapability? capability
---@return boolean observed
local function limits_source(self, agent_name)
  local observed = false
  for _, id in ipairs(self.order) do
    local session = self.sessions[id]
    local state = session and session:inspect() or nil
    local client = session and session.client or nil
    if
      state ~= nil
      and state.agent == agent_name
      and state.status ~= "disposed"
      and state.status ~= "error"
      and client ~= nil
      and client.initialized
    then
      observed = true
      local capability = Limits.capability(client.agent_capabilities)
      if capability ~= nil then
        return session, capability, true
      end
    end
  end
  return nil, nil, observed
end

---@param self louiselm.session.Registry
---@param agent_name string
---@param message string
---@return louiselm.session.LimitsState state
local function limits_failure(self, agent_name, message)
  local previous = self.agent_limits[agent_name]
  local state = {
    agent = agent_name,
    status = previous ~= nil and previous.snapshot ~= nil and "stale" or "unavailable",
    snapshot = previous and previous.snapshot or nil,
    updated_at = previous and previous.updated_at or nil,
    error = message,
  }
  return set_agent_limits(self, agent_name, state)
end

---@param self louiselm.session.Registry
---@param agent_name string
---@param value unknown
---@return louiselm.session.LimitsState state
---@return string? error_message
local function accept_limits(self, agent_name, value)
  local snapshot, validation_error = Limits.snapshot(value)
  if snapshot == nil then
    local message = "malformed ACP account limits snapshot: " .. (validation_error or "invalid data")
    return limits_failure(self, agent_name, message), message
  end
  local state = {
    agent = agent_name,
    status = snapshot_status(snapshot),
    snapshot = snapshot,
    updated_at = os.time(),
  }
  return set_agent_limits(self, agent_name, state)
end

---Return the current Agent-level account-limit state without starting or refreshing an Agent.
---@param self louiselm.session.Registry
---@param agent_name string Configured Agent name.
---@return louiselm.session.LimitsState? state
---@return string? error_message Validation failure.
function Registry:inspect_agent_limits(agent_name)
  if self.disposed then
    return nil, "session registry is disposed"
  end
  if type(agent_name) ~= "string" or agent_name == "" or self.definitions[agent_name] == nil then
    return nil, "unknown agent '" .. tostring(agent_name) .. "'"
  end
  local source, _, observed = limits_source(self, agent_name)
  local state = self.agent_limits[agent_name]
  if state ~= nil and state.snapshot ~= nil and snapshot_expired(state.snapshot) then
    state.status = "stale"
    state.error = "account limits reset time passed without a confirmed refresh"
  end
  if state ~= nil then
    if source == nil then
      if state.snapshot ~= nil then
        state.status = "stale"
      elseif state.status == "loading" then
        state.status = "unavailable"
      end
      state.error = state.error or "no live limits-capable Session"
    end
    return nvim.deepcopy(state)
  end
  if source ~= nil then
    return { agent = agent_name, status = "loading" }
  end
  if observed then
    return { agent = agent_name, status = "unsupported" }
  end
  return { agent = agent_name, status = "not_observed" }
end

---Refresh account limits through one live capability-advertising Session for an Agent.
---@param self louiselm.session.Registry
---@param agent_name string Configured Agent name.
---@param callback fun(state: louiselm.session.LimitsState, error?: string) Completion callback.
---@return boolean started
---@return string? error_message Validation or immediate transport failure.
function Registry:refresh_agent_limits(agent_name, callback)
  if type(callback) ~= "function" then
    return false, "account limits callback must be a function"
  end
  local current, inspect_error = self:inspect_agent_limits(agent_name)
  if current == nil then
    return false, inspect_error
  end
  local source, capability = limits_source(self, agent_name)
  if source == nil or capability == nil then
    callback(current)
    return true
  end
  local previous = self.agent_limits[agent_name]
  local loading = {
    agent = agent_name,
    status = "loading",
    snapshot = previous and previous.snapshot or nil,
    updated_at = previous and previous.updated_at or nil,
  }
  set_agent_limits(self, agent_name, loading)
  local client = source.client
  if client == nil then
    local state = limits_failure(self, agent_name, "ACP account limits source disappeared")
    callback(nvim.deepcopy(state), state.error)
    return true
  end
  local source_id = source:inspect().id
  local request_id, request_error = client:request(
    capability.read_method,
    nvim.empty_dict(),
    function(result, rpc_error)
      if self.disposed or self.sessions[source_id] ~= source then
        return
      end
      if rpc_error ~= nil then
        local message = "ACP account limits read failed: " .. tostring(rpc_error.message or "request failed")
        local state = limits_failure(self, agent_name, message)
        callback(nvim.deepcopy(state), message)
        return
      end
      local state, validation_error = accept_limits(self, agent_name, result)
      callback(nvim.deepcopy(state), validation_error)
    end
  )
  if request_id == nil then
    local message = "ACP account limits read failed: " .. (request_error or "request could not be sent")
    local state = limits_failure(self, agent_name, message)
    callback(nvim.deepcopy(state), message)
    return false, message
  end
  return true
end

---Subscribe to accepted Agent-level account-limit state changes.
---@param self louiselm.session.Registry
---@param callback fun(state: louiselm.session.LimitsState) Observer; may run in an ACP fast-event callback.
---@return fun()? unsubscribe
---@return string? error_message Validation or lifecycle failure.
function Registry:on_agent_limits(callback)
  if self.disposed then
    return nil, "session registry is disposed"
  end
  if type(callback) ~= "function" then
    return nil, "account limits listener must be a function"
  end
  self.agent_limits_listeners[callback] = true
  local subscribed = true
  return function()
    if not subscribed then
      return
    end
    subscribed = false
    self.agent_limits_listeners[callback] = nil
  end
end

---Accept a custom account-limits notification from one owned Session.
---@param self louiselm.session.Registry
---@param session louiselm.session.Session Source Session.
---@param message louiselm.acp.JsonRpcNotification Raw ACP notification.
function Registry:handle_agent_notification(session, message)
  if self.disposed or type(message) ~= "table" then
    return
  end
  local session_state = session:inspect()
  if session_state.status == "disposed" or self.sessions[session_state.id] ~= session then
    return
  end
  local client = session.client
  local capability = client and Limits.capability(client.agent_capabilities) or nil
  if capability == nil or message.method ~= capability.updated_method then
    return
  end
  accept_limits(self, session_state.agent, message.params)
end

---List persistent and live remembered permission rules.
---@param self louiselm.session.Registry
---@return louiselm.permission.Rule[]? rules
---@return string? error_message State read or validation failure.
function Registry:list_permissions()
  return self.permission_store:list()
end

---Revoke one remembered permission rule.
---@param self louiselm.session.Registry
---@param id string Stable rule identifier.
---@return boolean revoked
---@return string? error_message Validation or persistence failure.
function Registry:revoke_permission(id)
  return self.permission_store:revoke(id)
end

---Dispose every live session and close the registry.
---@param self louiselm.session.Registry
---@return boolean disposed
---@return string? error_message First close error, if any.
function Registry:dispose()
  if self.disposed then
    return true
  end
  self.disposed = true
  self.agent_limits_listeners = {}
  local first_error
  for id, discovery in pairs(self.discoveries) do
    discovery.done = true
    for name, client in pairs(discovery.clients) do
      discovery.clients[name] = nil
      local _, close_error = client:close()
      if first_error == nil and close_error ~= nil then
        first_error = close_error
      end
    end
    self.discoveries[id] = nil
  end
  local ids = self:list_sessions()
  for _, id in ipairs(ids) do
    local session = self.sessions[id]
    if session ~= nil then
      local _, close_error = session:dispose()
      if first_error == nil and close_error ~= nil then
        first_error = close_error
      end
    end
  end
  for index, registry in ipairs(registries) do
    if registry == self then
      table.remove(registries, index)
      break
    end
  end
  return first_error == nil, first_error
end

---Remove one disposed session from the registry.
---@param self louiselm.session.Registry
---@param id string Local session identifier.
function Registry:remove_session(id)
  self.sessions[id] = nil
  for index, current_id in ipairs(self.order) do
    if current_id == id then
      table.remove(self.order, index)
      return
    end
  end
end

return M
