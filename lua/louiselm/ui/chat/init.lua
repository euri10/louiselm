local Context = require("louiselm.ui.context")
local Diff = require("louiselm.ui.diff")

---@class louiselm.ui.ChatOptions
---@field agents? string[] Agent names shown by the new-session picker.
---@field skills? louiselm.skills.Skill[] Skills shown by the invocation picker.
---@field initial_contexts? louiselm.ui.ContextItem[] Context queued for every new session.

---@class louiselm.ui.ChatView
---@field session louiselm.session.Session Attached session.
---@field buffer integer Scratch buffer for the session.
---@field source_buffer integer Buffer that was current when the chat view was attached.
---@field prompt_line integer Zero-based prompt line.
---@field response_line integer? Zero-based first streamed response line.
---@field response_tail integer? Zero-based last streamed response line.
---@field contexts louiselm.ui.ContextItem[] Context items queued for the next prompt.
---@field context_prefix string Visible context markers prefixed to the prompt.
---@field unsubscribe fun() Session event listener removal function.

---@class louiselm.ui.Chat
---@field api louiselm.session.Api Session API used to create sessions.
---@field agents string[] Agent names for the picker.
---@field skills louiselm.skills.Skill[] Skills for the invocation picker.
---@field initial_contexts louiselm.ui.ContextItem[] Context queued for every new session.
---@field diff louiselm.ui.Diff File-edit review UI.
---@field views table<string, louiselm.ui.ChatView> Views by local session id.
---@field current_id string? Currently displayed session id.
---@field disposed boolean Whether the chat UI has been disposed.
---@field attach fun(self: louiselm.ui.Chat, session: louiselm.session.Session): boolean, string? Attach or focus a session.
---@field buffer fun(self: louiselm.ui.Chat, session_id?: string): integer? Return a session buffer.
---@field switch fun(self: louiselm.ui.Chat, session_id: string): boolean, string? Focus an attached session.
---@field submit fun(self: louiselm.ui.Chat, text?: string): string|number?, string? Submit the current prompt.
---@field queue_context fun(self: louiselm.ui.Chat, item: louiselm.ui.ContextItem): boolean, string? Queue context for the next prompt.
---@field mention_buffer fun(self: louiselm.ui.Chat): boolean, string? Queue the source buffer context.
---@field send_selection fun(self: louiselm.ui.Chat): boolean, string? Queue the source visual selection.
---@field pick_file fun(self: louiselm.ui.Chat, root?: string): boolean, string? Pick and queue a file context.
---@field pick_skill fun(self: louiselm.ui.Chat): boolean, string? Pick and queue a skill invocation.
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

---@param value unknown
---@return louiselm.skills.Skill[]? skills
---@return string? error_message
local function copy_skills(value)
  if value == nil then
    return {}
  end
  if type(value) ~= "table" then
    return nil, "chat skills must be a skill[]"
  end
  local skills = {}
  for index, skill in ipairs(value) do
    if
      type(skill) ~= "table"
      or type(skill.name) ~= "string"
      or skill.name == ""
      or type(skill.description) ~= "string"
      or type(skill.path) ~= "string"
    then
      return nil, string.format("chat skill at index %d is malformed", index)
    end
    skills[index] = { name = skill.name, description = skill.description, path = skill.path }
  end
  for key in pairs(value) do
    if type(key) ~= "number" or key < 1 or key > #value or key % 1 ~= 0 then
      return nil, "chat skills must be a dense skill[]"
    end
  end
  return skills
end

---@param value unknown
---@return louiselm.ui.ContextItem[]? contexts
---@return string? error_message
local function copy_initial_contexts(value)
  if value == nil then
    return {}
  end
  if type(value) ~= "table" then
    return nil, "chat initial contexts must be a context[]"
  end
  local contexts = {}
  for index, item in ipairs(value) do
    if type(item) ~= "table" or type(item.label) ~= "string" or type(item.text) ~= "string" then
      return nil, string.format("chat initial context at index %d is malformed", index)
    end
    contexts[index] = { label = item.label, text = item.text }
  end
  for key in pairs(value) do
    if type(key) ~= "number" or key < 1 or key > #value or key % 1 ~= 0 then
      return nil, "chat initial contexts must be a dense context[]"
    end
  end
  return contexts
end

---@param buffer integer
---@param line integer
---@param value string
local function set_line(buffer, line, value)
  nvim.api.nvim_buf_set_lines(buffer, line, line + 1, false, { value })
end

