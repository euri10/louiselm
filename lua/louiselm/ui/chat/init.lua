---@class louiselm.ui.ChatOptions
---@field agents? string[] Agent names shown by the new-session picker.

---@class louiselm.ui.ChatView
---@field session louiselm.session.Session Attached session.
---@field buffer integer Scratch buffer for the session.
---@field prompt_line integer Zero-based prompt line.
---@field response_line integer? Zero-based first streamed response line.
---@field response_tail integer? Zero-based last streamed response line.
---@field unsubscribe fun() Session event listener removal function.

---@class louiselm.ui.Chat
---@field api louiselm.session.Api Session API used to create sessions.
---@field agents string[] Agent names for the picker.
---@field views table<string, louiselm.ui.ChatView> Views by local session id.
---@field current_id string? Currently displayed session id.
---@field disposed boolean Whether the chat UI has been disposed.
---@field attach fun(self: louiselm.ui.Chat, session: louiselm.session.Session): boolean, string? Attach or focus a session.
---@field buffer fun(self: louiselm.ui.Chat, session_id?: string): integer? Return a session buffer.
---@field switch fun(self: louiselm.ui.Chat, session_id: string): boolean, string? Focus an attached session.
---@field submit fun(self: louiselm.ui.Chat, text?: string): string|number?, string? Submit the current prompt.
---@field new_session fun(self: louiselm.ui.Chat, agent_name?: string, options?: louiselm.session.Options): louiselm.session.Session?, string? Create a session, using the picker when needed.
---@field dispose fun(self: louiselm.ui.Chat): boolean Dispose buffers and listeners.

local M = {}
local Chat = {}
Chat.__index = Chat

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

---@param value unknown
---@return string[]? agents
---@return string? error_message
local function copy_agents(value)
  if value == nil then
    return {}
  end
  if type(value) ~= "table" then
    return nil, "chat agents must be a string[]"
  end
  local agents = {}
  for index = 1, #value do
    if type(value[index]) ~= "string" or value[index] == "" then
      return nil, "chat agents must be a string[]"
    end
    agents[index] = value[index]
  end
  for key in pairs(value) do
    if type(key) ~= "number" or key < 1 or key > #value or key % 1 ~= 0 then
      return nil, "chat agents must be a dense string[]"
    end
  end
  return agents
end

