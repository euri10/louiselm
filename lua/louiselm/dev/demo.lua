local Events = require("louiselm.session.events")

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local M = {}
local Api = {}
local Session = {}
Api.__index = Api
Session.__index = Session

local AGENTS = {
  ["your-codex-here"] = true,
  ["your-claude-here"] = true,
}
local COPY = {
  en = {
    label = "[scripted demo — no Agent or Provider connected]",
    already_fixed = "The calculator is already fixed.",
    proposal = "I inspected the attached calculator and found `add` subtracting its right operand. Review this one-line fix.",
    allowed = "The in-memory file now returns `left + right`.",
    rejected = "No file changed. Send another prompt whenever you want to retry.",
    cancelled = "The scripted turn was cancelled.",
    handoff = "Handoff received. I have the reviewed context and takeover task in this separate Session.",
    seeded_user_first = "Inspect the calculator test failure.",
    seeded_reply_first = "The failing assertion points at `calculator.add`; inspect its implementation next.",
    seeded_user_second = "Keep this Session for later.",
    seeded_reply_second = "Session saved. Resume can restore this history.",
  },
  ["zh-CN"] = {
    label = "[脚本化演示——未连接任何 Agent 或 Provider]",
    already_fixed = "计算器已经修复。",
    proposal = "我检查了已附加的计算器，发现 `add` 错把右操作数相减。请审查这一行修改。",
    allowed = "内存文件现在会返回 `left + right`。",
    rejected = "文件没有变化。你可以随时再发一条提示重试。",
    cancelled = "脚本化回合已取消。",
    handoff = "已收到 Handoff。我已在这个独立 Session 中取得审查后的上下文和接管任务。",
    seeded_user_first = "检查计算器测试为何失败。",
    seeded_reply_first = "失败的断言指向 `calculator.add`；下一步请检查它的实现。",
    seeded_user_second = "保留这个 Session，稍后继续。",
    seeded_reply_second = "Session 已保存；Resume 可以恢复这段历史。",
  },
}
local BROKEN_EXPRESSION = "return left - right"
local FIXED_EXPRESSION = "return left + right"
local SEEDED_SESSION_ID = "scripted-resume-1"

---@param path string
---@return string? content
---@return string? error_message
local function read_file(path)
  local stat = nvim.uv.fs_stat(path)
  if stat == nil or stat.type ~= "file" then
    return nil, "demo project is missing " .. path
  end
  local file, open_error = io.open(path, "rb")
  if file == nil then
    return nil, "could not read demo file: " .. tostring(open_error)
  end
  local content = file:read("*a")
  file:close()
  return content
end

---@param path string
---@param content string
---@return boolean written
---@return string? error_message
local function write_file(path, content)
  local file, open_error = io.open(path, "wb")
  if file == nil then
    return false, "could not write demo file: " .. tostring(open_error)
  end
  local written, write_error = file:write(content)
  local closed, close_error = file:close()
  if written == nil or not closed then
    return false, "could not write demo file: " .. tostring(write_error or close_error)
  end
  return true
end

---@param value unknown
---@return boolean present
local function has_prompt_text(value)
  if type(value) == "string" then
    return value ~= ""
  end
  if type(value) ~= "table" then
    return false
  end
  for _, block in ipairs(value) do
    if type(block) == "table" then
      if type(block.text) == "string" and block.text ~= "" then
        return true
      end
      if type(block.uri) == "string" and block.uri ~= "" then
        return true
      end
      if type(block.resource) == "table" and has_prompt_text({ block.resource }) then
        return true
      end
    end
  end
  return false
end

---@param value unknown
---@param needle string
---@return boolean present
local function prompt_contains(value, needle)
  if type(value) == "string" then
    return value:find(needle, 1, true) ~= nil
  end
  if type(value) ~= "table" then
    return false
  end
  if type(value.text) == "string" and value.text:find(needle, 1, true) ~= nil then
    return true
  end
  if type(value.resource) == "table" and prompt_contains(value.resource, needle) then
    return true
  end
  for _, block in ipairs(value) do
    if prompt_contains(block, needle) then
      return true
    end
  end
  return false
end

