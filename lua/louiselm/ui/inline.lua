---@class louiselm.ui.InlineOptions
---@field agents? string[] Agent names shown when starting a session.
---@field cwd? string Working directory for newly created sessions.

---@class louiselm.ui.Inline
---@field api louiselm.session.Api Headless session API.
---@field agents string[] Configured agent names.
---@field cwd string? Working directory for new sessions.
---@field session louiselm.session.Session? Session used by the assistant.
---@field unsubscribe fun()? Session listener removal function.
---@field disposed boolean Whether the controller has been disposed.
---@field running boolean Whether a prompt is active.
---@field buffer integer? Buffer being edited.
---@field start_row integer? Zero-based replacement start row.
---@field start_col integer? Zero-based replacement start column.
---@field end_row integer? Zero-based replacement end row.
---@field end_col integer? Zero-based replacement end column.
---@field response string Accumulated response text.
---@field new fun(api: louiselm.session.Api, options?: louiselm.ui.InlineOptions): louiselm.ui.Inline?, string?
---@field run fun(self: louiselm.ui.Inline, prompt: string, agent_name?: string): string|number?, string? Start the prompt; returns the session id while startup is pending.
---@field dispose fun(self: louiselm.ui.Inline): boolean

local M = {}
local Inline = {}
Inline.__index = Inline

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

---@param value unknown
---@return string[]? agents
---@return string? error_message
local function copy_agents(value)
  if value == nil then
    return {}, nil
  end
  if type(value) ~= "table" then
    return nil, "inline agents must be a string[]"
  end
  local agents = {}
  for index, agent in ipairs(value) do
    if type(agent) ~= "string" or agent == "" then
      return nil, "inline agents must be a string[]"
    end
    agents[index] = agent
  end
  if #agents == 0 then
    return nil, "inline agents must not be empty"
  end
  return agents, nil
end

---@param self louiselm.ui.Inline
---@param event louiselm.session.Event
local function handle_event(self, event)
  if self.disposed or self.session == nil or event.session_id ~= self.session:inspect().id then
    return
  end
  if event.type == "chunk" then
    local data = event.data
    local text = type(data) == "table" and data.text or nil
    if text == nil and type(data) == "table" and type(data.content) == "table" then
      text = data.content.text
    end
    if type(text) ~= "string" or text == "" then
      return
    end
    self.response = self.response .. text
    if self.buffer == nil or not nvim.api.nvim_buf_is_valid(self.buffer) then
      return
    end
    nvim.api.nvim_buf_set_text(
      self.buffer,
      assert(self.start_row),
      assert(self.start_col),
      assert(self.end_row),
      assert(self.end_col),
      nvim.split(self.response, "\n", { plain = true })
    )
    local lines = nvim.split(self.response, "\n", { plain = true })
    self.end_row = assert(self.start_row) + #lines - 1
    self.end_col = #lines == 1 and assert(self.start_col) + #lines[1] or #lines[#lines]
  elseif event.type == "turn_done" or event.type == "error" then
    self.running = false
  end
end