---@param value string
---@return string[] lines Split text while preserving a trailing empty line.
local function split_lines(value)
  local lines = {}
  local start = 1
  while true do
    local newline = string.find(value, "\n", start, true)
    if newline == nil then
      lines[#lines + 1] = string.sub(value, start)
      return lines
    end
    lines[#lines + 1] = string.sub(value, start, newline - 1)
    start = newline + 1
  end
end

---@param value unknown
---@return string? text Text carried by an ACP chunk.
local function chunk_text(value)
  if type(value) ~= "table" then
    return nil
  end
  if type(value.text) == "string" then
    return value.text
  end
  if type(value.content) == "table" and type(value.content.text) == "string" then
    return value.content.text
  end
  return nil
end

---@param value unknown
---@return string
local function tool_id(value)
  if type(value) == "table" then
    if type(value.toolCallId) == "string" and value.toolCallId ~= "" then
      return value.toolCallId
    end
    if type(value.tool_call_id) == "string" and value.tool_call_id ~= "" then
      return value.tool_call_id
    end
  end
  return "unknown"
end

---@param value unknown
---@return string? text
local function field(value, name)
  if type(value) == "table" and type(value[name]) == "string" and value[name] ~= "" then
    return value[name]
  end
  return nil
end

---@param buffer integer
---@param line integer
---@param value string
local function set_line(buffer, line, value)
  nvim.api.nvim_buf_set_lines(buffer, line, line + 1, false, { value })
end

---@param self louiselm.ui.Chat
---@param view louiselm.ui.ChatView
---@param lines string[]
local function insert_before_prompt(self, view, lines)
  nvim.api.nvim_buf_set_lines(view.buffer, view.prompt_line, view.prompt_line, false, lines)
  view.prompt_line = view.prompt_line + #lines
end

---@param self louiselm.ui.Chat
---@param view louiselm.ui.ChatView
---@param event louiselm.session.Event
local function handle_event(self, view, event)
  if self.disposed or self.views[event.session_id] ~= view then
    return
  end
  if not nvim.api.nvim_buf_is_valid(view.buffer) then
    return
  end

  if event.type == "chunk" then
    local text = chunk_text(event.data)
    if text == nil then
      return
    end
    if view.response_tail == nil then
      insert_before_prompt(self, view, { "" })
      view.response_line = view.prompt_line - 1
      view.response_tail = view.response_line
    end
    local current = nvim.api.nvim_buf_get_lines(view.buffer, view.response_tail, view.response_tail + 1, false)[1] or ""
    local lines = split_lines(current .. text)
    nvim.api.nvim_buf_set_lines(view.buffer, view.response_tail, view.response_tail + 1, false, lines)
    local added = #lines - 1
    view.response_tail = view.response_tail + added
    view.prompt_line = view.prompt_line + added
  elseif event.type == "tool_call_started" or event.type == "tool_call_finished" then
    local status = field(event.data, "status")
    local suffix = event.type == "tool_call_started" and "started" or "finished"
    local title = field(event.data, "title")
    local detail = tool_id(event.data)
    if event.type == "tool_call_started" and title ~= nil then
      detail = detail .. ": " .. title
    end
    if event.type == "tool_call_finished" and status ~= nil then
      detail = detail .. " (" .. status .. ")"
    end
    insert_before_prompt(self, view, { "[tool " .. suffix .. "] " .. detail })
  elseif event.type == "error" then
    local message = field(event.data, "message") or "unknown session error"
    insert_before_prompt(self, view, { "Error: " .. message })
  elseif event.type == "turn_done" then
    view.response_line = nil
    view.response_tail = nil
  end
end

---Create a chat UI controller without creating buffers or mappings.
---@param api louiselm.session.Api Headless session API.
---@param options? louiselm.ui.ChatOptions Agent picker options.
---@return louiselm.ui.Chat? chat
---@return string? error_message
function M.new(api, options)
  if type(api) ~= "table" then
    return nil, "chat requires a session API"
  end
  if options ~= nil and type(options) ~= "table" then
    return nil, "chat options must be a table"
  end
  if options ~= nil then
    for key in pairs(options) do
      if key ~= "agents" then
        return nil, "unknown chat option '" .. tostring(key) .. "'"
      end
    end
  end
  local agents, agents_error = copy_agents(options and options.agents)
  if agents == nil then
    return nil, agents_error
  end
  local chat = setmetatable({ api = api, agents = agents, views = {}, current_id = nil, disposed = false }, Chat)
  return chat, nil
end

---Attach a session to a scratch markdown buffer and focus it.
---@param self louiselm.ui.Chat
---@param session louiselm.session.Session Session to display.
---@return boolean attached
---@return string? error_message Validation or buffer creation error.
function Chat:attach(session)
  if self.disposed then
    return false, "chat UI is disposed"
  end
  if type(session) ~= "table" or type(session.inspect) ~= "function" or type(session.on) ~= "function" then
    return false, "chat requires a session"
  end
  local state = session:inspect()
  if type(state) ~= "table" or type(state.id) ~= "string" or type(state.agent) ~= "string" then
    return false, "session has invalid state"
  end
  local existing = self.views[state.id]
  if existing ~= nil then
    return self:switch(state.id)
  end

  local buffer = nvim.api.nvim_create_buf(false, true)
  nvim.api.nvim_buf_set_name(buffer, "louiselm://" .. state.id)
  nvim.api.nvim_set_option_value("buftype", "nofile", { buf = buffer })
  nvim.api.nvim_set_option_value("bufhidden", "hide", { buf = buffer })
  nvim.api.nvim_set_option_value("swapfile", false, { buf = buffer })
  nvim.api.nvim_set_option_value("filetype", "markdown", { buf = buffer })
  nvim.api.nvim_buf_set_lines(buffer, 0, -1, false, { "# " .. state.agent .. " · " .. state.id, "", "> " })

  local view = {
    session = session,
    buffer = buffer,
    prompt_line = 2,
    response_line = nil,
    response_tail = nil,
    unsubscribe = function() end,
  }
  view.unsubscribe = session:on(function(event)
    -- ACP stdout callbacks run in a fast event; buffer APIs must run later.
    nvim.schedule(function()
      handle_event(self, view, event)
    end)
  end)
  self.views[state.id] = view
  self.current_id = state.id
  nvim.api.nvim_set_current_buf(buffer)
  nvim.keymap.set("i", "<CR>", function()
    self:submit()
  end, { buffer = buffer, silent = true, desc = "Submit louiselm prompt" })
  return true
end

---Return the buffer for a session, or the current chat buffer.
---@param self louiselm.ui.Chat
---@param session_id? string Session id; defaults to the current session.
---@return integer? buffer
function Chat:buffer(session_id)
  local id = session_id or self.current_id
  local view = id and self.views[id]
  return view and view.buffer or nil
end

---Switch focus to an attached session buffer.
---@param self louiselm.ui.Chat
---@param session_id string Session id.
---@return boolean switched
---@return string? error_message
function Chat:switch(session_id)
  if self.disposed then
    return false, "chat UI is disposed"
  end
  local view = self.views[session_id]
  if view == nil then
    return false, "session is not attached"
  end
  if not nvim.api.nvim_buf_is_valid(view.buffer) then
    return false, "session buffer is invalid"
  end
  self.current_id = session_id
  nvim.api.nvim_set_current_buf(view.buffer)
  return true
end

---Submit text to the current session; slash commands are passed through unchanged.
---@param self louiselm.ui.Chat
---@param text? string Prompt text; defaults to the current buffer prompt line.
---@return string|number? request_id ACP request id, or nil on failure.
---@return string? error_message Validation or session error.
function Chat:submit(text)
  if self.disposed then
    return nil, "chat UI is disposed"
  end
  local view = self.current_id and self.views[self.current_id]
  if view == nil then
    return nil, "no chat session is attached"
  end
  if text == nil then
    local line = nvim.api.nvim_buf_get_lines(view.buffer, view.prompt_line, view.prompt_line + 1, false)[1]
    text = line and (string.sub(line, 1, 2) == "> " and string.sub(line, 3) or line) or ""
  end
  if type(text) ~= "string" or text == "" then
    return nil, "prompt must be a non-empty string"
  end

  set_line(view.buffer, view.prompt_line, "> " .. text)
  nvim.api.nvim_buf_set_lines(view.buffer, view.prompt_line + 1, view.prompt_line + 1, false, { "", "> " })
  view.response_line = view.prompt_line + 1
  view.response_tail = view.response_line
  view.prompt_line = view.prompt_line + 2
  if nvim.api.nvim_get_current_buf() == view.buffer then
    nvim.api.nvim_win_set_cursor(0, { view.prompt_line + 1, 2 })
  end

  local request_id, prompt_error = view.session:prompt(text)
  if request_id == nil then
    insert_before_prompt(self, view, { "Error: " .. (prompt_error or "prompt failed") })
    view.response_line = nil
    view.response_tail = nil
    return nil, prompt_error or "prompt failed"
  end
  return request_id
end

---Create a session and attach it; select an agent when no name is supplied.
---@param self louiselm.ui.Chat
---@param agent_name? string Configured agent name.
---@param options? louiselm.session.Options Session creation options.
---@return louiselm.session.Session? session Created session, or nil while the picker is open.
---@return string? error_message Validation or creation error.
function Chat:new_session(agent_name, options)
  if self.disposed then
    return nil, "chat UI is disposed"
  end
  if agent_name == nil then
    if #self.agents == 0 then
      return nil, "no chat agents configured"
    end
    if #self.agents > 1 then
      nvim.ui.select(self.agents, { prompt = "louiselm agent: " }, function(choice)
        if choice ~= nil then
          self:new_session(choice, options)
        end
      end)
      return nil
    end
    agent_name = self.agents[1]
  end
  if type(agent_name) ~= "string" or agent_name == "" then
    return nil, "agent name must be a non-empty string"
  end
  local session, session_error = self.api:create_session(agent_name, options)
  if session == nil then
    return nil, session_error
  end
  local attached, attach_error = self:attach(session)
  if not attached then
    return nil, attach_error
  end
  return session
end

---Remove chat buffers and event listeners without disposing the sessions.
---@param self louiselm.ui.Chat
---@return boolean disposed
function Chat:dispose()
  if self.disposed then
    return true
  end
  self.disposed = true
  for id, view in pairs(self.views) do
    view.unsubscribe()
    if nvim.api.nvim_buf_is_valid(view.buffer) then
      nvim.api.nvim_buf_delete(view.buffer, { force = true })
    end
    self.views[id] = nil
  end
  self.current_id = nil
  return true
end

return M
