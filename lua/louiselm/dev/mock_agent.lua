---@class louiselm.dev.MockAgentOptions
---@field mode? "echo"|"static"|"permission"|"crash" Response behavior. Defaults to echo.
---@field response? string Static response text.
---@field crash_on? "initialize"|"session/new"|"session/load"|"session/prompt" Crash before handling a request.
---@field fail_session_load? boolean Respond to `session/load` with an ACP `RequestError.resourceNotFound`
---(-32002) instead of succeeding, while `initialize` still advertises `loadSession: true`. Models an Agent
---that advertises resume support but no longer has the requested session (evicted, expired, reinstalled).
---@field replay_user_message? string User text replayed as a `user_message_chunk` notification before the `session/load` response, modeling an agent launched with history replay.
---@field replay_reasoning? string Reasoning text replayed as an `agent_thought_chunk` notification before the `session/load` response, modeling an agent whose session history includes thinking blocks.
---@field available_commands? table[] Commands advertised via `available_commands_update` right after the session is created or loaded.

---@class louiselm.dev.MockAgentState
---@field initialized boolean
---@field next_session integer
---@field next_permission integer
---@field sessions table<string, { cwd: string }>
---@field pending_prompt? { id: string|number, session_id: string, text: string }
---@field pending_permission? string|number

local M = {}

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local function write_message(message)
  io.stdout:write(nvim.json.encode(message) .. "\n")
  io.stdout:flush()
end

---@param id string|number|nil
---@param result unknown
local function write_response(id, result)
  write_message({ jsonrpc = "2.0", id = id, result = result })
end

---@param id string|number|nil
---@param code integer
---@param message string
local function write_error(id, code, message)
  write_message({ jsonrpc = "2.0", id = id, error = { code = code, message = message } })
end

---@param method string
---@param params table
local function write_notification(method, params)
  write_message({ jsonrpc = "2.0", method = method, params = params })
end

---@param id integer
---@param params table
local function write_permission_request(id, params)
  write_message({ jsonrpc = "2.0", id = id, method = "session/request_permission", params = params })
end

---@param prompt table
---@return string
local function prompt_text(prompt)
  local first = prompt[1]
  if type(first) == "table" and type(first.text) == "string" then
    return first.text
  end
  return "mock response"
end

---@param state louiselm.dev.MockAgentState
---@param prompt { id: string|number, session_id: string, text: string }
---@param response string
local function finish_prompt(state, prompt, response)
  write_notification("session/update", {
    sessionId = prompt.session_id,
    update = {
      sessionUpdate = "agent_message_chunk",
      content = { type = "text", text = response },
    },
  })
  write_notification("session/update", {
    sessionId = prompt.session_id,
    update = { sessionUpdate = "turn_done", stopReason = "end_turn" },
  })
  write_response(prompt.id, { stopReason = "end_turn" })
  state.pending_prompt = nil
end

---@param state louiselm.dev.MockAgentState
local function finish_cancelled_prompt(state)
  local prompt = state.pending_prompt
  if prompt == nil then
    return
  end
  write_notification("session/update", {
    sessionId = prompt.session_id,
    update = { sessionUpdate = "turn_done", stopReason = "cancelled" },
  })
  write_response(prompt.id, { stopReason = "cancelled" })
  state.pending_prompt = nil
end

---@param options louiselm.dev.MockAgentOptions
---@param method string
local function should_crash(options, method)
  if options.mode == "crash" and method == "session/prompt" then
    return true
  end
  return options.crash_on == method
end

---@param options louiselm.dev.MockAgentOptions
---@return string response
local function configured_response(options)
  if options.mode == "static" or options.mode == "permission" then
    return options.response or "mock response"
  end
  return ""
end

