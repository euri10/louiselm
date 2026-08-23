local Context = require("louiselm.ui.context")
local Diff = require("louiselm.ui.diff")
local Gates = require("louiselm.permission.gates")
local Picker = require("louiselm.ui.picker")
local Skills = require("louiselm.skills")
local Transcript = require("louiselm.session.transcript")
local Usage = require("louiselm.workflow.usage")

---@class louiselm.ui.ChatOptions
---@field agents? string[] Agent names shown by the new-session picker.
---@field skills? louiselm.skills.Skill[] Skills shown by the invocation picker.
---@field skill_paths? string[] Configured roots rediscovered when the invocation picker opens.
---@field initial_contexts? louiselm.ui.ContextItem[] Context queued for every new session.
---@field skill_catalog? string Hidden catalog held for the first accepted model prompt in a new inject session.
---@field instructions_context? louiselm.ui.ContextItem Project instructions resource link queued only for brand-new sessions.

---@class louiselm.ui.ChatView
---@field session louiselm.session.Session Attached session.
---@field buffer integer Scratch buffer for the session.
---@field window integer Window displaying the session.
---@field source_buffer integer Buffer that was current when the chat view was attached.
---@field prompt_line integer Zero-based prompt line.
---@field prompt_mark integer Extmark tracking the prompt boundary through buffer edits.
---@field prompt_namespace integer Extmark namespace for the prompt boundary.
---@field transcript_tail integer? Zero-based last rendered transcript line.
---@field response_line integer? Zero-based first streamed response line.
---@field response_tail integer? Zero-based last streamed response line.
---@field response_started boolean Whether the assistant has rendered response text for this turn.
---@field last_block_kind ("prose"|"tool")? Kind of the most recently rendered transcript block; separates adjacent prose and tool blocks with a blank line.
---@field tool_lines table<string, integer> Zero-based rendered tool lines by ID.
---@field tool_ids table<integer, string> Tool-call IDs by zero-based rendered line.
---@field tool_statuses table<string, string> Latest tool status by ID.
---@field tool_titles table<string, string> Tool titles by ID.
---@field contexts louiselm.ui.ContextItem[] Context items queued for the next prompt.
---@field context_prefix string Visible context markers prefixed to the prompt.
---@field skill_catalog? string Hidden catalog pending for this new inject session.
---@field pending_skill? louiselm.skills.Skill Native-mode skill selection, resolved against advertised commands only at actual submission.
---@field context_folds louiselm.ui.ContextFold[] Submitted context fold ranges in this live buffer.
---@field fold_counts table<integer, integer> Number of context folds installed in each window.
---@field tool_folds louiselm.ui.ToolFold[] Completed tool-call fold ranges in this live buffer.
---@field tool_fold_counts table<integer, integer> Number of tool folds installed in each window.
---@field tool_fold_run louiselm.ui.ToolFoldRun? Contiguous rendered tool paragraph awaiting a boundary.
---@field tool_inspect_windows table<integer, boolean> Floating raw-payload windows owned by this chat.
---@field queued_prompt louiselm.ui.QueuedPrompt? Prompt committed for the next completed turn.
---@field queue_mark integer? Extmark showing queued prompt state.
---@field queue_namespace integer Extmark namespace for queued prompt state.
---@field setup_shown boolean Whether the initial options overview was offered.
---@field cost_before louiselm.session.Cost? Cumulative cost before the current turn.
---@field transcript louiselm.session.Transcript Full, untruncated record of this session's turns.
---@field unsubscribe fun() Session event listener removal function.

---@class louiselm.ui.QueuedPrompt
---@field text string User-authored prompt text without visible context markers.

---@class louiselm.ui.Chat
---@field api louiselm.session.Api Session API used to create sessions.
---@field agents string[] Agent names for the picker.
---@field skills louiselm.skills.Skill[] Skills for the invocation picker.
---@field skill_paths string[] Configured roots rediscovered when the invocation picker opens.
---@field skill_warning_signature string? Last reported discovery diagnostics, for notification deduplication.
---@field initial_contexts louiselm.ui.ContextItem[] Context queued for every new session.
---@field skill_catalog? string Hidden catalog copied only into brand-new inject sessions.
---@field instructions_context? louiselm.ui.ContextItem Project instructions resource link queued only for brand-new sessions.
---@field diff louiselm.ui.Diff File-edit review UI.
---@field usage louiselm.workflow.Usage Persistent measured usage ledger.
---@field decision_active? louiselm.ui.ChatDecision Permission decision currently presented.
---@field decision_queue louiselm.ui.ChatDecision[] Permission decisions waiting for the open one.
---@field queue_namespace integer Extmark namespace for queued prompt indicators.
---@field prompt_namespace integer Extmark namespace for prompt boundaries.
---@field header_namespace integer Highlight namespace for session diagnostics.
---@field views table<string, louiselm.ui.ChatView> Views by local session id.
---@field tool_inspect_windows table<integer, boolean> Floating raw-payload windows owned by this chat.
---@field winbars table<integer, string> Previous window bars by window id.
---@field current_id string? Currently displayed session id.
---@field handoffs table<integer, louiselm.ui.Handoff>
---@field disposed boolean Whether the chat UI has been disposed.
---@field attach fun(self: louiselm.ui.Chat, session: louiselm.session.Session): boolean, string? Attach or focus a session.
---@field buffer fun(self: louiselm.ui.Chat, session_id?: string): integer? Return a session buffer.
---@field inspect_tool fun(self: louiselm.ui.Chat): boolean, string? Open the raw payload under the cursor.
---@field switch fun(self: louiselm.ui.Chat, session_id: string): boolean, string? Focus an attached session.
---@field switch_session fun(self: louiselm.ui.Chat): boolean, string? Pick an attached session and focus it.
---@field close_session fun(self: louiselm.ui.Chat): boolean, string? Close the current session, confirming when active.
---@field should_block_quit fun(self: louiselm.ui.Chat): boolean Whether a last-window quit would abandon multiple sessions.
---@field cancel fun(self: louiselm.ui.Chat): boolean, string? Cancel the current session turn.
---@field session_options fun(self: louiselm.ui.Chat): boolean, string? Open the current session options overview.
---@field manage_permissions fun(self: louiselm.ui.Chat): boolean, string? Inspect and revoke remembered permission rules.
---@field rename_session fun(self: louiselm.ui.Chat, name: string): boolean, string? Rename the current session.
---@field session_id fun(self: louiselm.ui.Chat): string?, string? Return the current agent-scoped ACP session identifier.
---@field to_markdown fun(self: louiselm.ui.Chat, session_id?: string, path?: string): string?, string? Export a session's full transcript to a markdown file.
---@field open_handoff fun(self: louiselm.ui.Chat, target_session: louiselm.session.Session, source_session_id?: string): integer?, string? Open an editable transcript for a target session.
---@field submit_handoff fun(self: louiselm.ui.Chat, buffer: integer): boolean, string? Submit and close a handoff buffer.
---@field abandon_handoff fun(self: louiselm.ui.Chat, buffer: integer): boolean, string? Close a handoff buffer without sending it.
---@field set_config_option fun(self: louiselm.ui.Chat, id: string, value: string|boolean, callback?: fun(options: louiselm.session.ConfigOption[]?, error?: string)): string|number?, string? Change an idle session option.
---@field submit fun(self: louiselm.ui.Chat, text?: string): string|number|boolean?, string? Submit or queue the current prompt.
---@field queue_context fun(self: louiselm.ui.Chat, item: louiselm.ui.ContextItem): boolean, string? Queue context for the next prompt.
---@field mention_buffer fun(self: louiselm.ui.Chat): boolean, string? Queue the source buffer context.
---@field send_selection fun(self: louiselm.ui.Chat): boolean, string? Queue the source visual selection.
---@field pick_file fun(self: louiselm.ui.Chat, root?: string): boolean, string? Pick and queue a file context.
---@field pick_skill fun(self: louiselm.ui.Chat): boolean, string? Pick and queue a skill invocation.
---@field new_session fun(self: louiselm.ui.Chat, agent_name?: string, options?: louiselm.session.Options): louiselm.session.Session?, string? Create a session, using the picker when needed.
---@field hand_off fun(self: louiselm.ui.Chat): boolean, string? Hand the current session's reviewed transcript off to another configured agent.
---@field resume_session fun(self: louiselm.ui.Chat, all_workspaces?: boolean): boolean, string? Discover and load a prior ACP session.
---@field dispose fun(self: louiselm.ui.Chat): boolean Dispose buffers and listeners.

