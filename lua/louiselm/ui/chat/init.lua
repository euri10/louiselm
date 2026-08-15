local Context = require("louiselm.ui.context")
local Diff = require("louiselm.ui.diff")
local Skills = require("louiselm.skills")

---@class louiselm.ui.ChatOptions
---@field agents? string[] Agent names shown by the new-session picker.
---@field skills? louiselm.skills.Skill[] Skills shown by the invocation picker.
---@field skill_paths? string[] Configured roots rediscovered when the invocation picker opens.
---@field initial_contexts? louiselm.ui.ContextItem[] Context queued for every new session.
---@field skill_context? louiselm.ui.ContextItem Skill catalog queued only for inject sessions.

---@class louiselm.ui.ChatView
---@field session louiselm.session.Session Attached session.
---@field buffer integer Scratch buffer for the session.
---@field window integer Window displaying the session.
---@field source_buffer integer Buffer that was current when the chat view was attached.
---@field prompt_line integer Zero-based prompt line.
---@field transcript_tail integer? Zero-based last rendered transcript line.
---@field response_line integer? Zero-based first streamed response line.
---@field response_tail integer? Zero-based last streamed response line.
---@field response_started boolean Whether the assistant has rendered response text for this turn.
---@field tool_lines table<string, integer> Zero-based rendered tool lines by ID.
---@field tool_titles table<string, string> Tool titles by ID.
---@field contexts louiselm.ui.ContextItem[] Context items queued for the next prompt.
---@field context_prefix string Visible context markers prefixed to the prompt.
---@field queued_prompt louiselm.ui.QueuedPrompt? Prompt committed for the next completed turn.
---@field queue_mark integer? Extmark showing queued prompt state.
---@field queue_namespace integer Extmark namespace for queued prompt state.
---@field setup_shown boolean Whether the initial options overview was offered.
---@field unsubscribe fun() Session event listener removal function.

---@class louiselm.ui.QueuedPrompt
---@field text string User-authored prompt text without visible context markers.
---@field content string|table Prompt content with snapshotted context.

---@class louiselm.ui.Chat
---@field api louiselm.session.Api Session API used to create sessions.
---@field agents string[] Agent names for the picker.
---@field skills louiselm.skills.Skill[] Skills for the invocation picker.
---@field skill_paths string[] Configured roots rediscovered when the invocation picker opens.
---@field skill_warning_signature string? Last reported discovery diagnostics, for notification deduplication.
---@field initial_contexts louiselm.ui.ContextItem[] Context queued for every new session.
---@field skill_context? louiselm.ui.ContextItem Skill catalog queued only for inject sessions.
---@field diff louiselm.ui.Diff File-edit review UI.
---@field queue_namespace integer Extmark namespace for queued prompt indicators.
---@field views table<string, louiselm.ui.ChatView> Views by local session id.
---@field winbars table<integer, string> Previous window bars by window id.
---@field current_id string? Currently displayed session id.
---@field disposed boolean Whether the chat UI has been disposed.
---@field attach fun(self: louiselm.ui.Chat, session: louiselm.session.Session): boolean, string? Attach or focus a session.
---@field buffer fun(self: louiselm.ui.Chat, session_id?: string): integer? Return a session buffer.
---@field switch fun(self: louiselm.ui.Chat, session_id: string): boolean, string? Focus an attached session.
---@field switch_session fun(self: louiselm.ui.Chat): boolean, string? Pick an attached session and focus it.
---@field close_session fun(self: louiselm.ui.Chat): boolean, string? Close the current session, confirming when active.
---@field cancel fun(self: louiselm.ui.Chat): boolean, string? Cancel the current session turn.
---@field session_options fun(self: louiselm.ui.Chat): boolean, string? Open the current session options overview.
---@field manage_permissions fun(self: louiselm.ui.Chat): boolean, string? Inspect and revoke remembered permission rules.
---@field rename_session fun(self: louiselm.ui.Chat, name: string): boolean, string? Rename the current session.
---@field session_id fun(self: louiselm.ui.Chat): string?, string? Return the current agent-scoped ACP session identifier.
---@field set_config_option fun(self: louiselm.ui.Chat, id: string, value: string|boolean, callback?: fun(options: louiselm.session.ConfigOption[]?, error?: string)): string|number?, string? Change an idle session option.
---@field submit fun(self: louiselm.ui.Chat, text?: string): string|number|boolean?, string? Submit or queue the current prompt.
---@field queue_context fun(self: louiselm.ui.Chat, item: louiselm.ui.ContextItem): boolean, string? Queue context for the next prompt.
---@field mention_buffer fun(self: louiselm.ui.Chat): boolean, string? Queue the source buffer context.
---@field send_selection fun(self: louiselm.ui.Chat): boolean, string? Queue the source visual selection.
---@field pick_file fun(self: louiselm.ui.Chat, root?: string): boolean, string? Pick and queue a file context.
---@field pick_skill fun(self: louiselm.ui.Chat): boolean, string? Pick and queue a skill invocation.
---@field new_session fun(self: louiselm.ui.Chat, agent_name?: string, options?: louiselm.session.Options): louiselm.session.Session?, string? Create a session, using the picker when needed.
---@field resume_session fun(self: louiselm.ui.Chat, all_workspaces?: boolean): boolean, string? Discover and load a prior ACP session.
---@field dispose fun(self: louiselm.ui.Chat): boolean Dispose buffers and listeners.