---@param message table
---@param state louiselm.dev.MockAgentState
---@param options louiselm.dev.MockAgentOptions
---@return boolean continue_loop
local function handle_message(message, state, options)
  if message.method == nil then
    if state.pending_permission ~= nil and message.id == state.pending_permission then
      state.pending_permission = nil
      local prompt = state.pending_prompt
      if prompt ~= nil then
        local response = configured_response(options)
        finish_prompt(state, prompt, response ~= "" and response or prompt.text)
      end
    end
    return true
  end

  local method = message.method
  if type(method) ~= "string" or method == "" then
    write_error(message.id, -32600, "method must be a non-empty string")
    return true
  end
  if should_crash(options, method) then
    os.exit(23)
  end

  if method == "initialize" then
    state.initialized = true
    write_response(message.id, {
      protocolVersion = 1,
      agentCapabilities = { loadSession = true, sessionCapabilities = { list = {} } },
    })
    return true
  end
  if not state.initialized then
    write_error(message.id, -32000, "mock agent is not initialized")
    return true
  end

  local params = message.params
  if type(params) ~= "table" then
    write_error(message.id, -32602, "params must be an object")
    return true
  end

  if method == "session/new" then
    local session_id = "mock-session-" .. state.next_session
    state.next_session = state.next_session + 1
    state.sessions[session_id] = { cwd = type(params.cwd) == "string" and params.cwd or nvim.fn.getcwd() }
    write_response(message.id, { sessionId = session_id })
    if options.available_commands ~= nil then
      write_notification("session/update", {
        sessionId = session_id,
        update = { sessionUpdate = "available_commands_update", availableCommands = options.available_commands },
      })
    end
    return true
  end

  if method == "session/load" then
    if type(params.sessionId) ~= "string" or params.sessionId == "" then
      write_error(message.id, -32602, "sessionId must be a non-empty string")
      return true
    end
    if options.fail_session_load then
      write_error(message.id, -32002, "Resource not found: " .. params.sessionId)
      return true
    end
    state.sessions[params.sessionId] = { cwd = type(params.cwd) == "string" and params.cwd or nvim.fn.getcwd() }
    if options.replay_user_message ~= nil then
      write_notification("session/update", {
        sessionId = params.sessionId,
        update = {
          sessionUpdate = "user_message_chunk",
          content = { type = "text", text = options.replay_user_message },
        },
      })
    end
    if options.replay_reasoning ~= nil then
      write_notification("session/update", {
        sessionId = params.sessionId,
        update = {
          sessionUpdate = "agent_thought_chunk",
          content = { type = "text", text = options.replay_reasoning },
        },
      })
    end
    write_response(message.id, { sessionId = params.sessionId })
    if options.available_commands ~= nil then
      write_notification("session/update", {
        sessionId = params.sessionId,
        update = { sessionUpdate = "available_commands_update", availableCommands = options.available_commands },
      })
    end
    return true
  end

  if method == "session/list" then
    if type(params) ~= "table" or nvim.islist(params) then
      write_error(message.id, -32602, "Invalid params")
      return true
    end
    local sessions = {}
    local ids = {}
    for session_id in pairs(state.sessions) do
      ids[#ids + 1] = session_id
    end
    table.sort(ids)
    for _, session_id in ipairs(ids) do
      local session = state.sessions[session_id]
      if params.cwd == nil or params.cwd == session.cwd then
        sessions[#sessions + 1] = { sessionId = session_id, cwd = session.cwd, title = "Mock " .. session_id }
      end
    end
    write_response(message.id, { sessions = sessions })
    return true
  end

  if method == "session/prompt" then
    if type(params.sessionId) ~= "string" or not state.sessions[params.sessionId] then
      write_error(message.id, -32602, "unknown sessionId")
      return true
    end
    if type(params.prompt) ~= "table" then
      write_error(message.id, -32602, "prompt must be an array")
      return true
    end
    if state.pending_prompt ~= nil then
      write_error(message.id, -32000, "mock agent already has an active prompt")
      return true
    end
    local prompt = {
      id = message.id,
      session_id = params.sessionId,
      text = prompt_text(params.prompt),
    }
    state.pending_prompt = prompt
    if options.mode == "permission" then
      local permission_id = 1000 + state.next_permission
      state.next_permission = state.next_permission + 1
      state.pending_permission = permission_id
      write_permission_request(permission_id, {
        sessionId = prompt.session_id,
        toolCall = {
          kind = "edit",
          rawInput = { path = "mock.txt", diff = "-old\n+new" },
        },
        options = {
          { optionId = "allow-once", kind = "allow_once" },
          { optionId = "reject", kind = "reject" },
        },
      })
    else
      local response = configured_response(options)
      finish_prompt(state, prompt, response ~= "" and response or prompt.text)
    end
    return true
  end

  if method == "session/cancel" then
    finish_cancelled_prompt(state)
    return true
  end

  write_error(message.id, -32601, "method not found")
  return true
end

---Run the mock ACP agent until stdin closes or the process is terminated.
---@param options? louiselm.dev.MockAgentOptions Behavior overrides, primarily for direct development use.
---@return nil
function M.run(options)
  options = options or {}
  local mode = options.mode or nvim.env.LOUISELM_MOCK_MODE or "echo"
  local response = options.response
  if response == nil then
    response = nvim.env.LOUISELM_MOCK_RESPONSE
  end
  local crash_on = options.crash_on or nvim.env.LOUISELM_MOCK_CRASH_ON
  local fail_session_load = options.fail_session_load
  if fail_session_load == nil then
    fail_session_load = nvim.env.LOUISELM_MOCK_FAIL_SESSION_LOAD ~= nil
  end
  local replay_user_message = options.replay_user_message or nvim.env.LOUISELM_MOCK_REPLAY_USER
  local replay_reasoning = options.replay_reasoning or nvim.env.LOUISELM_MOCK_REPLAY_REASONING
  local available_commands = options.available_commands
  if available_commands == nil and nvim.env.LOUISELM_MOCK_AVAILABLE_COMMANDS ~= nil then
    local decode_ok, decoded = pcall(nvim.json.decode, nvim.env.LOUISELM_MOCK_AVAILABLE_COMMANDS)
    if decode_ok then
      available_commands = decoded
    end
  end
  local configured = {
    mode = mode,
    response = response,
    crash_on = crash_on,
    fail_session_load = fail_session_load,
    replay_user_message = replay_user_message,
    replay_reasoning = replay_reasoning,
    available_commands = available_commands,
  }
  local state = { initialized = false, next_session = 1, next_permission = 1, sessions = {} }

  while true do
    local line = io.read("*l")
    if line == nil then
      return
    end
    local call_ok, message = pcall(nvim.json.decode, line)
    if not call_ok or type(message) ~= "table" then
      write_error(nil, -32700, "invalid JSON")
    else
      handle_message(message, state, configured)
    end
  end
end

return M