local M = {}
local Chat = {}
Chat.__index = Chat

---@class louiselm.ui.Handoff
---@field target_session louiselm.session.Session
---@field target_session_id string

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
    skills[index] = {
      name = skill.name,
      description = skill.description,
      path = skill.path,
      explicit_only = skill.explicit_only == true,
    }
  end
  for key in pairs(value) do
    if type(key) ~= "number" or key < 1 or key > #value or key % 1 ~= 0 then
      return nil, "chat skills must be a dense skill[]"
    end
  end
  return skills
end

---@param item unknown
---@return boolean valid
local function is_context_item(item)
  return type(item) == "table"
    and type(item.label) == "string"
    and (type(item.text) == "string" or type(item.uri) == "string")
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
    if not is_context_item(item) then
      return nil, string.format("chat initial context at index %d is malformed", index)
    end
    contexts[index] = { label = item.label, text = item.text, uri = item.uri }
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

---Return a human-readable label for an option's current value.
---For select options, resolves the wire value to its ConfigValue name when available.
---@param option louiselm.session.ConfigOption
---@return string
local function option_display_value(option)
  if option.type == "select" and type(option.options) == "table" then
    for _, v in ipairs(option.options) do
      if v.value == option.current_value then
        return v.name
      end
    end
  end
  return tostring(option.current_value)
end

-- A select provider closes the picker a new one replaces, which would answer an open
-- permission request without a choice. Commands that open their own picker refuse while
-- a decision is presented instead of queueing behind it: a decision can open a nested
-- picker of its own, and the way out of a stuck decision must never be queued behind it.
local DECISION_OPEN_ERROR = "a louiselm permission decision is open; answer it first"
local HEADER_LINE_COUNT = 4
local ACP_HIGHLIGHT = "LouiselmAcpValue"
local DERIVED_HIGHLIGHT = "LouiselmDerivedValue"

local STATUS_HIGHLIGHTS = {
  ready = "LouiselmStatusReady",
  prompting = "LouiselmStatusActive",
  configuring = "LouiselmStatusActive",
  starting = "LouiselmStatusActive",
  waiting_permission = "LouiselmStatusWarning",
  cancelling = "LouiselmStatusWarning",
  error = "LouiselmStatusError",
  disposed = "LouiselmStatusWarning",
}

local DEFAULT_HIGHLIGHTS = {
  [ACP_HIGHLIGHT] = "Identifier",
  [DERIVED_HIGHLIGHT] = "Number",
  LouiselmStatusReady = "DiagnosticOk",
  LouiselmStatusActive = "DiagnosticInfo",
  LouiselmStatusWarning = "DiagnosticWarn",
  LouiselmStatusError = "DiagnosticError",
}

local function setup_highlights()
  for name, link in pairs(DEFAULT_HIGHLIGHTS) do
    nvim.api.nvim_set_hl(0, name, { default = true, link = link })
  end
end

---@param self louiselm.ui.Chat
---@param buffer integer
local function close_handoff(self, buffer)
  self.handoffs[buffer] = nil
  if nvim.api.nvim_buf_is_valid(buffer) then
    nvim.api.nvim_buf_delete(buffer, { force = true })
  end
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

---@param value string
---@return string
local function statusline_escape(value)
  return (single_line(value):gsub("%%", function()
    return "%%"
  end))
end