local M = {}
local Chat = {}
Chat.__index = Chat

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

---@param value unknown
---@param label string
---@return string[]? values
---@return string? error_message
local function copy_string_array(value, label)
  if value == nil then
    return {}
  end
  if type(value) ~= "table" then
    return nil, "chat " .. label .. " must be a string[]"
  end
  local values = {}
  for index = 1, #value do
    if type(value[index]) ~= "string" or value[index] == "" then
      return nil, "chat " .. label .. " must be a string[]"
    end
    values[index] = value[index]
  end
  for key in pairs(value) do
    if type(key) ~= "number" or key < 1 or key > #value or key % 1 ~= 0 then
      return nil, "chat " .. label .. " must be a dense string[]"
    end
  end
  return values
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

---@param value string
---@return string line
local function single_line(value)
  return (value:gsub("[\r\n]", " "))
end

local ACTIVE_TURN_STATUS = {
  prompting = true,
  waiting_permission = true,
  cancelling = true,
}

---@param view louiselm.ui.ChatView
local function clear_queued_prompt(view)
  view.queued_prompt = nil
  if view.queue_mark ~= nil and nvim.api.nvim_buf_is_valid(view.buffer) then
    nvim.api.nvim_buf_del_extmark(view.buffer, view.queue_namespace, view.queue_mark)
  end
  view.queue_mark = nil
end

---@param value number
---@return string
local function format_number(value)
  if value % 1 == 0 then
    return string.format("%.0f", value)
  end
  return tostring(value)
end

---@param agent string
---@param acp_session_id string
---@return string
local function report_id(agent, acp_session_id)
  return agent .. "/" .. acp_session_id
end