---@param self table
---@param status louiselm.session.Status
local function set_status(self, status)
  if self.state.status == status then
    return
  end
  self.state.status = status
  self.emitter:emit({
    type = "state_changed",
    session_id = self.state.id,
    data = { status = status },
  })
end

---@param self table
---@param event_type louiselm.session.EventType
---@param data unknown
local function emit(self, event_type, data)
  self.emitter:emit({ type = event_type, session_id = self.state.id, data = data })
end

---@param self table
---@param revision integer
---@param outcome "allowed"|"rejected"|"cancelled"
local function finish_turn(self, revision, outcome)
  if self.revision ~= revision or self.state.status == "disposed" then
    return
  end
  local status = outcome == "allowed" and "completed" or "cancelled"
  emit(self, "tool_call_finished", {
    toolCallId = "demo-edit-" .. self.state.current_turn,
    title = "Fix calculator.add",
    status = status,
  })
  local message = outcome == "allowed" and self.api.copy.allowed
    or outcome == "rejected" and self.api.copy.rejected
    or self.api.copy.cancelled
  emit(self, "chunk", { content = { type = "text", text = message } })
  self.pending_permission = nil
  set_status(self, "ready")
  emit(self, "turn_done", { stopReason = outcome == "cancelled" and "cancelled" or "end_turn" })
end

