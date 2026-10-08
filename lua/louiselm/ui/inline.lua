---@class louiselm.ui.InlineOptions
---@field agents? string[] Agent names shown when starting a session.
---@field cwd? string Working directory for newly created sessions.
---@field decisions? louiselm.ui.Decisions Shared permission owner; the caller retains its lifecycle.

---@class louiselm.ui.Inline
---@field api louiselm.session.Api Headless session API.
---@field agents string[] Configured agent names.
---@field cwd string? Working directory for new sessions.
---@field session louiselm.session.Session? Session used by the assistant.
---@field unsubscribe fun()? Session listener removal function.
---@field disposed boolean Whether the controller has been disposed.
---@field running boolean Whether a prompt is active.
---@field cancelled boolean Whether further output from the current turn must be ignored.
---@field decisions louiselm.ui.Decisions Permission presentation and responders.
---@field owns_decisions boolean Whether Disposal owns the permission controller.
---@field buffer integer? Buffer being edited.
---@field start_row integer? Zero-based replacement start row.
---@field start_col integer? Zero-based replacement start column.
---@field end_row integer? Zero-based replacement end row.
---@field end_col integer? Zero-based replacement end column.
---@field response string Accumulated response text.
---@field new fun(api: louiselm.session.Api, options?: louiselm.ui.InlineOptions): louiselm.ui.Inline?, string?
---@field run fun(self: louiselm.ui.Inline, prompt: string, agent_name?: string): string|number?, string? Start the prompt; returns the session id while startup is pending.
---@field dispose fun(self: louiselm.ui.Inline): boolean
---@field cancel fun(self: louiselm.ui.Inline): boolean, string? Cancel this inline turn without affecting other Sessions.

local Decisions = require("louiselm.ui.chat.decisions")
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
---@param session louiselm.session.Session
---@param event louiselm.session.Event
local function handle_event(self, session, event)
  if self.disposed or self.session ~= session then
    if event.type == "permission_requested" then
      self.decisions:request(session, nil, event.respond)
    end
    return
  end
  if event.type == "permission_requested" then
    self.decisions:request(session, event.data, event.respond)
  elseif event.type == "permission_cancelled" then
    self.decisions:cancel(session, type(event.data) == "table" and event.data.request_ids or nil)
  elseif event.type == "chunk" and not self.cancelled then
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
  elseif event.type == "turn_done" or event.type == "error" or event.type == "prompt_rejected" then
    self.running = false
  end
end

---@param self louiselm.ui.Inline
---@return string? context
---@return string? error_message
local function capture_context(self)
  local buffer = nvim.api.nvim_get_current_buf()
  local start = nvim.fn.getpos("'<")
  local finish = nvim.fn.getpos("'>")
  local lines
  if start[2] > 0 and finish[2] > 0 then
    local mode = nvim.fn.visualmode()
    if mode == "\22" then
      return nil, "inline does not support blockwise selections; use a characterwise or linewise selection"
    end
    if mode ~= "v" and mode ~= "V" then
      return nil, "inline selection mode is unavailable; select the text again"
    end
    -- Neovim owns Visual mode, exclusive endpoints and multibyte geometry.
    local captured, selected, regions = pcall(function()
      return nvim.fn.getregion(start, finish, { type = mode }),
        nvim.fn.getregionpos(start, finish, { type = mode, eol = true })
    end)
    if not captured then
      return nil, "could not capture inline selection: " .. tostring(selected)
    end
    if #selected == 0 or #regions == 0 then
      return nil, "inline selection is unavailable; select the text again"
    end
    lines = selected
    local first = regions[1][1]
    local last = regions[#regions][1]
    self.start_row, self.start_col = first[2] - 1, first[3] - 1
    self.end_row = last[2] - 1
    local last_line = nvim.api.nvim_buf_get_lines(buffer, self.end_row, self.end_row + 1, false)[1]
    self.end_col = math.min(#last_line, last[3] - 1 + #lines[#lines])
  else
    local cursor = nvim.api.nvim_win_get_cursor(0)
    self.start_row, self.start_col = cursor[1] - 1, cursor[2]
    self.end_row, self.end_col = self.start_row, self.start_col
    lines = nvim.api.nvim_buf_get_lines(buffer, self.start_row, self.start_row + 1, false)
  end
  self.buffer = buffer
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
  if options ~= nil and options.decisions ~= nil and type(options.decisions) ~= "table" then
    return nil, "inline decisions must be a permission controller"
  end
  local agents, agents_error = copy_agents(options and options.agents)
  if agents == nil then
    return nil, agents_error
  end
  if options and options.cwd ~= nil and type(options.cwd) ~= "string" then
    return nil, "inline cwd must be a string"
  end
  local inline = setmetatable({
    api = api,
    agents = agents,
    cwd = options and options.cwd,
    unsubscribe = nil,
    disposed = false,
    running = false,
    cancelled = false,
    owns_decisions = options == nil or options.decisions == nil,
    response = "",
  }, Inline)
  inline.decisions = options and options.decisions
    or Decisions.new({
      is_live = function(session)
        return not inline.disposed
          and inline.session == session
          and inline.buffer ~= nil
          and nvim.api.nvim_buf_is_valid(inline.buffer)
      end,
      report_error = function(_, message)
        nvim.notify("louiselm: " .. message, nvim.log.levels.ERROR)
      end,
      resolved = function() end,
    })
  return inline, nil
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
    self.decisions:retire(self.session)
    self.session:dispose()
    self.session = nil
  end
  self.response = ""
  self.running = true
  self.cancelled = false
  local session, session_error
  local function ready(ready_session, ready_error)
    nvim.schedule(function()
      if self.disposed or session == nil or self.session ~= session or self.cancelled then
        return
      end
      if ready_session == nil then
        self.running = false
        nvim.notify("louiselm: " .. (ready_error or "inline session failed to start"), nvim.log.levels.ERROR)
        return
      end
      local request_id, prompt_error = ready_session:prompt({
        { type = "text", text = context },
        { type = "text", text = prompt },
      })
      if request_id == nil then
        self.running = false
        nvim.notify("louiselm: " .. (prompt_error or "inline prompt failed"), nvim.log.levels.ERROR)
      end
    end)
  end
  session, session_error = self.api:create_session(agent_name, { cwd = self.cwd }, ready)
  if session == nil then
    self.running = false
    return nil, session_error
  end
  self.session = session
  self.unsubscribe = session:on(function(event)
    nvim.schedule(function()
      handle_event(self, session, event)
    end)
  end)
  self.decisions:present_next()
  return session:inspect().id, nil
end

---Cancel this inline turn and discard late output; startup cancellation disposes its unsent Session.
---@param self louiselm.ui.Inline
---@return boolean cancelled Whether cancellation succeeded.
---@return string? error_message Lifecycle or ACP cancellation failure.
function Inline:cancel()
  if self.disposed then
    return false, "inline UI is disposed"
  end
  local session = self.session
  if session == nil or not self.running then
    return false, "no inline prompt is running"
  end
  local status = session:inspect().status
  if status == "starting" or status == "ready" then
    self.cancelled = true
    self.running = false
    self.decisions:retire(session)
    session:dispose()
    self.session = nil
    self.decisions:present_next()
    return true
  end
  local sent, err = session:cancel()
  if sent then
    self.cancelled = true
  end
  return sent, err
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
    self.decisions:retire(self.session)
    self.session:dispose()
    self.session = nil
  end
  if self.owns_decisions then
    self.decisions:dispose()
  else
    self.decisions:present_next()
  end
  return true
end

return M