---@param state louiselm.session.State
---@return string
local function session_identity(state)
  local parts
  if state.acp_session_id ~= nil then
    local identity = report_id(state.agent, state.acp_session_id)
    if state.name ~= nil and state.name ~= state.id then
      parts = { identity, state.name, state.id, state.status or "unknown" }
    else
      parts = { identity, state.id, state.status or "unknown" }
    end
  elseif state.name ~= nil and state.name ~= state.id then
    parts = { state.name, state.agent, state.id, state.status or "unknown" }
  else
    parts = { state.agent, state.id, state.status or "unknown" }
  end
  if state.source == "loaded" then
    parts[#parts + 1] = "loaded"
  end
  if state.skills_policy ~= nil then
    parts[#parts + 1] = "skills: " .. (state.skills_policy == "off" and "off" or "on")
  end
  return table.concat(parts, " · ")
end

---@param state louiselm.session.State
---@return string
local function session_summary(state)
  local parts = { session_identity(state) }
  if state.activity ~= nil then
    parts[#parts + 1] = "activity=" .. single_line(state.activity)
  end
  for _, option in ipairs(state.config_options or {}) do
    parts[#parts + 1] = option.name .. "=" .. tostring(option.current_value)
  end
  if state.context ~= nil then
    local stale = state.context.stale and " stale" or ""
    parts[#parts + 1] = string.format(
      "context=%s/%s (%.0f%% %s%s)",
      format_number(state.context.used),
      format_number(state.context.size),
      state.context.percentage,
      state.context.pressure,
      stale
    )
  end
  if state.cost ~= nil then
    parts[#parts + 1] = "cost=" .. format_number(state.cost.amount) .. " " .. state.cost.currency
  end
  return table.concat(parts, " · ")
end

---@param session louiselm.session.DiscoveredSession
---@return string
local function discovered_session_summary(session)
  local parts = {
    report_id(single_line(session.agent), single_line(session.session_id)),
  }
  if session.title ~= nil and session.title ~= "" and session.title ~= session.session_id then
    parts[#parts + 1] = single_line(session.title)
  end
  parts[#parts + 1] = "cwd=" .. single_line(session.cwd)
  if session.updated_at ~= nil then
    parts[#parts + 1] = "updated=" .. single_line(session.updated_at)
  end
  return table.concat(parts, " · ")
end

---@param errors louiselm.session.DiscoveryError[]
---@return string
local function discovery_error_summary(errors)
  local messages = {}
  for _, discovery_error in ipairs(errors) do
    messages[#messages + 1] = discovery_error.agent .. ": " .. discovery_error.message
  end
  return table.concat(messages, "; ")
end

---@param state louiselm.session.State
---@return string
local function turn_label(state)
  if state.status == "ready" then
    return "Your turn"
  end
  if state.status == "prompting" then
    return "Model responding"
  end
  if state.status == "waiting_permission" then
    return "Waiting for permission"
  end
  if state.status == "cancelling" then
    return "Stopping"
  end
  if state.status == "starting" or state.status == "configuring" then
    return "Starting"
  end
  if state.status == "error" then
    return "Error"
  end
  return "Unavailable"
end

---@param state louiselm.session.State
---@return string
local function session_header(state)
  return session_summary(state) .. " · " .. turn_label(state)
end

---@param self louiselm.ui.Chat
---@param view louiselm.ui.ChatView
---@param win integer
local function render_winbar(self, view, win)
  if not nvim.api.nvim_win_is_valid(win) or nvim.api.nvim_win_get_buf(win) ~= view.buffer then
    return
  end
  if self.winbars[win] == nil then
    self.winbars[win] = nvim.api.nvim_get_option_value("winbar", { win = win })
  end
  nvim.api.nvim_set_option_value("winbar", turn_label(view.session:inspect()), { win = win })
end

---@param self louiselm.ui.Chat
local function restore_winbars(self)
  for win, value in pairs(self.winbars) do
    if nvim.api.nvim_win_is_valid(win) then
      nvim.api.nvim_set_option_value("winbar", value, { win = win })
    end
  end
  self.winbars = {}
end

---@param view louiselm.ui.ChatView
local function render_header(view)
  set_line(view.buffer, 0, "# " .. session_header(view.session:inspect()))
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

---@param view louiselm.ui.ChatView
---@return string text
local function prompt_text(view)
  local lines = nvim.api.nvim_buf_get_lines(view.buffer, view.prompt_line, -1, false)
  for index, line in ipairs(lines) do
    lines[index] = line:sub(1, 2) == "> " and line:sub(3) or line
  end
  return table.concat(lines, "\n")
end

---@param view louiselm.ui.ChatView
---@param text string
---@return integer line_count
local function replace_prompt(view, text)
  local lines = split_lines(text)
  for index, line in ipairs(lines) do
    lines[index] = "> " .. line
  end
  nvim.api.nvim_buf_set_lines(view.buffer, view.prompt_line, -1, false, lines)
  return #lines
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

local insert_transcript
local open_session_options

---@param option unknown
---@return string? identifier
---@return string label
local function permission_option(option)
  if type(option) == "string" and option ~= "" then
    return option, option
  end
  if type(option) ~= "table" then
    return nil, "invalid option"
  end
  local identifier = option.optionId or option.option_id
  if type(identifier) ~= "string" or identifier == "" then
    return nil, "invalid option"
  end
  local label = option.name or option.kind or identifier
  if type(label) ~= "string" or label == "" then
    label = identifier
  end
  return identifier, label
end

---@param value unknown
---@return unknown[]? options
local function permission_options(value)
  if type(value) ~= "table" then
    return nil
  end
  local options = {}
  for _, option in ipairs(value) do
    local identifier = permission_option(option)
    if identifier ~= nil then
      options[#options + 1] = option
    end
  end
  return options
end

---@param self louiselm.ui.Chat
---@param view louiselm.ui.ChatView
---@param respond fun(result: unknown, error?: louiselm.acp.JsonRpcError): boolean, string?
---@param result table
---@return boolean sent
local function send_permission_response(self, view, respond, result)
  if self.disposed or self.views[view.session:inspect().id] ~= view or not nvim.api.nvim_buf_is_valid(view.buffer) then
    return false
  end
  local call_ok, sent, send_error = pcall(respond, result)
  if not call_ok or not sent then
    local message = call_ok and (send_error or "permission response could not be sent") or tostring(sent)
    insert_transcript(self, view, { "Error: " .. message })
    return false
  end
  return true
end

---@param self louiselm.ui.Chat
---@param view louiselm.ui.ChatView
---@param respond? fun(result: unknown, error?: louiselm.acp.JsonRpcError): boolean, string?
---@return boolean sent
local function cancel_permission(self, view, respond)
  if type(respond) ~= "function" then
    insert_transcript(self, view, { "Error: permission request has no response callback" })
    return false
  end
  return send_permission_response(self, view, respond, { outcome = { outcome = "cancelled" } })
end

---@param self louiselm.ui.Chat
---@param view louiselm.ui.ChatView
---@param data table
---@param respond? fun(result: unknown, error?: louiselm.acp.JsonRpcError): boolean, string?
local function prompt_permission(self, view, data, respond)
  if type(respond) ~= "function" then
    insert_transcript(self, view, { "Error: permission request has no response callback" })
    return
  end
  local options = permission_options(data.options)
  if options == nil or #options == 0 then
    cancel_permission(self, view, respond)
    return
  end
  local operation = data.operation
  local kind = type(operation) == "table" and operation.kind or "unknown"
  nvim.ui.select(options, {
    prompt = "louiselm permission (" .. tostring(kind) .. "): ",
    format_item = function(option)
      local _, label = permission_option(option)
      return label
    end,
  }, function(choice)
    if choice == nil then
      cancel_permission(self, view, respond)
      return
    end
    local identifier = permission_option(choice)
    if identifier == nil then
      cancel_permission(self, view, respond)
      return
    end
    send_permission_response(self, view, respond, { outcome = { outcome = "selected", optionId = identifier } })
  end)
end

---@param self louiselm.ui.Chat
---@param view louiselm.ui.ChatView
---@param lines string[]
insert_transcript = function(self, view, lines)
  local replacement = {}
  for _, line in ipairs(lines) do
    for _, part in ipairs(split_lines(line)) do
      replacement[#replacement + 1] = part
    end
  end
  local insertion_line = view.transcript_tail == nil and view.prompt_line or view.transcript_tail + 1
  nvim.api.nvim_buf_set_lines(view.buffer, insertion_line, insertion_line, false, replacement)
  view.transcript_tail = insertion_line + #replacement - 1
  view.prompt_line = view.prompt_line + #replacement
end

---@param view louiselm.ui.ChatView
---@param text string
---@return string|table content
local function prompt_content(view, text)
  if #view.contexts == 0 then
    return text
  end
  local content = {}
  for _, item in ipairs(view.contexts) do
    content[#content + 1] = { type = "text", text = item.text }
  end
  if text ~= "" then
    content[#content + 1] = { type = "text", text = text }
  end
  return content
end

---@param view louiselm.ui.ChatView
---@param text string
local function set_prompt_line(view, text)
  replace_prompt(view, view.context_prefix .. text)
end

---@param message string
local function notify_prompt_error(message)
  nvim.notify("louiselm: " .. message, nvim.log.levels.ERROR)
end

---@param self louiselm.ui.Chat
---@param view louiselm.ui.ChatView
---@param text string
---@param content string|table
---@return string|number? request_id
---@return string? error_message
local function submit_prompt(self, view, text, content)
  local request_id, prompt_error = view.session:prompt(content)
  if request_id == nil then
    local message = prompt_error or "prompt failed"
    notify_prompt_error(message)
    return nil, message
  end

  clear_queued_prompt(view)
  local prompt_line_count = replace_prompt(view, text)
  local response_line = view.prompt_line + prompt_line_count
  nvim.api.nvim_buf_set_lines(view.buffer, response_line, response_line, false, { "", "> " })
  view.response_line = response_line
  view.response_tail = view.response_line
  view.response_started = false
  view.transcript_tail = view.response_tail
  view.prompt_line = response_line + 1
  if nvim.api.nvim_get_current_buf() == view.buffer then
    nvim.api.nvim_win_set_cursor(0, { view.prompt_line + 1, 2 })
  end
  view.contexts = {}
  view.context_prefix = ""
  return request_id
end

---@param self louiselm.ui.Chat
---@param view louiselm.ui.ChatView
---@param text string
---@param content string|table
local function queue_prompt(self, view, text, content)
  clear_queued_prompt(view)
  set_prompt_line(view, text)
  view.queued_prompt = { text = text, content = content }
  view.queue_mark = nvim.api.nvim_buf_set_extmark(view.buffer, self.queue_namespace, view.prompt_line, -1, {
    virt_text = { { "Queued for next turn", "Comment" } },
    virt_text_pos = "eol",
  })
end

---@param self louiselm.ui.Chat
---@param view louiselm.ui.ChatView
local function release_queued_prompt(self, view)
  local queued = view.queued_prompt
  if queued == nil then
    return
  end
  clear_queued_prompt(view)
  submit_prompt(self, view, queued.text, queued.content)
end

---@param option louiselm.session.ConfigOption
---@return table[] values
local function config_values(option)
  if option.type == "boolean" then
    return { { value = true, name = "true" }, { value = false, name = "false" } }
  end
  return option.options or {}
end

---@param self louiselm.ui.Chat
---@param view louiselm.ui.ChatView
---@param initial boolean
open_session_options = function(self, view, initial)
  if self.disposed or self.views[view.session:inspect().id] ~= view then
    return
  end
  local state = view.session:inspect()
  if state.status ~= "ready" or #state.config_options == 0 then
    return
  end
  if initial then
    if view.setup_shown then
      return
    end
    view.setup_shown = true
  end
  nvim.ui.select(state.config_options, {
    prompt = "louiselm session options: ",
    format_item = function(option)
      return option.name .. ": " .. tostring(option.current_value)
    end,
  }, function(option)
    if option == nil or self.disposed or self.views[state.id] ~= view then
      return
    end
    nvim.ui.select(config_values(option), {
      prompt = option.name .. ": ",
      format_item = function(value)
        return value.name
      end,
    }, function(choice)
      if self.disposed or self.views[state.id] ~= view then
        return
      end
      if choice == nil then
        open_session_options(self, view, false)
        return
      end
      local _, set_error = self:set_config_option(option.id, choice.value, function(_, callback_error)
        nvim.schedule(function()
          if self.disposed or self.views[state.id] ~= view then
            return
          end
          if callback_error ~= nil then
            insert_transcript(self, view, { "Error: " .. callback_error })
          else
            render_header(view)
          end
          open_session_options(self, view, false)
        end)
      end)
      if set_error ~= nil then
        insert_transcript(self, view, { "Error: " .. set_error })
      end
    end)
  end)
end

local TURN_USAGE_FIELDS = {
  "total_tokens",
  "input_tokens",
  "output_tokens",
  "thought_tokens",
  "cached_read_tokens",
  "cached_write_tokens",
}

---@param usage louiselm.session.TurnUsage?
---@return string? line
local function usage_line(usage)
  if usage == nil then
    return nil
  end
  local fields = {}
  for _, name in ipairs(TURN_USAGE_FIELDS) do
    if usage[name] ~= nil then
      fields[#fields + 1] = name .. "=" .. format_number(usage[name])
    end
  end
  if #fields == 0 then
    return nil
  end
  return "[usage] " .. table.concat(fields, " · ")
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

  if event.type == "state_changed" or event.type == "config_options_changed" or event.type == "usage_updated" then
    render_header(view)
    render_winbar(self, view, view.window)
  end
  if event.type == "state_changed" and view.session:inspect().status == "ready" then
    open_session_options(self, view, true)
  end

  if event.type == "chunk" then
    local text = chunk_text(event.data)
    if text == nil then
      return
    end
    if not view.response_started then
      local lines = split_lines(text)
      local insertion_line = view.response_tail
        or (view.transcript_tail == nil and view.prompt_line or view.transcript_tail + 1)
      if view.response_tail ~= nil then
        insertion_line = insertion_line + 1
      end
      nvim.api.nvim_buf_set_lines(view.buffer, insertion_line, insertion_line, false, lines)
      view.response_line = insertion_line
      view.response_tail = insertion_line + #lines - 1
      view.transcript_tail = view.response_tail
      view.prompt_line = view.prompt_line + #lines
      view.response_started = true
      return
    end
    local current = nvim.api.nvim_buf_get_lines(view.buffer, view.response_tail, view.response_tail + 1, false)[1] or ""
    local lines = split_lines(current .. text)
    nvim.api.nvim_buf_set_lines(view.buffer, view.response_tail, view.response_tail + 1, false, lines)
    local added = #lines - 1
    view.response_tail = view.response_tail + added
    view.transcript_tail = view.response_tail
    view.prompt_line = view.prompt_line + added
  elseif event.type == "tool_call_started" or event.type == "tool_call_finished" then
    local status = field(event.data, "status")
    local id = tool_id(event.data)
    local title = field(event.data, "title")
    if title ~= nil then
      title = single_line(title)
    end
    if event.type == "tool_call_started" then
      if title ~= nil then
        view.tool_titles[id] = title
      end
      local detail = id
      if title ~= nil then
        detail = detail .. ": " .. title
      end
      insert_transcript(self, view, { "[tool] " .. detail .. " (started)" })
      view.tool_lines[id] = view.transcript_tail
    else
      title = title or view.tool_titles[id]
      local detail = id
      if title ~= nil then
        detail = detail .. ": " .. title
      end
      detail = detail .. " (" .. (status or "finished") .. ")"
      local line = view.tool_lines[id]
      if line ~= nil and line < nvim.api.nvim_buf_line_count(view.buffer) then
        set_line(view.buffer, line, "[tool] " .. detail)
      else
        insert_transcript(self, view, { "[tool] " .. detail })
      end
      view.tool_lines[id] = nil
      view.tool_titles[id] = nil
    end
    view.response_line = nil
    view.response_tail = nil
    view.response_started = false
  elseif event.type == "error" then
    clear_queued_prompt(view)
    local message = field(event.data, "message") or "unknown session error"
    insert_transcript(self, view, { "Error: " .. message })
    view.response_line = nil
    view.response_tail = nil
    view.response_started = false
  elseif event.type == "permission_requested" then
    local data = event.data
    if type(data) == "table" and type(data.permission_error) == "string" then
      insert_transcript(self, view, { "Warning: " .. data.permission_error })
    elseif type(data) == "table" and data.remembered_decision ~= nil then
      insert_transcript(self, view, { "Warning: remembered decision requires a compatible once-only option" })
    end
    if type(data) == "table" and type(data.operation) == "table" and data.operation.kind == "file_edit" then
      local opened, open_error = self.diff:open(data, event.respond)
      if not opened then
        insert_transcript(self, view, { "Error: " .. (open_error or "could not open diff review") })
        cancel_permission(self, view, event.respond)
      end
    elseif type(data) == "table" then
      prompt_permission(self, view, data, event.respond)
    else
      cancel_permission(self, view, event.respond)
    end
  elseif event.type == "turn_done" then
    local line = usage_line(view.session:inspect().usage)
    if line ~= nil then
      insert_transcript(self, view, { line })
    end
    view.response_line = nil
    view.response_tail = nil
    view.response_started = false
    release_queued_prompt(self, view)
  end
end

---@param rule louiselm.permission.Rule
---@return string
local function permission_rule_label(rule)
  local scope
  if rule.kind == "command" then
    scope = "command " .. nvim.json.encode(rule.command)
  else
    scope = "file " .. tostring(rule.path)
  end
  return table.concat({ rule.decision .. " " .. rule.lifetime, rule.agent, rule.workspace, scope }, " · ")
end

---Inspect remembered permissions and confirm revocation through the configured picker.
---@param self louiselm.ui.Chat
---@return boolean started
---@return string? error_message State or API error.
function Chat:manage_permissions()
  if self.disposed then
    return false, "chat UI is disposed"
  end
  if type(self.api.list_permissions) ~= "function" or type(self.api.revoke_permission) ~= "function" then
    return false, "session API does not support remembered permissions"
  end
  local rules, rules_error = self.api:list_permissions()
  if rules == nil then
    return false, rules_error or "could not read remembered permissions"
  end
  if #rules == 0 then
    nvim.notify("louiselm: no remembered permissions", nvim.log.levels.INFO)
    return true
  end
  nvim.ui.select(rules, {
    prompt = "louiselm remembered permissions: ",
    format_item = permission_rule_label,
  }, function(rule)
    if rule == nil or self.disposed then
      return
    end
    nvim.ui.select({ "Revoke", "Keep" }, { prompt = "revoke " .. rule.id .. "? " }, function(choice)
      if choice ~= "Revoke" or self.disposed then
        return
      end
      local revoked, revoke_error = self.api:revoke_permission(rule.id)
      if not revoked then
        nvim.notify("louiselm: " .. (revoke_error or "permission rule no longer exists"), nvim.log.levels.ERROR)
        return
      end
      nvim.notify("louiselm: revoked permission " .. rule.id, nvim.log.levels.INFO)
    end)
  end)
  return true
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
      if
        key ~= "agents"
        and key ~= "skills"
        and key ~= "skill_paths"
        and key ~= "initial_contexts"
        and key ~= "skill_context"
      then
        return nil, "unknown chat option '" .. tostring(key) .. "'"
      end
    end
  end
  local agents, agents_error = copy_string_array(options and options.agents, "agents")
  if agents == nil then
    return nil, agents_error
  end
  local skill_paths, skill_paths_error = copy_string_array(options and options.skill_paths, "skill paths")
  if skill_paths == nil then
    return nil, skill_paths_error
  end
  local skills, skills_error = copy_skills(options and options.skills)
  if skills == nil then
    return nil, skills_error
  end
  local initial_contexts, contexts_error = copy_initial_contexts(options and options.initial_contexts)
  if initial_contexts == nil then
    return nil, contexts_error
  end
  local skill_contexts, skill_context_error =
    copy_initial_contexts(options and options.skill_context and { options.skill_context } or nil)
  if skill_contexts == nil then
    return nil, skill_context_error
  end
  local chat = setmetatable({
    api = api,
    agents = agents,
    skills = skills,
    skill_paths = skill_paths,
    skill_warning_signature = nil,
    initial_contexts = initial_contexts,
    skill_context = skill_contexts[1],
    diff = Diff.new(),
    queue_namespace = nvim.api.nvim_create_namespace("louiselm.chat.queued_prompt"),
    views = {},
    winbars = {},
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
  local window = nvim.api.nvim_get_current_win()
  local buffer = nvim.api.nvim_create_buf(false, true)
  nvim.api.nvim_buf_set_name(buffer, "louiselm://" .. state.id)
  nvim.api.nvim_set_option_value("buftype", "nofile", { buf = buffer })
  nvim.api.nvim_set_option_value("bufhidden", "hide", { buf = buffer })
  nvim.api.nvim_set_option_value("swapfile", false, { buf = buffer })
  nvim.api.nvim_set_option_value("filetype", "markdown", { buf = buffer })
  nvim.api.nvim_buf_set_lines(buffer, 0, -1, false, { "# " .. session_header(state), "", "> " })

  local view = {
    session = session,
    buffer = buffer,
    window = window,
    source_buffer = source_buffer,
    prompt_line = 2,
    transcript_tail = nil,
    response_line = nil,
    response_tail = nil,
    response_started = false,
    tool_lines = {},
    tool_titles = {},
    contexts = {},
    context_prefix = "",
    queued_prompt = nil,
    queue_mark = nil,
    queue_namespace = self.queue_namespace,
    setup_shown = false,
    unsubscribe = function() end,
  }
  nvim.api.nvim_buf_attach(buffer, false, {
    on_lines = function(_, _, _, first_line, last_line)
      if view.queued_prompt ~= nil and first_line <= view.prompt_line and last_line > view.prompt_line then
        clear_queued_prompt(view)
      end
    end,
  })
  view.unsubscribe = session:on(function(event)
    -- ACP stdout callbacks run in a fast event; buffer APIs must run later.
    nvim.schedule(function()
      handle_event(self, view, event)
    end)
  end)
  self.views[state.id] = view
  self.current_id = state.id
  nvim.api.nvim_set_current_buf(buffer)
  nvim.api.nvim_win_set_cursor(0, { view.prompt_line + 1, 2 })
  if #nvim.api.nvim_list_uis() > 0 then
    nvim.cmd.startinsert()
    nvim.api.nvim_win_set_cursor(0, { view.prompt_line + 1, 2 })
  end
  render_winbar(self, view, window)
  nvim.keymap.set("i", "<CR>", function()
    self:submit()
  end, { buffer = buffer, silent = true, desc = "Submit louiselm prompt" })
  if state.status == "ready" and #state.config_options > 0 then
    nvim.schedule(function()
      open_session_options(self, view, true)
    end)
  end
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
  restore_winbars(self)
  self.current_id = session_id
  view.window = nvim.api.nvim_get_current_win()
  nvim.api.nvim_set_current_buf(view.buffer)
  render_winbar(self, view, view.window)
  return true
end

---Pick one attached session using a compact state and telemetry row.
---@param self louiselm.ui.Chat
---@return boolean started
---@return string? error_message Lifecycle error.
function Chat:switch_session()
  if self.disposed then
    return false, "chat UI is disposed"
  end
  local sessions = {}
  for _, id in ipairs(self.api:list_sessions()) do
    local view = self.views[id]
    if view ~= nil then
      sessions[#sessions + 1] = view.session
    end
  end
  if #sessions == 0 then
    return false, "no chat sessions are attached"
  end
  nvim.ui.select(sessions, {
    prompt = "louiselm session: ",
    format_item = function(session)
      return session_summary(session:inspect())
    end,
  }, function(session)
    if session ~= nil then
      self:switch(session:inspect().id)
    end
  end)
  return true
end

---Rename the current attached session.
---@param self louiselm.ui.Chat
---@param name string New non-empty session name.
---@return boolean renamed
---@return string? error_message
function Chat:rename_session(name)
  if self.disposed then
    return false, "chat UI is disposed"
  end
  local view = self.current_id and self.views[self.current_id]
  if view == nil then
    return false, "no chat session is attached"
  end
  return view.session:set_name(name)
end

---Return the current agent-scoped ACP session identifier for bug reports.
---@param self louiselm.ui.Chat
---@return string? session_id
---@return string? error_message
function Chat:session_id()
  if self.disposed then
    return nil, "chat UI is disposed"
  end
  local view = self.current_id and self.views[self.current_id]
  if view == nil then
    return nil, "no chat session is open"
  end
  local state = view.session:inspect()
  if state.acp_session_id == nil then
    return nil, "current session has no ACP session id yet"
  end
  return report_id(state.agent, state.acp_session_id)
end

---@param self louiselm.ui.Chat
---@param view louiselm.ui.ChatView
local function close_view(self, view)
  local id = view.session:inspect().id
  restore_winbars(self)
  clear_queued_prompt(view)
  view.unsubscribe()
  self.views[id] = nil
  local _, close_error = view.session:dispose()
  if nvim.api.nvim_buf_is_valid(view.buffer) then
    nvim.api.nvim_buf_delete(view.buffer, { force = true })
  end
  self.current_id = nil
  for _, candidate in ipairs(self.api:list_sessions()) do
    if self.views[candidate] ~= nil then
      self:switch(candidate)
      break
    end
  end
  if close_error ~= nil then
    nvim.notify("louiselm: " .. close_error, nvim.log.levels.ERROR)
  end
end

---Dispose the current session and remove only its chat buffer.
---@param self louiselm.ui.Chat
---@return boolean started
---@return string? error_message Lifecycle error.
function Chat:close_session()
  if self.disposed then
    return false, "chat UI is disposed"
  end
  local view = self.current_id and self.views[self.current_id]
  if view == nil then
    return false, "no chat session is attached"
  end
  local status = view.session:inspect().status
  local has_queued_prompt = view.queued_prompt ~= nil
  if
    has_queued_prompt
    or status == "configuring"
    or status == "prompting"
    or status == "waiting_permission"
    or status == "cancelling"
  then
    local prompt = has_queued_prompt and "close active louiselm session and discard queued prompt? "
      or "close active louiselm session? "
    nvim.ui.select({ "Close", "Keep" }, { prompt = prompt }, function(choice)
      if choice == "Close" and not self.disposed and self.views[view.session:inspect().id] == view then
        close_view(self, view)
      end
    end)
    return true
  end
  close_view(self, view)
  return true
end

---Cancel the current session turn.
---@param self louiselm.ui.Chat
---@return boolean sent
---@return string? error_message Lifecycle or session error.
function Chat:cancel()
  if self.disposed then
    return false, "chat UI is disposed"
  end
  local view = self.current_id and self.views[self.current_id]
  if view == nil then
    return false, "no chat session is attached"
  end
  return view.session:cancel()
end

---Open the complete configuration overview for the current idle session.
---@param self louiselm.ui.Chat
---@return boolean opened
---@return string? error_message Lifecycle or state error.
function Chat:session_options()
  if self.disposed then
    return false, "chat UI is disposed"
  end
  local view = self.current_id and self.views[self.current_id]
  if view == nil then
    return false, "no chat session is attached"
  end
  local state = view.session:inspect()
  if state.status ~= "ready" then
    return false, "session is not idle; cancel the active turn first"
  end
  if #state.config_options == 0 then
    return false, "session has no supported options"
  end
  open_session_options(self, view, false)
  return true
end

---Change one option on the current idle session.
---@param self louiselm.ui.Chat
---@param id string Option identifier.
---@param value string|boolean New typed option value.
---@param callback? fun(options: louiselm.session.ConfigOption[]?, error?: string) Completion callback.
---@return string|number? request_id
---@return string? error_message
function Chat:set_config_option(id, value, callback)
  if self.disposed then
    return nil, "chat UI is disposed"
  end
  local view = self.current_id and self.views[self.current_id]
  if view == nil then
    return nil, "no chat session is attached"
  end
  return view.session:set_config_option(id, value, callback)
end

---Submit text to the current session; slash commands are passed through unchanged.
---@param self louiselm.ui.Chat
---@param text? string Prompt text; defaults to the current buffer prompt line.
---@return string|number|boolean? request_id ACP request id, true when queued, or nil on failure.
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
    text = prompt_text(view)
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
  local content = prompt_content(view, text)
  local status = view.session:inspect().status
  if ACTIVE_TURN_STATUS[status] then
    queue_prompt(self, view, text, content)
    return true
  end

  set_prompt_line(view, text)
  if status ~= "ready" then
    notify_prompt_error("session is not ready")
    return nil, "session is not ready"
  end
  return submit_prompt(self, view, text, content)
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
  local view = self.current_id and self.views[self.current_id]
  if view == nil then
    return false, "no chat session is attached"
  end
  local state = view.session:inspect()
  if state.skills_policy == "off" then
    return false, "skill picker is disabled for this session"
  end
  local skills = self.skills
  if #self.skill_paths > 0 then
    local diagnostics
    skills, diagnostics = Skills.discover(self.skill_paths, state.working_dir)
    for _, diagnostic in ipairs(diagnostics) do
      if diagnostic.code == "missing_dependency" then
        return false, diagnostic.message
      end
    end
    if #diagnostics == 0 then
      self.skill_warning_signature = nil
    else
      local signature_parts = {}
      for _, diagnostic in ipairs(diagnostics) do
        signature_parts[#signature_parts + 1] = diagnostic.path .. "\0" .. diagnostic.message
      end
      local signature = table.concat(signature_parts, "\0")
      if signature ~= self.skill_warning_signature then
        nvim.notify(
          string.format(
            "louiselm: skill discovery found %d issue(s); run :checkhealth louiselm for details",
            #diagnostics
          ),
          nvim.log.levels.WARN
        )
        self.skill_warning_signature = signature
      end
    end
  end
  if #skills == 0 then
    return false, "no chat skills configured"
  end
  return Context.skills.pick(skills, function(skill, error_message)
    if skill ~= nil then
      self:queue_context(Context.skills.context(skill))
    elseif error_message ~= nil then
      nvim.notify("louiselm: " .. error_message, nvim.log.levels.ERROR)
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
  if session:inspect().skills_policy == "inject" and self.skill_context ~= nil then
    local queued, queue_error = self:queue_context(self.skill_context)
    if not queued then
      return nil, queue_error
    end
  end
  return session
end

---Discover and load an ACP session into a new chat buffer.
---@param self louiselm.ui.Chat
---@param all_workspaces? boolean Omit the current-workspace filter when true.
---@return boolean started
---@return string? error_message Validation or discovery startup error.
function Chat:resume_session(all_workspaces)
  if self.disposed then
    return false, "chat UI is disposed"
  end
  if all_workspaces ~= nil and type(all_workspaces) ~= "boolean" then
    return false, "all_workspaces must be a boolean"
  end
  local options = {}
  if not all_workspaces then
    options.cwd = nvim.fn.getcwd()
  end

  return self.api:discover_sessions(options, function(sessions, errors)
    -- ACP process callbacks are fast events; all selection and buffer work stays on the main loop.
    nvim.schedule(function()
      if self.disposed then
        return
      end
      if #errors > 0 then
        local level = #sessions == 0 and nvim.log.levels.ERROR or nvim.log.levels.WARN
        nvim.notify("louiselm: session discovery: " .. discovery_error_summary(errors), level)
      end
      if #sessions == 0 then
        if #errors == 0 then
          nvim.notify("louiselm: no recoverable sessions found", nvim.log.levels.INFO)
        end
        return
      end

      nvim.ui.select(sessions, {
        prompt = all_workspaces and "louiselm session (all workspaces): " or "louiselm session: ",
        format_item = discovered_session_summary,
      }, function(selected)
        if selected == nil or self.disposed then
          return
        end
        local error_reported = false
        local session, load_error = self.api:load_session(selected.agent, selected.session_id, {
          cwd = selected.cwd,
          name = single_line(selected.title ~= nil and selected.title ~= "" and selected.title or selected.session_id),
        }, function(_, ready_error)
          if ready_error == nil then
            return
          end
          error_reported = true
          nvim.schedule(function()
            if not self.disposed then
              nvim.notify("louiselm: " .. ready_error, nvim.log.levels.ERROR)
            end
          end)
        end)
        if session == nil then
          if not error_reported then
            nvim.notify("louiselm: " .. (load_error or "could not load session"), nvim.log.levels.ERROR)
          end
          return
        end
        local attached, attach_error = self:attach(session)
        if not attached then
          session:dispose()
          nvim.notify("louiselm: " .. (attach_error or "could not attach loaded session"), nvim.log.levels.ERROR)
        end
      end)
    end)
  end)
end

---Remove chat buffers and event listeners without disposing the sessions.
---@param self louiselm.ui.Chat
---@return boolean disposed
function Chat:dispose()
  if self.disposed then
    return true
  end
  self.disposed = true
  restore_winbars(self)
  self.diff:dispose()
  for id, view in pairs(self.views) do
    clear_queued_prompt(view)
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