---@param self table
---@param revision integer
local function propose_edit(self, revision)
  if self.revision ~= revision or self.state.status ~= "prompting" then
    return
  end
  local content, read_error = read_file(self.calculator_path)
  if content == nil then
    emit(self, "error", { message = read_error })
    set_status(self, "ready")
    return
  end
  local start = content:find(BROKEN_EXPRESSION, 1, true)
  if start == nil then
    emit(self, "chunk", {
      content = { type = "text", text = self.api.copy.label .. "\n\n" .. self.api.copy.already_fixed },
    })
    set_status(self, "ready")
    emit(self, "turn_done", { stopReason = "end_turn" })
    return
  end
  local proposed = content:sub(1, start - 1) .. FIXED_EXPRESSION .. content:sub(start + #BROKEN_EXPRESSION)
  local line_start = content:sub(1, start - 1):match(".*\n()") or 1
  local line_end = content:find("\n", start, true) or (#content + 1)
  local broken_line = content:sub(line_start, line_end - 1)
  local expression_start = assert(broken_line:find(BROKEN_EXPRESSION, 1, true))
  local fixed_line = broken_line:sub(1, expression_start - 1)
    .. FIXED_EXPRESSION
    .. broken_line:sub(expression_start + #BROKEN_EXPRESSION)
  local edit_diff = "@@ -4 +4 @@\n-" .. broken_line .. "\n+" .. fixed_line .. "\n"
  local tool_id = "demo-edit-" .. self.state.current_turn
  emit(self, "chunk", {
    content = {
      type = "text",
      text = self.api.copy.label .. "\n\n" .. self.api.copy.proposal,
    },
  })
  emit(self, "tool_call_started", {
    toolCallId = tool_id,
    title = "Fix calculator.add",
    kind = "edit",
    status = "pending",
  })
  local permission = { answered = false }
  self.pending_permission = permission
  set_status(self, "waiting_permission")
  self.emitter:emit({
    type = "permission_requested",
    session_id = self.state.id,
    data = {
      request_id = "demo-permission-" .. self.state.current_turn,
      operation = { kind = "file_edit", path = self.calculator_path, diff = edit_diff },
      policy_decision = "ask",
      options = {
        { optionId = "reject-once", kind = "reject_once", name = "Reject" },
        { optionId = "allow-once", kind = "allow_once", name = "Allow once" },
      },
      toolCall = {
        toolCallId = tool_id,
        kind = "edit",
        title = "Fix calculator.add",
        rawInput = { path = self.calculator_path, diff = edit_diff },
      },
    },
    respond = function(result)
      if permission.answered then
        return false, "permission request was already answered"
      end
      if self.revision ~= revision or self.state.status == "disposed" then
        return false, "permission request is no longer active"
      end
      local outcome = type(result) == "table" and type(result.outcome) == "table" and result.outcome or nil
      local selected = outcome ~= nil and outcome.outcome == "selected" and outcome.optionId or nil
      local cancelled = outcome ~= nil and outcome.outcome == "cancelled"
      if selected ~= "allow-once" and selected ~= "reject-once" and not cancelled then
        return false, "demo permission response must select an advertised option"
      end
      permission.answered = true
      self.api.schedule(80, function()
        if self.revision ~= revision or self.state.status == "disposed" then
          return
        end
        if selected == "allow-once" then
          local written, write_error = write_file(self.calculator_path, proposed)
          if not written then
            emit(self, "error", { message = write_error })
            set_status(self, "ready")
            return
          end
          local buffer = nvim.fn.bufnr(self.calculator_path)
          if buffer ~= -1 and nvim.api.nvim_buf_is_loaded(buffer) then
            nvim.api.nvim_buf_set_lines(buffer, 0, -1, false, nvim.fn.readfile(self.calculator_path))
            nvim.bo[buffer].modified = false
          end
          finish_turn(self, revision, "allowed")
        elseif selected == "reject-once" then
          finish_turn(self, revision, "rejected")
        else
          finish_turn(self, revision, "cancelled")
        end
      end)
      return true
    end,
  })
end

---@param self table
---@param revision integer
local function acknowledge_handoff(self, revision)
  if self.revision ~= revision or self.state.status ~= "prompting" then
    return
  end
  emit(self, "chunk", {
    content = {
      type = "text",
      text = self.api.copy.label .. "\n\n" .. self.api.copy.handoff,
    },
  })
  set_status(self, "ready")
  emit(self, "turn_done", { stopReason = "end_turn" })
end

---Subscribe to typed events from this browser-local Session.
---@param callback louiselm.session.EventCallback
---@return fun() unsubscribe
function Session:on(callback)
  return self.emitter:on(callback)
end

---Return a copy of this browser-local Session state.
---@return louiselm.session.State state
function Session:inspect()
  return nvim.deepcopy(self.state)
end

---Rename this browser-local Session.
---@param name string
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
  emit(self, "state_changed", { status = self.state.status })
  return true
end

---Start one deterministic browser-local turn.
---@param prompt louiselm.session.Prompt
---@return integer? request_id
---@return string? error_message
function Session:prompt(prompt)
  if self.state.status ~= "ready" then
    return nil, "session is not ready"
  end
  if not has_prompt_text(prompt) then
    return nil, "prompt must contain non-empty text or context"
  end
  self.state.current_turn = self.state.current_turn + 1
  self.revision = self.revision + 1
  local revision = self.revision
  local handoff = prompt_contains(prompt, "## Handoff")
  set_status(self, "prompting")
  self.api.schedule(120, function()
    if handoff then
      acknowledge_handoff(self, revision)
    else
      propose_edit(self, revision)
    end
  end)
  return self.state.current_turn
end

---Cancel the active browser-local turn.
---@return boolean cancelled
---@return string? error_message
function Session:cancel()
  if self.state.status ~= "prompting" and self.state.status ~= "waiting_permission" then
    return false, "session is not prompting"
  end
  if self.pending_permission ~= nil then
    self.pending_permission.answered = true
    emit(self, "permission_cancelled", { request_ids = { "demo-permission-" .. self.state.current_turn } })
  end
  self.revision = self.revision + 1
  local revision = self.revision
  set_status(self, "cancelling")
  self.api.schedule(40, function()
    finish_turn(self, revision, "cancelled")
  end)
  return true
end

---Reject configuration changes because this introductory Session advertises none.
---@return nil
---@return string error_message
function Session:set_config_option()
  return nil, "demo session advertises no configuration options"
end

---Dispose this browser-local Session and suppress late scheduled events.
---@return boolean disposed
function Session:dispose()
  if self.state.status == "disposed" then
    return true
  end
  self.revision = self.revision + 1
  self.pending_permission = nil
  set_status(self, "disposed")
  self.api:remove_session(self.state.id)
  self.emitter:clear()
  return true
end

---@param self table
---@param id string
function Api:remove_session(id)
  self.sessions[id] = nil
  for index, candidate in ipairs(self.order) do
    if candidate == id then
      table.remove(self.order, index)
      return
    end
  end
end

---@param api table
---@param agent_name string
---@param options table
---@param source "new"|"loaded"
---@param acp_session_id string
---@param status louiselm.session.Status
---@return table session
local function add_session(api, agent_name, options, source, acp_session_id, status)
  api.next_id = api.next_id + 1
  local id = "demo-" .. api.next_id
  local session = setmetatable({
    api = api,
    emitter = Events.new(),
    revision = 0,
    calculator_path = api.project_root .. "/lua/calculator.lua",
    client = { agent_capabilities = { loadSession = true } },
    state = {
      id = id,
      name = options.name or id,
      source = source,
      agent = agent_name,
      acp_session_id = acp_session_id,
      status = status,
      working_dir = options.cwd or api.project_root,
      current_turn = 0,
      config_options = {},
      commands = {},
      skills_policy = "off",
      embedded_context = true,
    },
  }, Session)
  if type(options.on_event) == "function" then
    session:on(options.on_event)
  end
  api.sessions[id] = session
  api.order[#api.order + 1] = id
  return session
end

---Create a deterministic browser-local Session for one placeholder Agent.
---@param agent_name string
---@param options? louiselm.session.Options
---@param ready_callback? fun(session: louiselm.session.Session?, error?: string)
---@return table? session
---@return string? error_message
function Api:create_session(agent_name, options, ready_callback)
  if self.disposed then
    return nil, "demo API is disposed"
  end
  if not AGENTS[agent_name] then
    return nil, "unknown demo Agent '" .. tostring(agent_name) .. "'"
  end
  options = options or {}
  if type(options) ~= "table" then
    return nil, "session options must be a table"
  end
  local session = add_session(self, agent_name, options, "new", "scripted-new-" .. (self.next_id + 1), "ready")
  if ready_callback ~= nil then
    self.schedule(0, function()
      if session:inspect().status ~= "disposed" then
        ready_callback(session)
      end
    end)
  end
  return session
end

---Discover the one seeded recoverable Session used by the Resume lesson.
---@param options? louiselm.session.DiscoveryOptions
---@param callback louiselm.session.DiscoveryCallback
---@return boolean started
---@return string? error_message
function Api:discover_sessions(options, callback)
  if self.disposed then
    return false, "demo API is disposed"
  end
  options = options or {}
  if type(options) ~= "table" then
    return false, "discovery options must be a table"
  end
  for key in pairs(options) do
    if key ~= "cwd" then
      return false, "unknown discovery option '" .. tostring(key) .. "'"
    end
  end
  if type(callback) ~= "function" then
    return false, "discovery callback must be a function"
  end
  self.schedule(80, function()
    if self.disposed then
      return
    end
    local sessions = {}
    if options.cwd == nil or nvim.fs.normalize(options.cwd) == self.project_root then
      sessions[1] = {
        agent = "your-codex-here",
        session_id = SEEDED_SESSION_ID,
        cwd = self.project_root,
        title = "Continue the calculator investigation",
        updated_at = "2026-09-04T18:00:00Z",
      }
    end
    callback(sessions, {})
  end)
  return true
end

---Load and replay the seeded recoverable Session.
---@param agent_name string
---@param acp_session_id string
---@param options? louiselm.session.Options
---@param ready_callback? fun(session: louiselm.session.Session?, error?: string)
---@return table? session
---@return string? error_message
function Api:load_session(agent_name, acp_session_id, options, ready_callback)
  if self.disposed then
    return nil, "demo API is disposed"
  end
  if agent_name ~= "your-codex-here" or acp_session_id ~= SEEDED_SESSION_ID then
    return nil, "demo Session is not recoverable"
  end
  options = options or {}
  if type(options) ~= "table" then
    return nil, "session options must be a table"
  end
  local session = add_session(self, agent_name, options, "loaded", acp_session_id, "starting")
  self.schedule(120, function()
    if session:inspect().status == "disposed" then
      return
    end
    emit(session, "user_chunk", { content = { type = "text", text = self.copy.seeded_user_first } })
    emit(session, "chunk", {
      content = {
        type = "text",
        text = self.copy.label .. "\n\n" .. self.copy.seeded_reply_first,
      },
    })
    emit(session, "user_chunk", { content = { type = "text", text = self.copy.seeded_user_second } })
    emit(session, "chunk", {
      content = { type = "text", text = self.copy.label .. "\n\n" .. self.copy.seeded_reply_second },
    })
    set_status(session, "ready")
    if ready_callback ~= nil then
      ready_callback(session)
    end
  end)
  return session
end

---Look up one live demo Session.
---@param id string
---@return table? session
function Api:get_session(id)
  return self.sessions[id]
end

---Return live demo Session ids in creation order.
---@return string[] ids
function Api:list_sessions()
  return nvim.deepcopy(self.order)
end

---Return the placeholder Agent's currently unobserved limits state.
---@param agent_name string
---@return table? state
---@return string? error_message
function Api:inspect_agent_limits(agent_name)
  if not AGENTS[agent_name] then
    return nil, "unknown demo Agent '" .. tostring(agent_name) .. "'"
  end
  return nvim.deepcopy(self.limits[agent_name] or { agent = agent_name, status = "not_observed" })
end

---Asynchronously report the placeholder Agent's limits state.
---@param agent_name string
---@param callback fun(state: table, error?: string)
---@return boolean started
---@return string? error_message
function Api:refresh_agent_limits(agent_name, callback)
  local state, state_error = self:inspect_agent_limits(agent_name)
  if state == nil then
    return false, state_error
  end
  if type(callback) ~= "function" then
    return false, "limits callback must be a function"
  end
  self.schedule(0, function()
    if not self.disposed then
      callback(state)
    end
  end)
  return true
end

---Subscribe to placeholder Agent limits changes.
---@param callback fun(state: table)
---@return fun()? unsubscribe
---@return string? error_message
function Api:on_agent_limits(callback)
  if type(callback) ~= "function" then
    return nil, "limits callback must be a function"
  end
  local active = true
  self.limit_listeners[callback] = true
  return function()
    if active then
      active = false
      self.limit_listeners[callback] = nil
    end
  end
end

---Publish a deterministic reached-limit snapshot for the lifecycle lesson.
---@param agent_name string
---@return boolean started
---@return string? error_message
function Api:simulate_limit(agent_name)
  if not AGENTS[agent_name] then
    return false, "unknown demo Agent '" .. tostring(agent_name) .. "'"
  end
  local now = os.time()
  local state = {
    agent = agent_name,
    status = "fresh",
    updated_at = now,
    snapshot = {
      default_bucket_id = "scripted",
      buckets = {
        {
          id = "scripted",
          label = "Scripted quota",
          reached_type = "rate_limit",
          windows = { { used_percent = 100, duration_mins = 300, resets_at = now + 15 * 60 } },
        },
      },
    },
  }
  self.limits[agent_name] = state
  self.schedule(40, function()
    if self.disposed then
      return
    end
    for listener in pairs(self.limit_listeners) do
      listener(nvim.deepcopy(state))
    end
  end)
  return true
end

---Return no remembered permissions; the demo never persists choices.
---@return table[] rules
function Api:list_permissions()
  return {}
end

---Select the language used by future scripted demo transcript content.
---@param language "en"|"zh-CN"
---@return boolean selected
---@return string? error_message
function Api:set_language(language)
  if COPY[language] == nil then
    return false, "unknown demo language '" .. tostring(language) .. "'"
  end
  self.language = language
  self.copy = COPY[language]
  return true
end

---Reject permission revocation because the demo persists no choices.
---@return boolean revoked
---@return string error_message
function Api:revoke_permission()
  return false, "demo stores no permissions"
end

---Dispose all browser-local Sessions.
---@return boolean disposed
function Api:dispose()
  if self.disposed then
    return true
  end
  self.disposed = true
  local ids = nvim.deepcopy(self.order)
  for _, id in ipairs(ids) do
    local session = self.sessions[id]
    if session ~= nil then
      session:dispose()
    end
  end
  self.limit_listeners = {}
  return true
end

---Create the browser-local demo API. It never starts a process or Provider connection.
---@param options table `{ project_root, schedule?, language? }`.
---@return table? api
---@return string? error_message
function M.new(options)
  if type(options) ~= "table" then
    return nil, "demo options must be a table"
  end
  for key in pairs(options) do
    if key ~= "project_root" and key ~= "schedule" and key ~= "language" then
      return nil, "unknown demo option '" .. tostring(key) .. "'"
    end
  end
  if type(options.project_root) ~= "string" or options.project_root == "" then
    return nil, "demo project_root must be a non-empty string"
  end
  if options.schedule ~= nil and type(options.schedule) ~= "function" then
    return nil, "demo schedule must be a function"
  end
  local language = options.language or "en"
  if COPY[language] == nil then
    return nil, "unknown demo language '" .. tostring(language) .. "'"
  end
  local project_root = nvim.fs.normalize(nvim.fn.fnamemodify(options.project_root, ":p"))
  local api = setmetatable({
    project_root = project_root,
    language = language,
    copy = COPY[language],
    schedule = options.schedule or function(_, callback)
      nvim.schedule(callback)
    end,
    sessions = {},
    order = {},
    next_id = 0,
    limit_listeners = {},
    limits = {},
    disposed = false,
  }, Api)
  return api, nil
end

---@param message string?
local function report_error(message)
  if message ~= nil then
    nvim.notify("louiselm demo: " .. message, nvim.log.levels.ERROR)
  end
end

---@param api table
---@param chat louiselm.ui.Chat
local function register_commands(api, chat)
  nvim.api.nvim_create_user_command("LouiselmDemoLanguage", function(command)
    local _, language_error = api:set_language(command.args)
    report_error(language_error)
  end, { desc = "Select scripted demo transcript language", force = true, nargs = 1 })
  nvim.api.nvim_create_user_command("LouiselmSessionNew", function()
    local _, session_error = chat:new_session()
    report_error(session_error)
  end, { desc = "Create a separate louiselm demo Session", force = true })
  nvim.api.nvim_create_user_command("LouiselmResume", function()
    local _, resume_error = chat:resume_session()
    report_error(resume_error)
  end, { desc = "Resume the seeded louiselm demo Session", force = true })
  nvim.api.nvim_create_user_command("LouiselmSessionSwitch", function()
    local _, switch_error = chat:switch_session()
    report_error(switch_error)
  end, { desc = "Switch between louiselm demo Sessions", force = true })
  nvim.api.nvim_create_user_command("LouiselmHandOff", function()
    local _, handoff_error = chat:hand_off()
    report_error(handoff_error)
  end, { desc = "Open a reviewed louiselm demo Handoff", force = true })
  nvim.api.nvim_create_user_command("LouiselmLimits", function()
    local _, limits_error = chat:show_limits()
    report_error(limits_error)
  end, { desc = "Inspect simulated Agent limits", force = true })
  nvim.api.nvim_create_user_command("LouiselmDemoLimit", function()
    local _, limit_error = api:simulate_limit("your-codex-here")
    report_error(limit_error)
  end, { desc = "Trigger the scripted Agent limit", force = true })
  nvim.api.nvim_create_user_command("LouiselmSessionId", function()
    local id, id_error = chat:session_id()
    if id ~= nil then
      nvim.notify(id, nvim.log.levels.INFO)
    else
      report_error(id_error)
    end
  end, { desc = "Show the current demo Session id", force = true })
end

---Open LouiseLM's real chat UI on a disposable browser-local Session.
---@param options table `{ project_root, language? }`.
---@return table? runtime
---@return string? error_message
function M.start(options)
  local api, api_error = M.new(options)
  if api == nil then
    return nil, api_error
  end
  local calculator_path = api.project_root .. "/lua/calculator.lua"
  nvim.cmd.edit(nvim.fn.fnameescape(calculator_path))
  local Chat = require("louiselm.ui.chat")
  local chat, chat_error = Chat.new(api, {
    agents = { "your-codex-here", "your-claude-here" },
    markdown_highlighting = false,
    start_insert_on_switch = false,
    initial_contexts = {
      { label = "buffer: " .. calculator_path, text = "Current buffer: " .. calculator_path },
    },
  })
  if chat == nil then
    api:dispose()
    return nil, chat_error
  end
  -- The browser has no local Attention socket; remove its activity observer before
  -- attaching the first buffer so ordinary cursor movement never attempts native I/O.
  chat.attention:dispose()
  local session, session_error =
    chat:new_session("your-codex-here", { cwd = api.project_root, name = "Fix calculator" })
  if session == nil then
    chat:dispose()
    return nil, session_error
  end
  register_commands(api, chat)
  return { api = api, chat = chat, session = session }
end

return M