---@param self louiselm.ui.Chat
---@param view louiselm.ui.ChatView
---@param item louiselm.ui.ContextItem
---@return boolean queued
---@return string? error_message
local function queue_context(self, view, item)
  if self.disposed or self.views[view.session:inspect().id] ~= view then
    return false, "chat UI is disposed"
  end
  if type(item) ~= "table" or type(item.label) ~= "string" or type(item.text) ~= "string" then
    return false, "context item must contain label and text strings"
  end
  local line = nvim.api.nvim_buf_get_lines(view.buffer, view.prompt_line, view.prompt_line + 1, false)[1] or "> "
  local text = line:sub(1, 2) == "> " and line:sub(3) or line
  if view.context_prefix ~= "" and text:sub(1, #view.context_prefix) == view.context_prefix then
    text = text:sub(#view.context_prefix + 1)
  end
  view.contexts[#view.contexts + 1] = { label = item.label, text = item.text }
  view.context_prefix = view.context_prefix .. "[context: " .. item.label .. "] "
  set_line(view.buffer, view.prompt_line, "> " .. view.context_prefix .. text)
  return true
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
  elseif event.type == "permission_requested" then
    local data = event.data
    if type(data) == "table" and type(data.operation) == "table" and data.operation.kind == "file_edit" then
      local opened, open_error = self.diff:open(data, event.respond)
      if not opened then
        insert_before_prompt(self, view, { "Error: " .. (open_error or "could not open diff review") })
      end
    end
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
      if key ~= "agents" and key ~= "skills" and key ~= "initial_contexts" then
        return nil, "unknown chat option '" .. tostring(key) .. "'"
      end
    end
  end
  local agents, agents_error = copy_agents(options and options.agents)
  if agents == nil then
    return nil, agents_error
  end
  local skills, skills_error = copy_skills(options and options.skills)
  if skills == nil then
    return nil, skills_error
  end
  local initial_contexts, contexts_error = copy_initial_contexts(options and options.initial_contexts)
  if initial_contexts == nil then
    return nil, contexts_error
  end
  local chat = setmetatable({
    api = api,
    agents = agents,
    skills = skills,
    initial_contexts = initial_contexts,
    diff = Diff.new(),
    views = {},
    current_id = nil,
    disposed = false,
  }, Chat)
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

  local source_buffer = nvim.api.nvim_get_current_buf()
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
    source_buffer = source_buffer,
    prompt_line = 2,
    response_line = nil,
    response_tail = nil,
    contexts = {},
    context_prefix = "",
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
  if type(text) ~= "string" then
    return nil, "prompt must be a non-empty string"
  end
  local context_items = view.contexts
  local context_prefix = view.context_prefix
  if context_prefix ~= "" and text:sub(1, #context_prefix) == context_prefix then
    text = text:sub(#context_prefix + 1)
  end
  if text == "" and #context_items == 0 then
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

  ---@type string|table
  local prompt = text
  if #context_items > 0 then
    prompt = {}
    for _, item in ipairs(context_items) do
      prompt[#prompt + 1] = { type = "text", text = item.text }
    end
    if text ~= "" then
      prompt[#prompt + 1] = { type = "text", text = text }
    end
  end
  local request_id, prompt_error = view.session:prompt(prompt)
  if request_id == nil then
    insert_before_prompt(self, view, { "Error: " .. (prompt_error or "prompt failed") })
    view.response_line = nil
    view.response_tail = nil
    return nil, prompt_error or "prompt failed"
  end
  view.contexts = {}
  view.context_prefix = ""
  return request_id
end

---Queue a context item for the current chat prompt.
---@param self louiselm.ui.Chat
---@param item louiselm.ui.ContextItem Context item.
---@return boolean queued
---@return string? error_message Validation or lifecycle error.
function Chat:queue_context(item)
  if self.disposed then
    return false, "chat UI is disposed"
  end
  local view = self.current_id and self.views[self.current_id]
  if view == nil then
    return false, "no chat session is attached"
  end
  return queue_context(self, view, item)
end

---Queue the buffer that was current when the active chat view was attached.
---@param self louiselm.ui.Chat
---@return boolean queued
---@return string? error_message Context or lifecycle error.
function Chat:mention_buffer()
  local view = self.current_id and self.views[self.current_id]
  if view == nil then
    return false, "no chat session is attached"
  end
  return queue_context(self, view, Context.buffer(view.source_buffer))
end

---Queue the visual selection from the source buffer of the active chat view.
---@param self louiselm.ui.Chat
---@return boolean queued
---@return string? error_message Context or selection error.
function Chat:send_selection()
  local view = self.current_id and self.views[self.current_id]
  if view == nil then
    return false, "no chat session is attached"
  end
  local item, selection_error = Context.selection(view.source_buffer)
  if item == nil then
    return false, selection_error
  end
  return queue_context(self, view, item)
end

---Pick a file and queue its path for the active chat prompt.
---@param self louiselm.ui.Chat
---@param root? string Directory to scan.
---@return boolean started
---@return string? error_message Picker or lifecycle error.
function Chat:pick_file(root)
  if self.disposed then
    return false, "chat UI is disposed"
  end
  local started, pick_error = Context.files.pick(root, function(path, error_message)
    if path == nil then
      return
    end
    local item = assert(Context.files.context(path))
    self:queue_context(item)
  end)
  return started, pick_error
end

---Pick a skill and queue its slash invocation for the active chat prompt.
---@param self louiselm.ui.Chat
---@return boolean started
---@return string? error_message Picker or lifecycle error.
function Chat:pick_skill()
  if self.disposed then
    return false, "chat UI is disposed"
  end
  if #self.skills == 0 then
    return false, "no chat skills configured"
  end
  return Context.skills.pick(self.skills, function(skill)
    if skill ~= nil then
      self:queue_context(Context.skills.context(skill))
    end
  end)
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
  for _, item in ipairs(self.initial_contexts) do
    local queued, queue_error = self:queue_context(item)
    if not queued then
      return nil, queue_error
    end
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
  self.diff:dispose()
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