---@param state louiselm.session.State
---@return string
local function session_identity(state)
  local parts = { state.id }
  if state.name ~= nil and state.name ~= state.id then
    parts[#parts + 1] = state.name
  end
  parts[#parts + 1] = state.status or "unknown"
  if state.source == "loaded" then
    parts[#parts + 1] = "loaded"
  end
  if state.skills_policy ~= nil then
    parts[#parts + 1] = "skills: " .. state.skills_policy
  end
  parts[#parts + 1] = state.acp_session_id ~= nil and report_id(state.agent, state.acp_session_id) or state.agent
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
    parts[#parts + 1] = option.name .. "=" .. option_display_value(option)
  end
  if state.context ~= nil then
    local stale = state.context.stale and " stale" or ""
    parts[#parts + 1] = string.format(
      "context=%s/%s (%.0f%%%s)",
      format_number(state.context.used),
      format_number(state.context.size),
      state.context.percentage,
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
---@return string raw
---@return string derived
local function context_display(state)
  local context = state.context
  if context == nil then
    return "", ""
  end
  local raw = "context=" .. format_number(context.used) .. "/" .. format_number(context.size)
  local derived = string.format("%.0f%%", context.percentage)
  if context.stale then
    derived = derived .. " stale"
  end
  return raw, derived
end

---@param state louiselm.session.State
---@return string?
local function cost_display(state)
  if state.cost == nil then
    return nil
  end
  return "cost=" .. format_number(state.cost.amount) .. " " .. single_line(state.cost.currency)
end

---@param state louiselm.session.State
---@return string
local function header_identity(state)
  local agent = single_line(state.agent)
  local parts = {
    state.acp_session_id and report_id(agent, single_line(state.acp_session_id)) or agent,
  }
  if state.name ~= nil and state.name ~= state.id then
    parts[#parts + 1] = single_line(state.name)
  end
  parts[#parts + 1] = single_line(state.id)
  return "# " .. table.concat(parts, " · ")
end

---@class louiselm.ui.HeaderHighlight
---@field line integer Zero-based header line.
---@field start_col integer Zero-based byte column.
---@field end_col integer Exclusive zero-based byte column.
---@field group string Highlight group name.

---@param highlights louiselm.ui.HeaderHighlight[]
---@param line integer
---@param start_col integer
---@param text string
---@param group string
local function add_header_highlight(highlights, line, start_col, text, group)
  highlights[#highlights + 1] = {
    line = line,
    start_col = start_col,
    end_col = start_col + #text,
    group = group,
  }
end

---@param state louiselm.session.State
---@return string[] lines
---@return louiselm.ui.HeaderHighlight[] highlights
local function session_header(state)
  local highlights = {}
  local session_parts = {
    "status=" .. tostring(state.status or "unknown"),
    "display=" .. turn_label(state),
  }
  if state.skills_policy ~= nil then
    session_parts[#session_parts + 1] = "skills=" .. state.skills_policy
  end
  if state.source == "loaded" then
    session_parts[#session_parts + 1] = "source=loaded"
  end
  local session_line = "Session: " .. table.concat(session_parts, " · ")
  local display_text = "display=" .. turn_label(state)
  local display_start = assert(session_line:find(display_text, 1, true)) - 1
  add_header_highlight(
    highlights,
    1,
    display_start,
    display_text,
    STATUS_HIGHLIGHTS[state.status] or "LouiselmStatusWarning"
  )

  local options_line = "ACP options:"
  for index, option in ipairs(state.config_options or {}) do
    options_line = options_line .. (index == 1 and " " or " · ")
    local option_text = single_line(option.name) .. "=" .. single_line(option_display_value(option))
    local start_col = #options_line
    options_line = options_line .. option_text
    add_header_highlight(highlights, 2, start_col, option_text, ACP_HIGHLIGHT)
  end

  local telemetry_line = "Telemetry:"
  local raw_context, derived_context = context_display(state)
  if raw_context ~= "" then
    telemetry_line = telemetry_line .. " "
    local raw_start = #telemetry_line
    telemetry_line = telemetry_line .. raw_context
    add_header_highlight(highlights, 3, raw_start, raw_context, ACP_HIGHLIGHT)
    telemetry_line = telemetry_line .. " ("
    local derived_start = #telemetry_line
    telemetry_line = telemetry_line .. derived_context
    add_header_highlight(highlights, 3, derived_start, derived_context, DERIVED_HIGHLIGHT)
    telemetry_line = telemetry_line .. ")"
  end
  local cost = cost_display(state)
  if cost ~= nil then
    telemetry_line = telemetry_line .. (raw_context == "" and " " or " · ")
    local cost_start = #telemetry_line
    telemetry_line = telemetry_line .. cost
    add_header_highlight(highlights, 3, cost_start, cost, ACP_HIGHLIGHT)
  end

  return { header_identity(state), session_line, options_line, telemetry_line }, highlights
end

---@param group string
---@param text string
---@return string
local function winbar_segment(group, text)
  return "%#" .. group .. "#" .. statusline_escape(text) .. "%*"
end

---@param state louiselm.session.State
---@return string
local function session_winbar(state)
  local fields = {
    winbar_segment(STATUS_HIGHLIGHTS[state.status] or "LouiselmStatusWarning", turn_label(state)),
  }
  local raw_context, derived_context = context_display(state)
  if raw_context ~= "" then
    fields[#fields + 1] = winbar_segment(ACP_HIGHLIGHT, raw_context)
      .. " ("
      .. winbar_segment(DERIVED_HIGHLIGHT, derived_context)
      .. ")"
  end
  local cost = cost_display(state)
  if cost ~= nil then
    fields[#fields + 1] = winbar_segment(ACP_HIGHLIGHT, cost)
  end
  return table.concat(fields, " · ")
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
  nvim.api.nvim_set_option_value("winbar", session_winbar(view.session:inspect()), { win = win })
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

---@param self louiselm.ui.Chat
---@param view louiselm.ui.ChatView
local function render_header(self, view)
  local lines, highlights = session_header(view.session:inspect())
  nvim.api.nvim_buf_set_lines(view.buffer, 0, HEADER_LINE_COUNT, false, lines)
  nvim.api.nvim_buf_clear_namespace(view.buffer, self.header_namespace, 0, HEADER_LINE_COUNT)
  for _, highlight in ipairs(highlights) do
    nvim.api.nvim_buf_add_highlight(
      view.buffer,
      self.header_namespace,
      highlight.group,
      highlight.line,
      highlight.start_col,
      highlight.end_col
    )
  end
end

---Render one context chip in the visible prompt prefix without touching queued content.
---@param self louiselm.ui.Chat
---@param view louiselm.ui.ChatView
---@param label string
---@return boolean rendered
---@return string? error_message
local function render_chip(self, view, label)
  if self.disposed or self.views[view.session:inspect().id] ~= view then
    return false, "chat UI is disposed"
  end
  local line = nvim.api.nvim_buf_get_lines(view.buffer, view.prompt_line, view.prompt_line + 1, false)[1] or "> "
  local text = line:sub(1, 2) == "> " and line:sub(3) or line
  if view.context_prefix ~= "" and text:sub(1, #view.context_prefix) == view.context_prefix then
    text = text:sub(#view.context_prefix + 1)
  end
  view.context_prefix = view.context_prefix .. "[context: " .. label .. "] "
  set_line(view.buffer, view.prompt_line, "> " .. view.context_prefix .. text)
  if nvim.api.nvim_get_current_buf() == view.buffer then
    nvim.api.nvim_win_set_cursor(0, { view.prompt_line + 1, 2 + #view.context_prefix })
  end
  return true
end

---@param self louiselm.ui.Chat
---@param view louiselm.ui.ChatView
---@param item louiselm.ui.ContextItem
---@return boolean queued
---@return string? error_message
local function queue_context(self, view, item)
  if not is_context_item(item) then
    return false, "context item must contain a label and a text or uri string"
  end
  local rendered, render_error = render_chip(self, view, item.label)
  if not rendered then
    return false, render_error
  end
  view.contexts[#view.contexts + 1] = { label = item.label, text = item.text, uri = item.uri }
  return true
end

---Queue a native-mode skill selection, resolved against advertised commands only at submission.
---@param self louiselm.ui.Chat
---@param view louiselm.ui.ChatView
---@param skill louiselm.skills.Skill
---@return boolean queued
---@return string? error_message
local function queue_native_skill(self, view, skill)
  local rendered, render_error = render_chip(self, view, "skill: " .. skill.name)
  if not rendered then
    return false, render_error
  end
  view.pending_skill = skill
  return true
end

---Queue one inject-mode skill body in context order, retaining a failed read for retry.
---@param self louiselm.ui.Chat
---@param view louiselm.ui.ChatView
---@param skill louiselm.skills.Skill
---@return boolean queued
---@return string? error_message
local function queue_injected_skill(self, view, skill)
  local rendered, render_error = render_chip(self, view, "skill: " .. skill.name)
  if not rendered then
    return false, render_error
  end
  view.contexts[#view.contexts + 1] = {
    label = "skill: " .. skill.name,
    text = skill.content,
    skill_path = skill.path,
  }
  if skill.content == nil then
    return true, "could not read selected skill: " .. skill.path
  end
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

---@class louiselm.ui.ContextFold
---@field first integer Zero-based first folded line.
---@field last integer Zero-based last folded line.

---@class louiselm.ui.ToolFoldRun
---@field first integer Zero-based first rendered tool line.
---@field last integer Zero-based last rendered tool line.

---@class louiselm.ui.ToolFold
---@field first integer Zero-based first folded line.
---@field last integer Zero-based last folded line.

---@param item louiselm.ui.ContextItem
---@return table block
local function context_content(item)
  if item.uri ~= nil then
    return { type = "resource_link", uri = item.uri, name = item.label }
  end
  return { type = "text", text = item.text }
end

---@param view louiselm.ui.ChatView
---@param win integer
local function apply_context_folds(view, win)
  if not nvim.api.nvim_win_is_valid(win) or nvim.api.nvim_win_get_buf(win) ~= view.buffer then
    return
  end
  nvim.api.nvim_set_option_value("foldmethod", "manual", { win = win })
  nvim.api.nvim_set_option_value("foldenable", true, { win = win })
  local applied = view.fold_counts[win] or 0
  nvim.api.nvim_win_call(win, function()
    for index = applied + 1, #view.context_folds do
      local fold = view.context_folds[index]
      nvim.api.nvim_cmd({ cmd = "fold", range = { fold.first + 1, fold.last + 1 } }, {})
    end
  end)
  view.fold_counts[win] = #view.context_folds
end

---@param view louiselm.ui.ChatView
---@param win integer
local function apply_tool_folds(view, win)
  if not nvim.api.nvim_win_is_valid(win) or nvim.api.nvim_win_get_buf(win) ~= view.buffer then
    return
  end
  nvim.api.nvim_set_option_value("foldmethod", "manual", { win = win })
  nvim.api.nvim_set_option_value("foldenable", true, { win = win })
  local applied = view.tool_fold_counts[win] or 0
  nvim.api.nvim_win_call(win, function()
    for index = applied + 1, #view.tool_folds do
      local fold = view.tool_folds[index]
      nvim.api.nvim_cmd({ cmd = "fold", range = { fold.first + 1, fold.last + 1 } }, {})
    end
  end)
  view.tool_fold_counts[win] = #view.tool_folds
end

---@param view louiselm.ui.ChatView
local function close_tool_fold_run(view)
  local run = view.tool_fold_run
  if run == nil then
    return
  end
  local fold_first
  local added = false
  for line = run.first, run.last + 1 do
    local id = view.tool_ids[line]
    if id ~= nil and view.tool_statuses[id] == "completed" then
      fold_first = fold_first or line
    elseif fold_first ~= nil then
      if line - fold_first > 1 then
        view.tool_folds[#view.tool_folds + 1] = { first = fold_first, last = line - 1 }
        added = true
      end
      fold_first = nil
    end
  end
  if added then
    apply_tool_folds(view, view.window)
  end
  view.tool_fold_run = nil
end

---@param view louiselm.ui.ChatView
---@param line integer Zero-based rendered tool line.
local function record_tool_line(view, line)
  local run = view.tool_fold_run
  if run ~= nil and line ~= run.last + 1 then
    close_tool_fold_run(view)
    run = nil
  end
  if run == nil then
    view.tool_fold_run = { first = line, last = line }
  else
    run.last = line
  end
end

---@param view louiselm.ui.ChatView
---@param id string
---@return louiselm.session.TranscriptEntry? entry
local function transcript_tool(view, id)
  for _, entry in ipairs(view.transcript:snapshot()) do
    if entry.kind == "tool_call" and entry.id == id then
      return entry
    end
  end
  return nil
end

---@param self louiselm.ui.Chat
---@param view louiselm.ui.ChatView
---@param id string
---@return boolean opened
---@return string? error_message
local function open_tool_inspector(self, view, id)
  local entry = transcript_tool(view, id)
  if entry == nil then
    return false, "tool-call payload is unavailable"
  end
  local lines = split_lines(nvim.inspect(entry.raw or {}))
  local buffer = nvim.api.nvim_create_buf(false, true)
  nvim.api.nvim_set_option_value("buftype", "nofile", { buf = buffer })
  nvim.api.nvim_set_option_value("bufhidden", "wipe", { buf = buffer })
  nvim.api.nvim_set_option_value("swapfile", false, { buf = buffer })
  nvim.api.nvim_buf_set_lines(buffer, 0, -1, false, lines)
  nvim.api.nvim_set_option_value("modifiable", false, { buf = buffer })
  local width = math.min(100, math.max(1, nvim.o.columns - 4))
  local height = math.min(#lines, math.max(1, nvim.o.lines - 4))
  local window = nvim.api.nvim_open_win(buffer, true, {
    relative = "editor",
    row = 1,
    col = 2,
    width = width,
    height = height,
    style = "minimal",
    border = "rounded",
  })
  self.tool_inspect_windows[window] = true
  return true
end

---@param view louiselm.ui.ChatView
---@param text string
---@param contexts louiselm.ui.ContextItem[]
---@return integer line_count
local function replace_submitted_prompt(view, text, contexts)
  local lines = {}
  if #contexts > 0 then
    local labels = {}
    for index, item in ipairs(contexts) do
      labels[index] = single_line(item.label)
    end
    lines[1] = "> [contexts: " .. table.concat(labels, " · ") .. "]"
    for _, item in ipairs(contexts) do
      lines[#lines + 1] = "[context: " .. single_line(item.label) .. "]"
      if item.text ~= nil then
        nvim.list_extend(lines, split_lines(item.text))
      else
        local block = context_content(item)
        lines[#lines + 1] = "type: " .. block.type
        lines[#lines + 1] = "name: " .. block.name
        lines[#lines + 1] = "uri: " .. block.uri
      end
    end
    view.context_folds[#view.context_folds + 1] = {
      first = view.prompt_line,
      last = view.prompt_line + #lines - 1,
    }
  end
  for _, line in ipairs(split_lines(text)) do
    lines[#lines + 1] = "> " .. line
  end
  nvim.api.nvim_buf_set_lines(view.buffer, view.prompt_line, -1, false, lines)
  apply_context_folds(view, view.window)
  return #lines
end

---@param view louiselm.ui.ChatView
---@param line integer
local function mark_prompt(view, line)
  view.prompt_line = line
  view.prompt_mark = nvim.api.nvim_buf_set_extmark(view.buffer, view.prompt_namespace, line, 0, {
    id = view.prompt_mark,
    right_gravity = false,
  })
end

---@param view louiselm.ui.ChatView
---@return integer line
local function current_prompt_line(view)
  local position = nvim.api.nvim_buf_get_extmark_by_id(view.buffer, view.prompt_namespace, view.prompt_mark, {})
  if #position == 2 then
    view.prompt_line = position[1]
  end
  return view.prompt_line
end

---@param view louiselm.ui.ChatView
---@return string text
local function prompt_text(view)
  local lines = nvim.api.nvim_buf_get_lines(view.buffer, current_prompt_line(view), -1, false)
  for index, line in ipairs(lines) do
    lines[index] = line:sub(1, 2) == "> " and line:sub(3) or line
  end
  return table.concat(lines, "\n")
end

---@param view louiselm.ui.ChatView
---@param text string
---@return integer line_count
local function replace_prompt(view, text)
  local prompt_line = current_prompt_line(view)
  local lines = split_lines(text)
  for index, line in ipairs(lines) do
    lines[index] = "> " .. line
  end
  nvim.api.nvim_buf_set_lines(view.buffer, prompt_line, -1, false, lines)
  mark_prompt(view, prompt_line)
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
---@return boolean has_image Whether an ACP tool payload contains an image block.
local function tool_has_image(value)
  if type(value) ~= "table" then
    return false
  end
  if value.type == "image" then
    return true
  end
  for _, nested in pairs(value) do
    if tool_has_image(nested) then
      return true
    end
  end
  return false
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
  local ordered = {}
  local rejections = {}
  for _, option in ipairs(Gates.decision_options({ options = options }, "deny")) do
    ordered[#ordered + 1] = option
    rejections[option] = true
  end
  for _, option in ipairs(options) do
    if not rejections[option] then
      ordered[#ordered + 1] = option
    end
  end
  return ordered
end

---@param operation unknown
---@return string prompt
local function permission_prompt(operation)
  local kind = type(operation) == "table" and operation.kind or "unknown"
  if kind == "command" and type(operation.command) == "table" then
    local encoded_ok, encoded_command = pcall(nvim.json.encode, operation.command)
    if encoded_ok and type(encoded_command) == "string" then
      return "louiselm permission (command): " .. encoded_command .. " "
    end
  end
  if type(kind) ~= "string" or kind == "" then
    kind = "unknown"
  end
  return "louiselm permission (" .. kind .. ", details unavailable): "
end

---@param self louiselm.ui.Chat
---@param view louiselm.ui.ChatView
---@return boolean hosted Whether this view can still host a permission decision.
local function hosts_view(self, view)
  return not self.disposed and self.views[view.session:inspect().id] == view and nvim.api.nvim_buf_is_valid(view.buffer)
end

---@param self louiselm.ui.Chat
---@param view louiselm.ui.ChatView
---@param respond fun(result: unknown, error?: louiselm.acp.JsonRpcError): boolean, string?
---@param result table
---@return boolean sent
local function send_permission_response(self, view, respond, result)
  if not hosts_view(self, view) then
    -- A choice made after the view is gone must grant nothing, but the request still
    -- has to be answered or the agent blocks on it for the rest of the session.
    local closed_ok, closed = pcall(respond, { outcome = { outcome = "cancelled" } })
    return closed_ok and closed == true
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
---@param respond fun(result: unknown, error?: louiselm.acp.JsonRpcError): boolean, string? Decision responder.
local function prompt_permission(self, view, data, respond)
  local options = permission_options(data.options)
  if options == nil or #options == 0 then
    cancel_permission(self, view, respond)
    return
  end
  Picker.select(options, {
    prompt = permission_prompt(data.operation),
    format_item = function(option)
      local _, label = permission_option(option)
      return label
    end,
  }, function(choice)
    if choice == nil then
      cancel_permission(self, view, respond)
      return
    end
    local result = Gates.select_response(choice)
    if result == nil then
      cancel_permission(self, view, respond)
      return
    end
    send_permission_response(self, view, respond, result)
  end)
end

---@class louiselm.ui.ChatDecision
---@field view louiselm.ui.ChatView View whose session asked for a decision.
---@field data table ACP permission request data.
---@field respond fun(result: unknown, error?: louiselm.acp.JsonRpcError): boolean, string? ACP responder.
---@field answered boolean Whether this decision was already answered or cancelled.

---@type fun(self: louiselm.ui.Chat)
local pump_decisions

---Wrap one ACP responder so the decision slot is released exactly once, after the answer.
---The slot is compared by identity: a stale picker answering late must not release the
---decision that replaced it.
---@param self louiselm.ui.Chat
---@param decision louiselm.ui.ChatDecision Decision being opened.
---@return fun(result: unknown, error?: louiselm.acp.JsonRpcError): boolean, string? respond
local function decision_responder(self, decision)
  return function(result, rpc_error)
    if decision.answered then
      return false, "permission decision was already answered"
    end
    decision.answered = true
    local sent, send_error = decision.respond(result, rpc_error)
    if self.decision_active == decision then
      self.decision_active = nil
      pump_decisions(self)
    end
    return sent, send_error
  end
end

---Open one decision: a diff review for file edits, a picker for everything else.
---@param self louiselm.ui.Chat
---@param decision louiselm.ui.ChatDecision Decision to present.
local function open_decision(self, decision)
  local view = decision.view
  local data = decision.data
  local respond = decision_responder(self, decision)
  if not hosts_view(self, view) then
    cancel_permission(self, view, respond)
    return
  end
  if type(data.operation) == "table" and data.operation.kind == "file_edit" then
    local opened, open_error = self.diff:open(data, respond)
    if not opened then
      insert_transcript(self, view, { "Error: " .. (open_error or "could not open diff review") })
      cancel_permission(self, view, respond)
    end
    return
  end
  -- A select provider that throws would otherwise strand every later decision behind it.
  local call_ok, prompt_error = pcall(prompt_permission, self, view, data, respond)
  if not call_ok then
    insert_transcript(self, view, { "Error: permission picker failed: " .. tostring(prompt_error) })
    cancel_permission(self, view, respond)
  end
end

---Present the next queued decision while none is open.
---One picker or review hosts one decision at a time across every attached session,
---because an async select provider closes the picker a new one replaces, which answers
---that request without the user choosing and can leave the replacement unanswered.
---@param self louiselm.ui.Chat
pump_decisions = function(self)
  while self.decision_active == nil do
    local decision = table.remove(self.decision_queue, 1)
    if decision == nil then
      return
    end
    if decision.answered then
      -- Its turn was cancelled while it waited; the session already answered it.
    elseif self.disposed then
      -- Disposal answers what it can no longer host: an unanswered request blocks its agent.
      decision.answered = true
      cancel_permission(self, decision.view, decision.respond)
    else
      self.decision_active = decision
      open_decision(self, decision)
    end
  end
end

---Release the decisions whose requests the session already answered on cancellation.
---@param self louiselm.ui.Chat
---@param view louiselm.ui.ChatView View whose session cancelled.
---@param request_ids unknown Identifiers reported by the session.
local function cancel_decisions(self, view, request_ids)
  if type(request_ids) ~= "table" then
    return
  end
  local cancelled = {}
  for _, request_id in ipairs(request_ids) do
    cancelled[request_id] = true
  end
  for _, decision in ipairs(self.decision_queue) do
    if decision.view == view and cancelled[decision.data.request_id] then
      decision.answered = true
    end
  end
  local active = self.decision_active
  if active == nil or active.view ~= view or not cancelled[active.data.request_id] then
    return
  end
  active.answered = true
  self.decision_active = nil
  -- A review is ours to close; an open picker is not, and answering it later reports
  -- that the decision was already answered.
  self.diff:close()
  pump_decisions(self)
end

---Queue one permission decision and present it as soon as the chat is free.
---@param self louiselm.ui.Chat
---@param view louiselm.ui.ChatView View whose session asked for a decision.
---@param data table ACP permission request data.
---@param respond? fun(result: unknown, error?: louiselm.acp.JsonRpcError): boolean, string? ACP responder.
local function queue_decision(self, view, data, respond)
  if type(respond) ~= "function" then
    insert_transcript(self, view, { "Error: permission request has no response callback" })
    return
  end
  self.decision_queue[#self.decision_queue + 1] = { view = view, data = data, respond = respond, answered = false }
  pump_decisions(self)
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
  mark_prompt(view, view.prompt_line + #replacement)
end

---Build prompt content from the current context queue and a pending native skill selection.
---A pending skill is resolved here, against the caller's latest advertised commands, so that
---both an immediate submission and a queued-prompt release each see the freshest command cache
---rather than one snapshotted when the picker ran.
---@param view louiselm.ui.ChatView
---@param text string
---@return string|table? content
---@return string? error_message Native command resolution failure; content is unsent.
---@return louiselm.ui.ContextItem[] contexts Exact attached contexts in transport order.
local function build_content(view, text)
  if text:sub(1, 1) == "/" then
    return text, nil, {}
  end
  local final_text = text
  if view.pending_skill ~= nil then
    if view.pending_skill.content == nil then
      local content = Skills.read(view.pending_skill.path)
      if content == nil then
        return nil, "could not read selected skill: " .. view.pending_skill.path, {}
      end
      view.pending_skill.content = content
    end
    local command_name, resolve_error = Skills.resolve_command(view.pending_skill, view.session:inspect().commands)
    if command_name == nil then
      return nil, resolve_error, {}
    end
    final_text = final_text == "" and ("/" .. command_name) or ("/" .. command_name .. " " .. final_text)
  end
  if view.skill_catalog == nil and #view.contexts == 0 then
    return final_text, nil, {}
  end
  local content = {}
  local contexts = {}
  if view.skill_catalog ~= nil then
    local catalog = { label = "skill-index", text = view.skill_catalog }
    contexts[#contexts + 1] = catalog
    content[#content + 1] = context_content(catalog)
  end
  for _, item in ipairs(view.contexts) do
    if item.text == nil and item.skill_path ~= nil then
      local skill_content = Skills.read(item.skill_path)
      if skill_content == nil then
        return nil, "could not read selected skill: " .. item.skill_path, {}
      end
      item.text = skill_content
    end
    contexts[#contexts + 1] = item
    content[#content + 1] = context_content(item)
  end
  if final_text ~= "" then
    content[#content + 1] = { type = "text", text = final_text }
  end
  return content, nil, contexts
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
---@return string|number? request_id
---@return string? error_message
local function submit_prompt(self, view, text)
  local content, resolve_error, contexts = build_content(view, text)
  if content == nil then
    notify_prompt_error(resolve_error or "prompt could not be resolved")
    return nil, resolve_error
  end
  local state = view.session:inspect()
  local request_id, prompt_error = view.session:prompt(content)
  if request_id == nil then
    local message = prompt_error or "prompt failed"
    notify_prompt_error(message)
    return nil, message
  end
  view.cost_before = state.cost
  view.transcript:record_user(text)

  clear_queued_prompt(view)
  local slash_prompt = text:sub(1, 1) == "/"
  local prompt_line_count = replace_submitted_prompt(view, text, contexts)
  local response_line = view.prompt_line + prompt_line_count
  local next_prefix = slash_prompt and view.context_prefix or ""
  nvim.api.nvim_buf_set_lines(view.buffer, response_line, response_line, false, { "", "> " .. next_prefix })
  view.response_line = response_line
  view.response_tail = view.response_line
  view.response_started = false
  view.last_block_kind = nil
  view.transcript_tail = view.response_tail
  mark_prompt(view, response_line + 1)
  if nvim.api.nvim_get_current_buf() == view.buffer then
    nvim.api.nvim_win_set_cursor(0, { view.prompt_line + 1, 2 + #next_prefix })
  end
  if not slash_prompt then
    view.contexts = {}
    view.context_prefix = ""
    view.skill_catalog = nil
    view.pending_skill = nil
  end
  return request_id
end

---@param self louiselm.ui.Chat
---@param view louiselm.ui.ChatView
---@param text string
local function queue_prompt(self, view, text)
  clear_queued_prompt(view)
  set_prompt_line(view, text)
  view.queued_prompt = { text = text }
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
  submit_prompt(self, view, queued.text)
end

---@param option louiselm.session.ConfigOption
---@return table[] values
local function config_values(option)
  if option.type == "boolean" then
    return { { value = true, name = "true" }, { value = false, name = "false" } }
  end
  return option.options or {}
end

---@param summary louiselm.workflow.UsageSummary
---@return string
local function usage_details(summary)
  local details = {}
  if summary.average_tokens ~= nil then
    details[#details + 1] = format_number(summary.average_tokens) .. " tokens/turn"
  end
  for _, cost in ipairs(summary.costs) do
    details[#details + 1] = format_number(cost.average) .. " " .. cost.currency .. "/turn"
  end
  if #details == 0 then
    return ""
  end
  return " (observed: " .. table.concat(details, " · ") .. " · " .. summary.samples .. " samples)"
end

---@param self louiselm.ui.Chat
---@param state louiselm.session.State
---@param option_id string
---@param value string|boolean
---@return string
local function option_usage_details(self, state, option_id, value)
  local summary = self.usage:summary(state.agent, option_id, value)
  if summary == nil then
    return ""
  end
  return usage_details(summary)
end

---@param self louiselm.ui.Chat
---@param view louiselm.ui.ChatView
---@param initial boolean
open_session_options = function(self, view, initial)
  if self.disposed or self.views[view.session:inspect().id] ~= view or self.decision_active then
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
  Picker.select(state.config_options, {
    prompt = "louiselm session options: ",
    format_item = function(option)
      return option.name .. ": " .. option_display_value(option)
    end,
  }, function(option)
    if option == nil or self.disposed or self.views[state.id] ~= view then
      return
    end
    Picker.select(config_values(option), {
      prompt = option.name .. ": ",
      format_item = function(value)
        return value.name .. option_usage_details(self, state, option.id, value.value)
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
            render_header(self, view)
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
  if self.disposed or self.views[event.session_id] ~= view or not nvim.api.nvim_buf_is_valid(view.buffer) then
    if event.type == "permission_requested" and type(event.respond) == "function" then
      -- Scheduled before teardown, delivered after it: no view is left to host the choice,
      -- and no other consumer will answer. The result has nowhere left to be reported.
      pcall(event.respond, { outcome = { outcome = "cancelled" } })
    end
    return
  end

  if event.type == "state_changed" or event.type == "config_options_changed" or event.type == "usage_updated" then
    render_header(self, view)
    render_winbar(self, view, view.window)
  end
  if event.type == "state_changed" and view.session:inspect().status == "ready" then
    open_session_options(self, view, true)
    release_queued_prompt(self, view)
  end

  if event.type == "user_chunk" then
    local text = chunk_text(event.data)
    if text == nil then
      return
    end
    local lines = split_lines(text)
    for index, line in ipairs(lines) do
      lines[index] = "> " .. line
    end
    lines[#lines + 1] = ""
    insert_transcript(self, view, lines)
    view.response_line = nil
    view.response_tail = nil
    view.response_started = false
    view.last_block_kind = nil
  elseif event.type == "chunk" then
    local text = chunk_text(event.data)
    if text == nil then
      return
    end
    close_tool_fold_run(view)
    if not view.response_started then
      local lines = split_lines(text)
      local insertion_line = view.response_tail
        or (view.transcript_tail == nil and view.prompt_line or view.transcript_tail + 1)
      if view.response_tail ~= nil then
        insertion_line = insertion_line + 1
      end
      local separator = 0
      if view.last_block_kind == "tool" then
        nvim.api.nvim_buf_set_lines(view.buffer, insertion_line, insertion_line, false, { "" })
        insertion_line = insertion_line + 1
        separator = 1
      end
      local response_line_count = #lines
      if view.last_block_kind ~= "tool" then
        lines[#lines + 1] = ""
      end
      nvim.api.nvim_buf_set_lines(view.buffer, insertion_line, insertion_line, false, lines)
      view.response_line = insertion_line
      view.response_tail = insertion_line + response_line_count - 1
      view.transcript_tail = view.response_tail
      mark_prompt(view, view.prompt_line + separator + #lines)
      view.response_started = true
      view.last_block_kind = "prose"
      return
    end
    local current = nvim.api.nvim_buf_get_lines(view.buffer, view.response_tail, view.response_tail + 1, false)[1] or ""
    local lines = split_lines(current .. text)
    nvim.api.nvim_buf_set_lines(view.buffer, view.response_tail, view.response_tail + 1, false, lines)
    local added = #lines - 1
    view.response_tail = view.response_tail + added
    view.transcript_tail = view.response_tail
    mark_prompt(view, view.prompt_line + added)
  elseif event.type == "tool_call_started" or event.type == "tool_call_finished" then
    local status = field(event.data, "status")
    local id = tool_id(event.data)
    local title = field(event.data, "title")
    if title ~= nil then
      title = single_line(title)
    end
    if event.type == "tool_call_started" then
      view.tool_statuses[id] = status or "started"
      if title ~= nil then
        view.tool_titles[id] = title
      end
      local detail = id
      if title ~= nil then
        detail = detail .. ": " .. title
      end
      local lines = { "[tool] " .. detail .. " (started)" }
      if view.last_block_kind == "prose" then
        table.insert(lines, 1, "")
      end
      insert_transcript(self, view, lines)
      view.tool_lines[id] = view.transcript_tail
      view.tool_ids[view.transcript_tail] = id
      record_tool_line(view, view.transcript_tail)
      view.last_block_kind = "tool"
    else
      title = title or view.tool_titles[id]
      local detail = id
      if title ~= nil then
        detail = detail .. ": " .. title
      end
      detail = detail .. " (" .. (status or "finished") .. ")"
      if status == "completed" and tool_has_image(event.data) then
        detail = detail .. " · image result — use :LouiselmInspectTool"
      end
      local line = view.tool_lines[id]
      local rendered_line
      if line ~= nil and line < nvim.api.nvim_buf_line_count(view.buffer) then
        set_line(view.buffer, line, "[tool] " .. detail)
        rendered_line = line
      else
        local lines = { "[tool] " .. detail }
        if view.last_block_kind == "prose" then
          table.insert(lines, 1, "")
        end
        insert_transcript(self, view, lines)
        local inserted_line = view.transcript_tail
        if inserted_line ~= nil then
          rendered_line = inserted_line
          view.tool_ids[inserted_line] = id
          record_tool_line(view, inserted_line)
        end
        view.last_block_kind = "tool"
      end
      view.tool_lines[id] = nil
      view.tool_statuses[id] = status or "finished"
      view.tool_titles[id] = nil
      if rendered_line ~= nil then
        view.tool_ids[rendered_line] = id
      end
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
    view.last_block_kind = nil
  elseif event.type == "permission_requested" then
    local data = event.data
    if type(data) == "table" and type(data.permission_error) == "string" then
      insert_transcript(self, view, { "Warning: " .. data.permission_error })
    elseif type(data) == "table" and data.remembered_decision ~= nil then
      insert_transcript(self, view, { "Warning: remembered decision requires a compatible once-only option" })
    end
    if type(data) == "table" then
      queue_decision(self, view, data, event.respond)
    else
      cancel_permission(self, view, event.respond)
    end
  elseif event.type == "permission_cancelled" then
    cancel_decisions(self, view, type(event.data) == "table" and event.data.request_ids or nil)
  elseif event.type == "turn_done" then
    close_tool_fold_run(view)
    local state = view.session:inspect()
    local cost
    if
      view.cost_before ~= nil
      and state.cost ~= nil
      and view.cost_before.currency == state.cost.currency
      and state.cost.amount >= view.cost_before.amount
    then
      cost = { amount = state.cost.amount - view.cost_before.amount, currency = state.cost.currency }
    end
    view.cost_before = nil
    local recorded, usage_error = self.usage:record(state.agent, state.config_options, state.usage, cost)
    if not recorded then
      insert_transcript(self, view, { "Error: " .. (usage_error or "could not record measured usage") })
    end
    local line = usage_line(state.usage)
    if line ~= nil then
      insert_transcript(self, view, { line })
    end
    view.response_line = nil
    view.response_tail = nil
    view.response_started = false
    view.last_block_kind = nil
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
  if self.decision_active then
    return false, DECISION_OPEN_ERROR
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
  Picker.select(rules, {
    prompt = "louiselm remembered permissions: ",
    format_item = permission_rule_label,
  }, function(rule)
    if rule == nil or self.disposed then
      return
    end
    Picker.select({ "Revoke", "Keep" }, { prompt = "revoke " .. rule.id .. "? " }, function(choice)
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
        and key ~= "skill_catalog"
        and key ~= "instructions_context"
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
  local skill_catalog = options and options.skill_catalog
  if skill_catalog ~= nil and (type(skill_catalog) ~= "string" or skill_catalog == "") then
    return nil, "chat skill catalog must be a non-empty string"
  end
  local instructions_contexts, instructions_context_error =
    copy_initial_contexts(options and options.instructions_context and { options.instructions_context } or nil)
  if instructions_contexts == nil then
    return nil, instructions_context_error
  end
  local usage, usage_error = Usage.new()
  if usage == nil then
    return nil, usage_error
  end
  setup_highlights()
  local chat = setmetatable({
    api = api,
    agents = agents,
    skills = skills,
    skill_paths = skill_paths,
    skill_warning_signature = nil,
    initial_contexts = initial_contexts,
    skill_catalog = skill_catalog,
    instructions_context = instructions_contexts[1],
    usage = usage,
    diff = Diff.new(),
    decision_queue = {},
    queue_namespace = nvim.api.nvim_create_namespace("louiselm.chat.queued_prompt"),
    prompt_namespace = nvim.api.nvim_create_namespace("louiselm.chat.prompt"),
    header_namespace = nvim.api.nvim_create_namespace("louiselm.chat.header"),
    views = {},
    tool_inspect_windows = {},
    winbars = {},
    handoffs = {},
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
  -- Not the literal "markdown": that filetype is what third-party
  -- filetype-keyed integrations (image.nvim's markdown integration, at
  -- least) key off of, and they cannot tell this live, ever-growing
  -- transcript apart from a real markdown file a user is editing -- causing
  -- e.g. a full buffer re-parse on every keystroke looking for images that
  -- will never exist here. Highlighting is attached explicitly below,
  -- decoupled from `filetype`, so this buffer opts back into only what it
  -- actually wants.
  nvim.api.nvim_set_option_value("filetype", "louiselm-session", { buf = buffer })
  nvim.treesitter.start(buffer, "markdown")
  local header = session_header(state)
  local initial_lines = nvim.list_extend(header, { "", "> " })
  nvim.api.nvim_buf_set_lines(buffer, 0, -1, false, initial_lines)

  local view = {
    session = session,
    buffer = buffer,
    window = window,
    source_buffer = source_buffer,
    prompt_line = HEADER_LINE_COUNT + 1,
    transcript_tail = nil,
    response_line = nil,
    response_tail = nil,
    response_started = false,
    last_block_kind = nil,
    tool_lines = {},
    tool_ids = {},
    tool_statuses = {},
    tool_titles = {},
    contexts = {},
    context_prefix = "",
    skill_catalog = nil,
    pending_skill = nil,
    context_folds = {},
    fold_counts = {},
    tool_folds = {},
    tool_fold_counts = {},
    tool_fold_run = nil,
    queued_prompt = nil,
    queue_mark = nil,
    queue_namespace = self.queue_namespace,
    prompt_namespace = self.prompt_namespace,
    setup_shown = false,
    cost_before = nil,
    transcript = Transcript.new(),
    unsubscribe = function() end,
  }
  mark_prompt(view, view.prompt_line)
  nvim.api.nvim_buf_attach(buffer, false, {
    on_lines = function(_, _, _, first_line, last_line)
      if view.queued_prompt ~= nil and first_line <= view.prompt_line and last_line > view.prompt_line then
        clear_queued_prompt(view)
      end
    end,
  })
  view.unsubscribe = session:on(function(event)
    -- Recording is a pure data transform, not an editor/UI operation, so it can run
    -- directly in this fast-event callback instead of waiting for the scheduled turn.
    view.transcript:record(event)
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
  render_header(self, view)
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

---Open a transcript review buffer for another session.
---@param self louiselm.ui.Chat
---@param target_session louiselm.session.Session Session that will receive the reviewed prompt.
---@param source_session_id? string Attached source session; defaults to the current session.
---@return integer? buffer
---@return string? error_message Validation or session state error.
function Chat:open_handoff(target_session, source_session_id)
  if self.disposed then
    return nil, "chat UI is disposed"
  end
  if
    type(target_session) ~= "table"
    or type(target_session.inspect) ~= "function"
    or type(target_session.prompt) ~= "function"
  then
    return nil, "handoff requires a target session"
  end
  local source_id = source_session_id or self.current_id
  local source_view = source_id and self.views[source_id]
  if source_view == nil then
    return nil, "no source chat session is attached"
  end
  local target_state = target_session:inspect()
  if type(target_state) ~= "table" or type(target_state.id) ~= "string" then
    return nil, "target session has invalid state"
  end
  if target_state.id == source_id then
    return nil, "handoff target must differ from source session"
  end

  local buffer = nvim.api.nvim_create_buf(false, true)
  nvim.api.nvim_buf_set_name(buffer, "louiselm://handoff-" .. source_id .. "-" .. target_state.id)
  nvim.api.nvim_set_option_value("buftype", "nofile", { buf = buffer })
  nvim.api.nvim_set_option_value("bufhidden", "wipe", { buf = buffer })
  nvim.api.nvim_set_option_value("swapfile", false, { buf = buffer })
  nvim.api.nvim_set_option_value("filetype", "markdown", { buf = buffer })
  local markdown = Transcript.render(source_view.transcript:snapshot(), source_view.session:inspect())
  nvim.api.nvim_buf_set_lines(buffer, 0, -1, false, nvim.split(markdown, "\n", { plain = true }))
  self.handoffs[buffer] = { target_session = target_session, target_session_id = target_state.id }
  nvim.api.nvim_set_current_buf(buffer)
  nvim.keymap.set("n", "<C-s>", function()
    self:submit_handoff(buffer)
  end, { buffer = buffer, silent = true, desc = "Submit louiselm handoff" })
  nvim.keymap.set("n", "q", function()
    self:abandon_handoff(buffer)
  end, { buffer = buffer, silent = true, desc = "Abandon louiselm handoff" })
  nvim.keymap.set("n", "<Esc>", function()
    self:abandon_handoff(buffer)
  end, { buffer = buffer, silent = true, desc = "Abandon louiselm handoff" })
  return buffer
end

---Submit the current contents of a handoff review buffer.
---@param self louiselm.ui.Chat
---@param buffer integer Handoff buffer returned by `open_handoff`.
---@return boolean sent
---@return string? error_message Validation or target-session error.
function Chat:submit_handoff(buffer)
  local handoff = self.handoffs[buffer]
  if handoff == nil or not nvim.api.nvim_buf_is_valid(buffer) then
    return false, "handoff buffer is not open"
  end
  local text = table.concat(nvim.api.nvim_buf_get_lines(buffer, 0, -1, false), "\n")
  if text:match("%S") == nil then
    return false, "handoff prompt must be a non-empty string"
  end
  local request_id, prompt_error = handoff.target_session:prompt(text)
  if request_id == nil then
    return false, prompt_error or "handoff prompt could not be sent"
  end
  close_handoff(self, buffer)
  if self.views[handoff.target_session_id] ~= nil then
    self:switch(handoff.target_session_id)
  end
  return true
end

---Close a handoff review buffer without sending its contents.
---@param self louiselm.ui.Chat
---@param buffer integer Handoff buffer returned by `open_handoff`.
---@return boolean abandoned
---@return string? error_message Validation error.
function Chat:abandon_handoff(buffer)
  local handoff = self.handoffs[buffer]
  if handoff == nil then
    return false, "handoff buffer is not open"
  end
  close_handoff(self, buffer)
  if self.views[handoff.target_session_id] ~= nil then
    self:switch(handoff.target_session_id)
  end
  return true
end

---Open the full raw payload for the tool call under the cursor.
---@param self louiselm.ui.Chat
---@return boolean opened
---@return string? error_message
function Chat:inspect_tool()
  if self.disposed then
    return false, "chat UI is disposed"
  end
  local view = self.current_id and self.views[self.current_id]
  if view == nil or nvim.api.nvim_get_current_buf() ~= view.buffer then
    return false, "no chat session is open"
  end
  local cursor = nvim.api.nvim_win_get_cursor(0)
  local id = view.tool_ids[cursor[1] - 1]
  if id == nil then
    return false, "cursor is not on a tool-call line"
  end
  return open_tool_inspector(self, view, id)
end

---Report whether a session id is attached, without switching to it or
---touching its buffer.
---
---`LouiselmToMarkdown` needs this to reject an unknown session id before it
---opens its destination-path prompt: the equivalent check inside
---`Chat:to_markdown` only runs once `vim.ui.input`'s callback has fired, so
---without a predicate the user types a full path for an export that was
---never going to happen (louiselm-euj).
---@param self louiselm.ui.Chat
---@param session_id string Session id.
---@return boolean attached
---@return string? error_message Reason the session cannot be used.
function Chat:is_attached(session_id)
  if self.disposed then
    return false, "chat UI is disposed"
  end
  if self.views[session_id] == nil then
    return false, "session is not attached"
  end
  return true
end

---Report whether quitting Neovim from the current chat buffer would abandon
---multiple attached sessions.
---@param self louiselm.ui.Chat
---@return boolean blocked Whether a last-window quit should be blocked.
function Chat:should_block_quit()
  if self.disposed or #nvim.api.nvim_list_wins() ~= 1 then
    return false
  end
  local current_buffer = nvim.api.nvim_get_current_buf()
  local attached = false
  local sessions = 0
  for _, view in pairs(self.views) do
    if view.buffer == current_buffer then
      attached = true
    end
    if view.session:inspect().status ~= "disposed" then
      sessions = sessions + 1
    end
  end
  return attached and sessions > 1
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
  nvim.api.nvim_win_set_cursor(0, { view.prompt_line + 1, 2 })
  if #nvim.api.nvim_list_uis() > 0 then
    nvim.cmd.startinsert()
    nvim.api.nvim_win_set_cursor(0, { view.prompt_line + 1, 2 })
  end
  apply_context_folds(view, view.window)
  apply_tool_folds(view, view.window)
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
  if self.decision_active then
    return false, DECISION_OPEN_ERROR
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
  Picker.select(sessions, {
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

---@param state louiselm.session.State
---@return string path
local function default_markdown_path(state)
  return nvim.fs.joinpath(nvim.fn.getcwd(), "louiselm-" .. state.id .. "-" .. os.date("%Y%m%d-%H%M%S") .. ".md")
end

---Export a session's full, untruncated transcript to a markdown file: every user
---message, every assistant message, and every tool call with its full command and
---result, reconstructed from the session's typed event stream rather than the
---(lossy, single-line) chat buffer. See `louiselm.session.transcript` for the exact
---output format; later blog tooling depends on it staying a plain, deterministic
---mapping from recorded turns to text.
---@param self louiselm.ui.Chat
---@param session_id? string Session id; defaults to the current session.
---@param path? string Destination file path; defaults to a generated path in the current working directory.
---@return string? path Markdown file written.
---@return string? error_message Lifecycle, lookup, or filesystem error.
function Chat:to_markdown(session_id, path)
  if self.disposed then
    return nil, "chat UI is disposed"
  end
  local id = session_id or self.current_id
  local view = id and self.views[id]
  if view == nil then
    return nil, session_id ~= nil and "session is not attached" or "no chat session is open"
  end
  local state = view.session:inspect()
  local destination = path or default_markdown_path(state)
  local markdown = Transcript.render(view.transcript:snapshot(), state)
  local ok, result = pcall(nvim.fn.writefile, split_lines(markdown), destination)
  if not ok or result ~= 0 then
    return nil, "could not write markdown file: " .. destination
  end
  return destination
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
    Picker.select({ "Close", "Keep" }, { prompt = prompt }, function(choice)
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
  if self.decision_active then
    return false, DECISION_OPEN_ERROR
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
  if text == "" and #context_items == 0 and view.pending_skill == nil then
    return nil, "prompt must be a non-empty string"
  end
  local status = view.session:inspect().status
  if ACTIVE_TURN_STATUS[status] then
    queue_prompt(self, view, text)
    return true
  end

  set_prompt_line(view, text)
  if status ~= "ready" then
    notify_prompt_error("session is not ready")
    return nil, "session is not ready"
  end
  return submit_prompt(self, view, text)
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
  if self.decision_active then
    return false, DECISION_OPEN_ERROR
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
  if self.decision_active then
    return false, DECISION_OPEN_ERROR
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
      local content = Skills.read(skill.path)
      local selected = {
        name = skill.name,
        description = skill.description,
        path = skill.path,
        content = content,
        explicit_only = skill.explicit_only == true,
      }
      local queued, queue_error
      if state.skills_policy == "native" then
        queued, queue_error = queue_native_skill(self, view, selected)
      else
        queued, queue_error = queue_injected_skill(self, view, selected)
      end
      if not queued or queue_error ~= nil then
        nvim.notify("louiselm: " .. (queue_error or "could not queue skill"), nvim.log.levels.ERROR)
      end
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
      if self.decision_active then
        return nil, DECISION_OPEN_ERROR
      end
      Picker.select(self.agents, { prompt = "louiselm agent: " }, function(choice)
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
  if session:inspect().skills_policy == "inject" and self.skill_catalog ~= nil then
    self.views[session:inspect().id].skill_catalog = self.skill_catalog
  end
  if self.instructions_context ~= nil then
    local queued, queue_error = self:queue_context(self.instructions_context)
    if not queued then
      return nil, queue_error
    end
  end
  return session
end

---Hand the current session's reviewed transcript off to another configured
---agent: pick a target agent, create and seed its session the same way
---`new_session` does, then open the editable transcript review buffer.
---@param self louiselm.ui.Chat
---@return boolean started
---@return string? error_message Validation or session-state error.
function Chat:hand_off()
  if self.disposed then
    return false, "chat UI is disposed"
  end
  if self.decision_active then
    return false, DECISION_OPEN_ERROR
  end
  local source_id = self.current_id
  local source_view = source_id and self.views[source_id]
  if source_view == nil then
    return false, "no chat session is attached"
  end
  local source_state = source_view.session:inspect()
  if source_state.status ~= "ready" or source_view.queued_prompt ~= nil then
    return false, "current session has an active turn; finish or cancel it before handing off"
  end
  if #self.agents < 2 then
    return false, "handoff requires at least two configured agents"
  end

  local candidates = {}
  for _, name in ipairs(self.agents) do
    if name ~= source_state.agent then
      candidates[#candidates + 1] = name
    end
  end

  Picker.select(candidates, { prompt = "louiselm handoff target: " }, function(agent_name)
    if agent_name == nil or self.disposed or self.views[source_id] ~= source_view then
      return
    end
    local target_session, session_error = self:new_session(agent_name)
    if target_session == nil then
      nvim.notify("louiselm: " .. (session_error or "could not create handoff target session"), nvim.log.levels.ERROR)
      return
    end
    local _, handoff_error = self:open_handoff(target_session, source_id)
    if handoff_error ~= nil then
      nvim.notify("louiselm: " .. handoff_error, nvim.log.levels.ERROR)
    end
  end)
  return true
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
  if self.decision_active then
    return false, DECISION_OPEN_ERROR
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

      Picker.select(sessions, {
        prompt = all_workspaces and "louiselm session (all workspaces): " or "louiselm session: ",
        format_item = discovered_session_summary,
      }, function(selected)
        if selected == nil or self.disposed then
          return
        end
        local error_reported = false
        local session, load_error = self.api:load_session(selected.agent, selected.session_id, {
          cwd = selected.cwd,
          name = selected.session_id,
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
  for window in pairs(self.tool_inspect_windows) do
    if nvim.api.nvim_win_is_valid(window) then
      nvim.api.nvim_win_close(window, true)
    end
  end
  self.tool_inspect_windows = {}
  self.decision_active = nil
  pump_decisions(self)
  for buffer in pairs(self.handoffs) do
    close_handoff(self, buffer)
  end
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