---@param self louiselm.ui.Inline
---@return string? context
---@return string? error_message
local function capture_context(self)
  local buffer = nvim.api.nvim_get_current_buf()
  local start = nvim.api.nvim_buf_get_mark(buffer, "<")
  local finish = nvim.api.nvim_buf_get_mark(buffer, ">")
  local selected = start[1] > 0 and finish[1] > 0
  if selected and (start[1] > finish[1] or (start[1] == finish[1] and start[2] > finish[2])) then
    start, finish = finish, start
  end
  if not selected then
    local cursor = nvim.api.nvim_win_get_cursor(0)
    start = { cursor[1], cursor[2] }
    finish = { cursor[1], cursor[2] }
  end
  local lines = nvim.api.nvim_buf_get_lines(buffer, start[1] - 1, finish[1], false)
  if selected and #lines > 0 then
    if #lines == 1 then
      lines[1] = lines[1]:sub(start[2] + 1, finish[2] + 1)
    else
      lines[1] = lines[1]:sub(start[2] + 1)
      lines[#lines] = lines[#lines]:sub(1, finish[2] + 1)
    end
  end
  self.buffer = buffer
  self.start_row = start[1] - 1
  self.start_col = start[2]
  self.end_row = finish[1] - 1
  self.end_col = finish[2] + (selected and 1 or 0)
  local path = nvim.fs.normalize(nvim.api.nvim_buf_get_name(buffer))
  return "File: " .. (path == "" and "[No Name]" or path) .. "\nSelected text:\n" .. table.concat(lines, "\n"), nil
end

---Create an inline assistant using the headless session API.
---@param api louiselm.session.Api Headless session API.
---@param options? louiselm.ui.InlineOptions Agent and working-directory options.
---@return louiselm.ui.Inline? inline
---@return string? error_message
function M.new(api, options)
  if type(api) ~= "table" then
    return nil, "inline requires a session API"
  end
  if options ~= nil and type(options) ~= "table" then
    return nil, "inline options must be a table"
  end
  local agents, agents_error = copy_agents(options and options.agents)
  if agents == nil then
    return nil, agents_error
  end
  if options and options.cwd ~= nil and type(options.cwd) ~= "string" then
    return nil, "inline cwd must be a string"
  end
  return setmetatable({
    api = api,
    agents = agents,
    cwd = options and options.cwd,
    unsubscribe = nil,
    disposed = false,
    running = false,
    response = "",
  }, Inline),
    nil
end

---Start an inline prompt and replace the current selection with its response.
---@param self louiselm.ui.Inline
---@param prompt string Instruction for the agent.
---@param agent_name? string Configured agent name.
---@return string|number? request_id ACP request id, or nil on failure.
---@return string? error_message Validation, lifecycle, or session error.
function Inline:run(prompt, agent_name)
  if self.disposed then
    return nil, "inline UI is disposed"
  end
  if self.running then
    return nil, "inline prompt is already running"
  end
  if type(prompt) ~= "string" or prompt == "" then
    return nil, "inline prompt must be a non-empty string"
  end
  agent_name = agent_name or self.agents[1]
  if type(agent_name) ~= "string" or agent_name == "" then
    return nil, "no inline agent configured"
  end
  local context, context_error = capture_context(self)
  if context == nil then
    return nil, context_error
  end
  if self.unsubscribe ~= nil then
    self.unsubscribe()
    self.unsubscribe = nil
  end
  if self.session ~= nil then
    self.session:dispose()
    self.session = nil
  end
  self.response = ""
  self.running = true
  local function submit(session)
    nvim.schedule(function()
      if self.disposed or self.session ~= session then
        return
      end
      local request_id, prompt_error = session:prompt({
        { type = "text", text = context },
        { type = "text", text = prompt },
      })
      if request_id == nil then
        self.running = false
        nvim.notify("louiselm: " .. (prompt_error or "inline prompt failed"), nvim.log.levels.ERROR)
      end
    end)
  end
  local session, session_error = self.api:create_session(
    agent_name,
    { cwd = self.cwd },
    function(ready_session, ready_error)
      if ready_session == nil then
        self.running = false
        nvim.notify("louiselm: " .. (ready_error or "inline session failed to start"), nvim.log.levels.ERROR)
        return
      end
      submit(ready_session)
    end
  )
  if session == nil then
    self.running = false
    return nil, session_error
  end
  self.session = session
  self.unsubscribe = session:on(function(event)
    nvim.schedule(function()
      handle_event(self, event)
    end)
  end)
  return session:inspect().id, nil
end

---Dispose the inline controller and its active session.
---@param self louiselm.ui.Inline
---@return boolean disposed Always true.
function Inline:dispose()
  if self.disposed then
    return true
  end
  self.disposed = true
  if self.unsubscribe ~= nil then
    self.unsubscribe()
    self.unsubscribe = nil
  end
  if self.session ~= nil then
    self.session:dispose()
    self.session = nil
  end
  return true
end

return M
