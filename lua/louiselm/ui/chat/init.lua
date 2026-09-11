local Context = require("louiselm.ui.context")
local Attention = require("louiselm.ui.attention")
local Decisions = require("louiselm.ui.chat.decisions")
local Inspector = require("louiselm.ui.chat.inspector")
local Limits = require("louiselm.ui.limits")
local Status = require("louiselm.ui.chat.status")
local ChatBuffer = require("louiselm.ui.chat.buffer")
local Draft = require("louiselm.ui.chat.draft")
local Handoffs = require("louiselm.ui.chat.handoff")
local Recovery = require("louiselm.workflow.recovery")
local Picker = require("louiselm.ui.picker")
local Skills = require("louiselm.skills")
local Transcript = require("louiselm.session.transcript")
local Usage = require("louiselm.routing.usage")

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

---@class louiselm.ui.ChatOptions
---@field agents? string[] Agent names shown by the new-session picker.
---@field skills? louiselm.skills.Skill[] Skills shown by the invocation picker.
---@field skill_paths? string[] Configured roots rediscovered when the invocation picker opens.
---@field initial_contexts? louiselm.ui.ContextItem[] Context queued for every new session.
---@field skill_catalog? string Hidden catalog held for the first accepted model prompt in a new inject session.
---@field instructions_context? louiselm.ui.ContextItem Project instructions resource link queued only for brand-new sessions.
---@field workflow? louiselm.routing.Coordinator Phase-aware routing coordinator.
---@field markdown_highlighting? boolean Whether chat buffers start Markdown tree-sitter highlighting; defaults to true.
---@field start_insert_on_switch? boolean Whether switching to a chat starts Insert mode; defaults to true.

---@class louiselm.ui.ChatView
---@field renderer louiselm.ui.ChatBuffer Buffer lifecycle and presentation coordinates.
---@field session louiselm.session.Session Attached session.
---@field source_buffer integer Buffer that was current when the chat view was attached.
---@field draft louiselm.ui.ChatDraft Staged content and queued text, independent of buffer coordinates.
---@field workflow_phase? louiselm.routing.PhaseMetadata Last phase-tagged skill used by this view.
---@field tool_inspect_windows table<integer, boolean> Floating raw-payload windows owned by this chat.
---@field setup_shown boolean Whether the initial options overview was offered.
---@field options_revision? integer Invalidates pending option-history pickers.
---@field unread_turn boolean Whether a completed background response has not been focused.
---@field replay_active boolean Whether session/load history is still arriving.
---@field replay_user_open boolean Whether consecutive replayed user chunks belong to the current historical turn.
---@field replay_turn integer Number of historical user turns replayed into this view.
---@field restored_usage table<integer, louiselm.session.TurnUsage> Persisted presentation usage by historical turn.
---@field replay_events? { event: louiselm.session.Event, state: louiselm.session.State? }[] UI events waiting for asynchronous usage history.
---@field transcript louiselm.session.Transcript Full, untruncated record of this session's turns.
---@field unsubscribe fun() Session event listener removal function.
---@field last_forensics_path string? Path of the most recently collected Forensics record for this session.

---@class louiselm.ui.Chat
---@field api louiselm.session.Api Session API used to create sessions.
---@field agents string[] Agent names for the picker.
---@field skills louiselm.skills.Skill[] Skills for the invocation picker.
---@field skill_paths string[] Configured roots rediscovered when the invocation picker opens.
---@field skill_warning_signature string? Last reported discovery diagnostics, for notification deduplication.
---@field initial_contexts louiselm.ui.ContextItem[] Context queued for every new session.
---@field skill_catalog? string Hidden catalog copied only into brand-new inject sessions.
---@field instructions_context? louiselm.ui.ContextItem Project instructions resource link queued only for brand-new sessions.
---@field markdown_highlighting boolean Whether chat buffers start Markdown tree-sitter highlighting.
---@field start_insert_on_switch boolean Whether switching to a chat starts Insert mode.
---@field attention louiselm.ui.Attention Shared durable Attention controller.
---@field decisions louiselm.ui.Decisions Permission presentation and responder lifecycle.
---@field usage louiselm.routing.Usage Persistent measured usage ledger.
---@field workflow? louiselm.routing.Coordinator Phase-aware routing coordinator.
---@field views table<string, louiselm.ui.ChatView> Views by local session id.
---@field view_order string[] Attached session ids in display order.
---@field tool_inspect_windows table<integer, boolean> Floating raw-payload windows owned by this chat.
---@field limits_buffers table<string, integer> Account-limit detail buffers by Agent.
---@field limits_alerts table<string, table<string, integer>> Emitted threshold ranks by Agent and reset cycle.
---@field limits_refreshing table<string, boolean> Agents with a refresh in flight.
---@field limits_timers table<string, louiselm.ui.LimitsTimer> Pending reset-expiry timers by Agent.
---@field limits_unsubscribe fun() Agent-limit observer removal function.
---@field winbars table<integer, string> Previous window bars by window id.
---@field winbar_targets table<integer, table<integer, string|false|louiselm.ui.LimitsTarget>> Click targets by window and minwid.
---@field winbar_resize_autocmd? integer Resize observer removed on disposal.
---@field current_id string? Currently displayed session id.
---@field handoffs louiselm.ui.Handoffs Review buffers and takeover validation.
---@field recovery louiselm.workflow.Recovery Park admission and pending resume resources.
---@field disposed boolean Whether the chat UI has been disposed.
---@field attach fun(self: louiselm.ui.Chat, session: louiselm.session.Session): boolean, string? Attach or focus a session.
---@field buffer fun(self: louiselm.ui.Chat, session_id?: string): integer? Return a session buffer.
---@field inspect_tool fun(self: louiselm.ui.Chat): boolean, string? Open the raw payload under the cursor.
---@field switch fun(self: louiselm.ui.Chat, session_id: string): boolean, string? Focus an attached session.
---@field winbar_click fun(self: louiselm.ui.Chat, target: integer, clicked_window?: integer): boolean, string? Follow a winbar click target.
---@field switch_session fun(self: louiselm.ui.Chat): boolean, string? Pick an attached session and focus it.
---@field close_session fun(self: louiselm.ui.Chat): boolean, string? Close the current session, confirming when active.
---@field staged_context fun(self: louiselm.ui.Chat): table<louiselm.session.Session, louiselm.ui.StagedContext> Report Staged context by attached Session.
---@field cancel fun(self: louiselm.ui.Chat): boolean, string? Cancel the current session turn.
---@field session_options fun(self: louiselm.ui.Chat): boolean, string? Open the current session options overview.
---@field show_limits fun(self: louiselm.ui.Chat, agent_name?: string): boolean, string? Inspect one configured Agent's account limits.
---@field manage_permissions fun(self: louiselm.ui.Chat): boolean, string? Inspect and revoke remembered permission rules.
---@field rename_session fun(self: louiselm.ui.Chat, name: string): boolean, string? Rename the current session.
---@field session_id fun(self: louiselm.ui.Chat): string?, string? Return the current agent-scoped ACP session identifier.
---@field collect_forensics fun(self: louiselm.ui.Chat, callback?: fun(path: string?, error_message?: string)): boolean, string? Collect and queue a Forensics resource link.
---@field latest_forensics_path fun(self: louiselm.ui.Chat): string?, string? Return the most recently collected Forensics record path for the current session.
---@field to_markdown fun(self: louiselm.ui.Chat, session_id?: string, path?: string): string?, string? Export a session's full transcript to a markdown file.
---@field open_handoff fun(self: louiselm.ui.Chat, target_session: louiselm.session.Session, source_session_id?: string): integer?, string? Open an editable transcript for a target session.
---@field submit_handoff fun(self: louiselm.ui.Chat, buffer: integer): boolean, string? Submit and close a handoff buffer.
---@field abandon_handoff fun(self: louiselm.ui.Chat, buffer: integer): boolean, string? Close a handoff buffer without sending it.
---@field set_config_option fun(self: louiselm.ui.Chat, id: string, value: string|boolean, callback?: fun(options: louiselm.session.ConfigOption[]?, error?: string)): string|number?, string? Change an idle session option.
---@field submit fun(self: louiselm.ui.Chat, text?: string): string|number|boolean?, string? Submit or queue the current prompt.
---@field queue_context fun(self: louiselm.ui.Chat, item: louiselm.ui.ContextItem): boolean, string? Queue context for the next prompt.
---@field mention_buffer fun(self: louiselm.ui.Chat): boolean, string? Queue the source buffer context.
---@field send_selection fun(self: louiselm.ui.Chat): boolean, string? Queue the source visual selection.
---@field mention_diagnostics fun(self: louiselm.ui.Chat): boolean, string? Queue the source buffer's diagnostics.
---@field pick_file fun(self: louiselm.ui.Chat, root?: string): boolean, string? Pick and queue a file context.
---@field pick_skill fun(self: louiselm.ui.Chat): boolean, string? Pick and queue a skill invocation.
---@field new_session fun(self: louiselm.ui.Chat, agent_name?: string, options?: louiselm.session.Options): louiselm.session.Session?, string? Create a session, using the picker when needed.
---@field hand_off fun(self: louiselm.ui.Chat): boolean, string? Hand the current session's reviewed transcript off to another configured agent.
---@field resume_session fun(self: louiselm.ui.Chat, all_workspaces?: boolean): boolean, string? Discover and load a prior ACP session.
---@field resume_park fun(self: louiselm.ui.Chat): boolean, string? Discover and load a durable cold-Parked Run.
---@field park fun(self: louiselm.ui.Chat): boolean, string? Cold-Park the current Session through a Run.
---@field dispose fun(self: louiselm.ui.Chat): boolean Dispose buffers and listeners.

local M = {}
local Chat = {}
Chat.__index = Chat

---@class louiselm.ui.LimitsTimer
---@field is_closing fun(self: louiselm.ui.LimitsTimer): boolean
---@field start fun(self: louiselm.ui.LimitsTimer, timeout: integer, repeat_interval: integer, callback: fun())
---@field stop fun(self: louiselm.ui.LimitsTimer)
---@field close fun(self: louiselm.ui.LimitsTimer)

---Copy a caller-owned array, rejecting a sparse table and any item `build` refuses.
---The density check is not pedantry: these values cross the headless session API, and a
---hole would silently truncate every later read that trusts `#value`.
---@generic T
---@param value unknown
---@param invalid string Error when the value is not a table.
---@param sparse string Error when the table has holes or non-sequential keys.
---@param build fun(item: unknown, index: integer): T?, string? Copy one item, or reject it.
---@return T[]? values
---@return string? error_message
local function copy_dense(value, invalid, sparse, build)
  if value == nil then
    return {}
  end
  if type(value) ~= "table" then
    return nil, invalid
  end
  local values = {}
  for index, item in ipairs(value) do
    local built, build_error = build(item, index)
    if built == nil then
      return nil, build_error
    end
    values[index] = built
  end
  for key in pairs(value) do
    if type(key) ~= "number" or key < 1 or key > #value or key % 1 ~= 0 then
      return nil, sparse
    end
  end
  return values
end

---@param value unknown
---@param label string
---@return string[]? values
---@return string? error_message
local function copy_string_array(value, label)
  local invalid = "chat " .. label .. " must be a string[]"
  return copy_dense(value, invalid, "chat " .. label .. " must be a dense string[]", function(item)
    if type(item) ~= "string" or item == "" then
      return nil, invalid
    end
    return item
  end)
end

---@param value unknown
---@return louiselm.skills.Skill[]? skills
---@return string? error_message
local function copy_skills(value)
  return copy_dense(
    value,
    "chat skills must be a skill[]",
    "chat skills must be a dense skill[]",
    function(skill, index)
      if
        type(skill) ~= "table"
        or type(skill.name) ~= "string"
        or skill.name == ""
        or type(skill.description) ~= "string"
        or type(skill.path) ~= "string"
      then
        return nil, string.format("chat skill at index %d is malformed", index)
      end
      return {
        name = skill.name,
        description = skill.description,
        path = skill.path,
        explicit_only = skill.explicit_only == true,
        phase = skill.phase,
      }
    end
  )
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
  return copy_dense(
    value,
    "chat initial contexts must be a context[]",
    "chat initial contexts must be a dense context[]",
    function(item, index)
      if not is_context_item(item) then
        return nil, string.format("chat initial context at index %d is malformed", index)
      end
      return { label = item.label, text = item.text, uri = item.uri }
    end
  )
end

---@param value string
---@return string line
local function single_line(value)
  return (value:gsub("[\r\n]", " "))
end

-- A select provider closes the picker a new one replaces, which would answer an open
-- permission request without a choice. Commands that open their own picker refuse while
-- a decision is presented instead of queueing behind it: a decision can open a nested
-- picker of its own, and the way out of a stuck decision must never be queued behind it.
local DECISION_OPEN_ERROR = "a louiselm permission decision is open; answer it first"
local DEFAULT_HIGHLIGHTS = {
  LouiselmAcpValue = "Identifier",
  LouiselmDerivedValue = "Number",
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

local ACTIVE_TURN_STATUS = {
  preparing = true,
  prompting = true,
  waiting_permission = true,
  cancelling = true,
}

---@param view louiselm.ui.ChatView
local function clear_queued_prompt(view)
  view.draft:queue(nil)
  view.renderer:clear_queue_indicator()
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

---@param session louiselm.session.DiscoveredSession
---@return string? subject
local function discovered_session_subject(session)
  local title = session.title
  if title == nil then
    return nil
  end
  title = title:match("^%s*(.-)%s*$") or ""
  if title:sub(1, #"<skills_instructions>") == "<skills_instructions>" then
    local closing = "</available_skills>"
    local closing_start = title:find(closing, 1, true)
    if closing_start == nil then
      return nil
    end
    title = title:sub(closing_start + #closing)
  end
  title = title:match("^%s*(.-)%s*$") or ""
  if title:sub(1, 2) == "[@" then
    local closing = title:find(")", 3, true)
    if closing ~= nil then
      title = title:sub(closing + 1)
    end
  elseif title:sub(1, #"[Resource link:") == "[Resource link:" then
    local closing = title:find("]", #"[Resource link:" + 1, true)
    if closing ~= nil then
      title = title:sub(closing + 1)
    end
  end
  title = single_line(title):match("^%s*(.-)%s*$") or ""
  if title == "" or title == session.session_id then
    return nil
  end
  return title
end

---@param session louiselm.session.DiscoveredSession
---@return string
local function discovered_session_summary(session)
  local parts = {
    report_id(single_line(session.agent), single_line(session.session_id)),
  }
  if session.updated_at ~= nil then
    parts[#parts + 1] = "updated=" .. single_line(session.updated_at)
  end
  parts[#parts + 1] = "cwd=" .. single_line(session.cwd)
  local subject = discovered_session_subject(session)
  if subject ~= nil then
    parts[#parts + 1] = subject
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

---@param self louiselm.ui.Chat
---@param view louiselm.ui.ChatView
---@param win integer
---@return string
local function chat_winbar(self, view, win)
  local state = view.session:inspect()
  local limits_state = self.api:inspect_agent_limits(state.agent)
  local limits
  if limits_state ~= nil then
    local text, group = Limits.summary(limits_state)
    if text ~= nil and group ~= nil then
      limits = { text = text, group = group }
    end
  end
  local base, limits_agent = Status.session_winbar(state, limits)
  local backgrounds = {} ---@type louiselm.ui.BackgroundStatus[]
  for _, id in ipairs(self.view_order) do
    local background = self.views[id]
    if id ~= state.id and background ~= nil then
      backgrounds[#backgrounds + 1] = {
        state = background.session:inspect(),
        unread_turn = background.unread_turn,
      }
    end
  end
  local available = nvim.api.nvim_win_get_width(win)
    - nvim.api.nvim_eval_statusline(base, { winid = win, use_winbar = true }).width
  local winbar, targets = Status.layout_winbar(base, limits_agent, backgrounds, available)
  self.winbar_targets[win] = targets
  return winbar
end

---@param self louiselm.ui.Chat
---@param view louiselm.ui.ChatView
---@param win integer
local function render_winbar(self, view, win)
  if not nvim.api.nvim_win_is_valid(win) or nvim.api.nvim_win_get_buf(win) ~= view.renderer.buffer then
    return
  end
  if self.winbars[win] == nil then
    self.winbars[win] = nvim.api.nvim_get_option_value("winbar", { win = win })
  end
  nvim.api.nvim_set_option_value("winbar", chat_winbar(self, view, win), { win = win })
end

---@param self louiselm.ui.Chat
local function render_winbars(self)
  for _, win in ipairs(nvim.api.nvim_list_wins()) do
    local buffer = nvim.api.nvim_win_get_buf(win)
    for _, id in ipairs(self.view_order) do
      local view = self.views[id]
      if view ~= nil and view.renderer.buffer == buffer then
        render_winbar(self, view, win)
        break
      end
    end
  end
end

local schedule_limits_expiry

---@param self louiselm.ui.Chat
---@param state louiselm.session.LimitsState
local function handle_limits_state(self, state)
  if self.disposed then
    return
  end
  local buffer = self.limits_buffers[state.agent]
  if buffer ~= nil then
    if nvim.api.nvim_buf_is_valid(buffer) then
      Limits.update(buffer, state)
    else
      self.limits_buffers[state.agent] = nil
    end
  end
  local alerts, seen = Limits.threshold_alerts(state, self.limits_alerts[state.agent] or {})
  self.limits_alerts[state.agent] = seen
  for _, alert in ipairs(alerts) do
    local level = alert.level == "error" and nvim.log.levels.ERROR or nvim.log.levels.WARN
    nvim.notify("louiselm: " .. alert.message, level)
  end
  render_winbars(self)
  schedule_limits_expiry(self, state)
end

---@param self louiselm.ui.Chat
---@param agent_name string
local function close_limits_timer(self, agent_name)
  local timer = self.limits_timers[agent_name]
  self.limits_timers[agent_name] = nil
  if timer ~= nil and not timer:is_closing() then
    timer:stop()
    timer:close()
  end
end

---@param self louiselm.ui.Chat
---@param state louiselm.session.LimitsState
schedule_limits_expiry = function(self, state)
  close_limits_timer(self, state.agent)
  if state.status ~= "fresh" or state.snapshot == nil then
    return
  end
  local expires_at
  for _, bucket in ipairs(state.snapshot.buckets) do
    for _, window in ipairs(bucket.windows) do
      if expires_at == nil or window.resets_at < expires_at then
        expires_at = window.resets_at
      end
    end
  end
  if expires_at == nil then
    return
  end
  ---@type louiselm.ui.LimitsTimer
  local timer = assert(nvim.uv.new_timer())
  self.limits_timers[state.agent] = timer
  timer:start(math.max(1, (expires_at - os.time()) * 1000 + 10), 0, function()
    timer:stop()
    timer:close()
    if self.limits_timers[state.agent] == timer then
      self.limits_timers[state.agent] = nil
    end
    nvim.schedule(function()
      if self.disposed then
        return
      end
      local current = self.api:inspect_agent_limits(state.agent)
      if current ~= nil then
        handle_limits_state(self, current)
      end
    end)
  end)
end

---@param self louiselm.ui.Chat
---@param agent_name string
local function refresh_limits(self, agent_name)
  if self.limits_refreshing[agent_name] then
    return
  end
  self.limits_refreshing[agent_name] = true
  local started = self.api:refresh_agent_limits(agent_name, function(state)
    self.limits_refreshing[agent_name] = nil
    -- ACP process callbacks are fast events; UI work belongs on the main loop.
    nvim.schedule(function()
      handle_limits_state(self, state)
    end)
  end)
  if not started then
    self.limits_refreshing[agent_name] = nil
  end
end

---@param self louiselm.ui.Chat
local function restore_winbars(self)
  for win, value in pairs(self.winbars) do
    if nvim.api.nvim_win_is_valid(win) then
      nvim.api.nvim_set_option_value("winbar", value, { win = win })
    end
  end
  self.winbars = {}
  self.winbar_targets = {}
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
  if self.disposed or self.views[view.session:inspect().id] ~= view then
    return false, "chat UI is disposed"
  end
  local text = view.draft:reconcile_skills(view.renderer:prompt_text())
  view.draft:add_context(item)
  view.renderer:set_prefix(view.draft.context_prefix, text)
  return true
end

---Stage a picker selection and render its prefix through the buffer owner.
---@param self louiselm.ui.Chat
---@param view louiselm.ui.ChatView
---@param skill louiselm.skills.Skill
---@param native boolean
---@return boolean queued
---@return string? error_message
local function queue_skill(self, view, skill, native)
  if self.disposed or self.views[view.session:inspect().id] ~= view then
    return false, "chat UI is disposed"
  end
  local text = view.draft:reconcile_skills(view.renderer:prompt_text())
  local stage_error = view.draft:select_skill(skill, native)
  view.renderer:set_prefix(view.draft.context_prefix, text)
  view.workflow_phase = skill.phase
  return true, stage_error
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
---@param lines string[]
---@return integer window
local function open_payload_inspector(self, lines)
  local window
  window = Inspector.open(lines, function()
    self.tool_inspect_windows[window] = nil
  end)
  self.tool_inspect_windows[window] = true
  return window
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
  open_payload_inspector(self, nvim.split(nvim.inspect(entry.raw or {}), "\n", { plain = true }))
  return true
end

---@param value unknown
---@return string? text
local function field(value, name)
  if type(value) == "table" and type(value[name]) == "string" and value[name] ~= "" then
    return value[name]
  end
  return nil
end

local open_session_options

---Resolve missing injected bodies at the existing filesystem boundary.
---@param draft louiselm.ui.ChatDraft
---@return string? error_message
local function read_context_skills(draft)
  for index, item in ipairs(draft.contexts) do
    if item.text == nil and item.skill_path ~= nil then
      local skill_content = Skills.read(item.skill_path)
      if skill_content == nil then
        return "could not read selected skill: " .. item.skill_path
      end
      draft:cache_context_content(index, skill_content)
    end
  end
  return nil
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
  local draft = view.draft
  local command_name
  if draft.pending_skill ~= nil then
    if draft.pending_skill.content == nil then
      local content = Skills.read(draft.pending_skill.path)
      if content == nil then
        return nil, "could not read selected skill: " .. draft.pending_skill.path, {}
      end
      draft:cache_native_content(content)
    end
    local resolve_error
    command_name, resolve_error = Skills.resolve_command(draft.pending_skill, view.session:inspect().commands)
    if command_name == nil then
      return nil, resolve_error, {}
    end
  end
  local context_error = read_context_skills(draft)
  if context_error ~= nil then
    return nil, context_error, {}
  end
  local content, contexts = draft:content(text, command_name, view.session:inspect().embedded_context)
  return content, nil, contexts
end

---@param view louiselm.ui.ChatView
---@param text string
local function set_prompt_line(view, text)
  view.renderer:replace_prompt(view.draft.context_prefix .. text)
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
  local phase = view.draft.pending_skill and view.draft.pending_skill.phase
  local request_id, prompt_error = view.session:prompt(content)
  if request_id == nil then
    local message = prompt_error or "prompt failed"
    notify_prompt_error(message)
    return nil, message
  end
  if state.acp_session_id ~= nil then
    self.attention:prompt_started(state.acp_session_id)
  end
  view.transcript:record_user(text)

  clear_queued_prompt(view)
  local slash_prompt = text:sub(1, 1) == "/"
  local next_prefix = slash_prompt and view.draft.context_prefix or ""
  view.renderer:accept_prompt(text, contexts, next_prefix, true)
  if not slash_prompt then
    view.draft:clear_context()
    if phase ~= nil then
      view.workflow_phase = phase
    end
  end
  return request_id
end

---@param view louiselm.ui.ChatView
---@param text string
---@param source_session_id string
---@param contexts louiselm.ui.ContextItem[]
local function record_handoff_prompt(view, text, source_session_id, contexts)
  view.transcript:record_handoff(text, source_session_id)
  clear_queued_prompt(view)
  view.renderer:accept_prompt(text, contexts, "", false)
end

---@param view louiselm.ui.ChatView
---@param text string
local function queue_prompt(view, text)
  clear_queued_prompt(view)
  set_prompt_line(view, text)
  view.draft:queue(text)
  view.renderer:queue_indicator()
end

---@param self louiselm.ui.Chat
---@param view louiselm.ui.ChatView
local function release_queued_prompt(self, view)
  local queued = view.draft.queued_prompt
  if queued == nil then
    return
  end
  clear_queued_prompt(view)
  submit_prompt(self, view, queued)
end

---@param option louiselm.session.ConfigOption
---@return table[] values
local function config_values(option)
  if option.type == "boolean" then
    return { { value = true, name = "true" }, { value = false, name = "false" } }
  end
  return option.options or {}
end

---@param summary louiselm.session.CohortSummary?
---@return string
local function usage_details(summary)
  if summary == nil or summary.turns == 0 then
    return " (No matching history)"
  end
  local details = { summary.turns .. " turns" }
  local tokens = summary.tokens.total_tokens
  if tokens ~= nil then
    details[#details + 1] = format_number(tokens.average) .. " tokens/turn · token data for " .. tokens.samples
  else
    details[#details + 1] = "No total-token data"
  end
  for _, cost in ipairs(summary.costs) do
    details[#details + 1] = format_number(cost.average)
      .. " "
      .. cost.currency
      .. "/turn · cost data for "
      .. cost.samples
  end
  return " (observed: " .. table.concat(details, " · ") .. ")"
end

---@param self louiselm.ui.Chat
---@param view louiselm.ui.ChatView
---@param initial boolean
open_session_options = function(self, view, initial)
  if self.disposed or self.views[view.session:inspect().id] ~= view or self.decisions:is_active() then
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
  view.options_revision = (view.options_revision or 0) + 1
  local revision = view.options_revision
  local function current()
    return not self.disposed
      and self.views[state.id] == view
      and self.current_id == state.id
      and not self.decisions:is_active()
      and view.options_revision == revision
      and view.session:inspect().status == "ready"
      and nvim.deep_equal(view.session:inspect().config_options, state.config_options)
  end
  Picker.select(state.config_options, {
    prompt = "louiselm session options: ",
    format_item = function(option)
      return option.name .. ": " .. Status.option_display_value(option)
    end,
  }, function(option)
    if option == nil or not current() then
      return
    end
    view.session:option_usage(option.id, function(candidates, query_error)
      if not current() then
        return
      end
      local details = {}
      for _, candidate in ipairs(candidates or {}) do
        details[candidate.value] = candidate.error and " (" .. candidate.error.message .. ")"
          or usage_details(candidate.summary)
      end
      Picker.select(config_values(option), {
        prompt = option.name .. ": ",
        format_item = function(value)
          return value.name
            .. (
              query_error and " (History unavailable: " .. query_error.message .. ")"
              or details[value.value]
              or usage_details(nil)
            )
        end,
      }, function(choice)
        if not current() then
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
              view.renderer:append({ "Error: " .. callback_error })
            else
              view.renderer:header(view.session:inspect())
            end
            open_session_options(self, view, false)
          end)
        end)
        if set_error ~= nil then
          view.renderer:append({ "Error: " .. set_error })
        end
      end)
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
---@param turn integer
local function restore_turn_usage(self, view, turn)
  local usage = view.restored_usage[turn]
  view.restored_usage[turn] = nil
  local line = usage_line(usage)
  if line ~= nil then
    view.renderer:usage(line)
  end
end

---@param self louiselm.ui.Chat
---@param view louiselm.ui.ChatView
---@param phase louiselm.routing.PhaseMetadata
---@param state louiselm.session.State
---@param callback fun()
local function present_workflow_feedback(self, view, phase, state, callback)
  local function finish(rating, context)
    if rating ~= nil then
      local recorded, error_message = self.workflow:feedback(phase, state, rating, context)
      if not recorded then
        view.renderer:append({ "Error: " .. (error_message or "could not record workflow feedback") })
      end
    end
    callback()
  end

  Picker.select({ "Good", "Skip", "Poor" }, { prompt = "louiselm workflow feedback: " }, function(choice)
    if choice == nil or self.disposed or self.views[state.id] ~= view then
      finish(nil)
      return
    end
    local rating = string.lower(choice)
    if rating ~= "poor" then
      finish(rating)
      return
    end
    nvim.ui.input({ prompt = "louiselm workflow feedback context: " }, function(context)
      if self.disposed or self.views[state.id] ~= view then
        return
      end
      finish(context and "poor" or nil, context)
    end)
  end)
end

---@param self louiselm.ui.Chat
---@param view louiselm.ui.ChatView
---@param candidate louiselm.routing.ApprovalCandidate
---@return boolean applied
---@return string? error_message
local function apply_recommendation(self, view, candidate)
  if candidate.action == "continue" then
    view.renderer:append({ "[workflow] approved CONTINUE: " .. candidate.label })
    return true
  end
  if candidate.action == "model" then
    if candidate.model == nil then
      return false, "approved Model change has no Model value"
    end
    local state = view.session:inspect()
    for _, option in ipairs(state.config_options) do
      if option.category == "model" then
        local _, option_error = self:set_config_option(option.id, candidate.model, function(_, error_message)
          nvim.schedule(function()
            if self.disposed or self.views[state.id] ~= view then
              return
            end
            if error_message ~= nil then
              view.renderer:append({ "Error: " .. error_message })
            else
              view.renderer:append({ "[workflow] approved Model: " .. candidate.model })
              view.renderer:header(view.session:inspect())
            end
          end)
        end)
        return option_error == nil, option_error
      end
    end
    return false, "current Session advertises no Model option"
  end
  local target, session_error = self:new_session(candidate.agent)
  if target == nil then
    return false, session_error or "could not create Handoff target Session"
  end
  local _, handoff_error = self:open_handoff(target, view.session:inspect().id)
  if handoff_error ~= nil then
    return false, handoff_error
  end
  return true
end

---@param self louiselm.ui.Chat
---@param view louiselm.ui.ChatView
---@param phase louiselm.routing.PhaseMetadata
---@param pending louiselm.routing.ApprovalPresentation
local function present_recommendations(self, view, phase, pending)
  if self.disposed or self.views[view.session:inspect().id] ~= view then
    return
  end
  Picker.select(pending.candidates, {
    prompt = "louiselm workflow recommendation: ",
    format_item = function(candidate)
      return candidate.label
    end,
  }, function(candidate)
    if candidate == nil or self.disposed or self.views[view.session:inspect().id] ~= view then
      if candidate == nil then
        self.workflow:clear_pending(phase)
      end
      return
    end
    Picker.select({ "Approve", "Reject" }, { prompt = "louiselm workflow recommendation action: " }, function(action)
      if action == nil or self.disposed or self.views[view.session:inspect().id] ~= view then
        if action == nil then
          self.workflow:clear_pending(phase)
        end
        return
      end
      if action == "Reject" then
        local rejected, rejection_error = self.workflow:reject(candidate)
        if not rejected then
          view.renderer:append({ "Error: " .. (rejection_error or "recommendation could not be rejected") })
        end
        return
      end
      local approved, approval_error = self.workflow:approve(candidate)
      if approved == nil then
        view.renderer:append({ "Error: " .. (approval_error or "recommendation could not be approved") })
        return
      end
      local applied, apply_error = apply_recommendation(self, view, approved)
      if not applied then
        view.renderer:append({ "Error: " .. (apply_error or "recommendation could not be applied") })
      end
    end)
  end)
end

---@param self louiselm.ui.Chat
---@param view louiselm.ui.ChatView
---@param event louiselm.session.Event
---@param completed_state? louiselm.session.State Immutable completion snapshot captured before queued UI work.
local function handle_event(self, view, event, completed_state)
  if self.disposed or self.views[event.session_id] ~= view or not nvim.api.nvim_buf_is_valid(view.renderer.buffer) then
    if event.type == "permission_requested" and type(event.respond) == "function" then
      -- Scheduled before teardown, delivered after it: no view is left to host the choice,
      -- and no other consumer will answer. The result has nowhere left to be reported.
      self.decisions:request(view.session, nil, event.respond)
    end
    return
  end

  view.renderer:reconcile()

  if
    event.type == "state_changed"
    or event.type == "config_options_changed"
    or event.type == "usage_updated"
    or event.type == "recording_changed"
  then
    view.renderer:header(view.session:inspect())
    render_winbars(self)
  end
  if event.type == "state_changed" then
    if event.data.status == "prompting" and view.session:inspect().acp_session_id ~= nil then
      self.attention:prompt_started(view.session:inspect().acp_session_id)
    elseif event.data.status == "disposed" then
      local state = view.session:inspect()
      if state.acp_session_id ~= nil then
        self.attention:session_disposed(state.acp_session_id)
      end
    end
  end
  if event.type == "state_changed" and event.data.status == "ready" then
    -- Load replay can end with a reasoning paragraph and no trailing answer, so
    -- the turn boundary only shows up here.
    view.renderer:close_reasoning()
    if view.replay_active then
      restore_turn_usage(self, view, view.replay_turn)
      view.replay_active = false
      view.replay_user_open = false
    end
    open_session_options(self, view, true)
    release_queued_prompt(self, view)
    refresh_limits(self, view.session:inspect().agent)
  end

  if event.type == "user_chunk" then
    view.renderer:close_reasoning()
    local continuing_prompt = view.replay_active and view.replay_user_open
    if view.replay_active and not view.replay_user_open then
      restore_turn_usage(self, view, view.replay_turn)
      view.replay_turn = view.replay_turn + 1
      view.replay_user_open = true
    end
    view.renderer:render(event, view.replay_active, continuing_prompt)
  elseif
    event.type == "chunk"
    or event.type == "thought_chunk"
    or event.type == "tool_call_started"
    or event.type == "tool_call_finished"
  then
    view.replay_user_open = false
    view.renderer:render(event)
  elseif event.type == "prompt_rejected" then
    view.renderer:append({ "Prompt not sent: " .. event.data.message })
  elseif event.type == "recording_changed" then
    if event.data.error ~= nil then
      clear_queued_prompt(view)
      view.renderer:append({ "Recording error: " .. event.data.error.message })
    end
  elseif event.type == "error" then
    view.replay_user_open = false
    clear_queued_prompt(view)
    local message = field(event.data, "message") or "unknown session error"
    view.renderer:error(message)
    local state = view.session:inspect()
    local run = view.session.owner_run
    self.attention:session_failed(state, run and run.id or nil)
  elseif event.type == "permission_requested" then
    local data = event.data
    local state = view.session:inspect()
    if type(data) == "table" then
      self.attention:permission_required(state, data)
    end
    if type(data) == "table" and type(data.permission_error) == "string" then
      view.renderer:append({ "Warning: " .. data.permission_error })
    elseif type(data) == "table" and data.remembered_decision ~= nil then
      view.renderer:append({ "Warning: remembered decision requires a compatible once-only option" })
    end
    self.decisions:request(view.session, data, event.respond)
  elseif event.type == "permission_cancelled" then
    local state = view.session:inspect()
    if state.acp_session_id ~= nil and type(event.data) == "table" then
      self.attention:permission_cancelled(state.acp_session_id, event.data.request_ids)
    end
    self.decisions:cancel(view.session, type(event.data) == "table" and event.data.request_ids or nil)
  elseif event.type == "turn_done" then
    view.renderer:finish_turn()
    local state = completed_state or view.session:inspect()
    self.attention:turn_done(state, nvim.api.nvim_get_current_buf() == view.renderer.buffer)
    local line = usage_line(state.usage)
    if line ~= nil then
      view.renderer:usage(line)
    end
    if self.workflow ~= nil and view.workflow_phase ~= nil and view.draft.queued_prompt == nil then
      local outcome = type(event.data) == "table" and event.data.stopReason == "cancelled" and "cancelled"
        or "completed"
      local observed, observe_error = self.workflow:observe(view.workflow_phase, state, outcome)
      if not observed then
        view.renderer:append({ "Error: " .. (observe_error or "could not record workflow evidence") })
      end
      present_workflow_feedback(self, view, view.workflow_phase, state, function()
        if self.disposed or self.views[state.id] ~= view then
          return
        end
        local pending, _, recommend_error = self.workflow:recommend(view.workflow_phase, state)
        if pending ~= nil then
          present_recommendations(self, view, view.workflow_phase, pending)
        elseif recommend_error ~= nil and recommend_error ~= "phase already has an approved choice" then
          view.renderer:append({ "Error: " .. recommend_error })
        end
      end)
    end
    view.renderer:reset_response()
    release_queued_prompt(self, view)
    view.unread_turn = state.status == "ready" and nvim.api.nvim_get_current_buf() ~= view.renderer.buffer
    render_winbars(self)
  end
end

---@param self louiselm.ui.Chat
---@param view louiselm.ui.ChatView
---@param event louiselm.session.Event
local function observe_view_event(self, view, event)
  -- Recording is a pure data transform, not an editor/UI operation, so it can run
  -- directly in this fast-event callback instead of waiting for the scheduled turn.
  view.transcript:record(event)
  local state = event.type == "turn_done" and nvim.deepcopy(view.session:inspect()) or nil
  -- ACP stdout callbacks run in a fast event; buffer APIs must run later.
  nvim.schedule(function()
    if view.replay_events ~= nil and not self.disposed and self.views[event.session_id] == view then
      view.replay_events[#view.replay_events + 1] = { event = event, state = state }
    else
      handle_event(self, view, event, state)
    end
  end)
end

---@param self louiselm.ui.Chat
---@param presentation louiselm.workflow.ParkPresentation
local function present_park(self, presentation)
  self.attention:run_parked(presentation.id)
  nvim.notify(
    string.format(
      "louiselm: budget Park: generated work %d/%d, reserved %d, pending %d, mutation %s",
      presentation.consumed,
      presentation.ceiling,
      presentation.reserved,
      #presentation.pending_mutation_ids,
      presentation.triggering_mutation_id or "none"
    ),
    nvim.log.levels.WARN
  )
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
  if self.decisions:is_active() then
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
        and key ~= "workflow"
        and key ~= "markdown_highlighting"
        and key ~= "start_insert_on_switch"
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
  local markdown_highlighting = options == nil or options.markdown_highlighting ~= false
  if options ~= nil and options.markdown_highlighting ~= nil and type(options.markdown_highlighting) ~= "boolean" then
    return nil, "chat markdown_highlighting must be a boolean"
  end
  local start_insert_on_switch = options == nil or options.start_insert_on_switch ~= false
  if options ~= nil and options.start_insert_on_switch ~= nil and type(options.start_insert_on_switch) ~= "boolean" then
    return nil, "chat start_insert_on_switch must be a boolean"
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
    markdown_highlighting = markdown_highlighting,
    start_insert_on_switch = start_insert_on_switch,
    workflow = options and options.workflow,
    attention = Attention.new(),
    usage = usage,
    views = {},
    view_order = {},
    tool_inspect_windows = {},
    limits_buffers = {},
    limits_alerts = {},
    limits_refreshing = {},
    limits_timers = {},
    limits_unsubscribe = function() end,
    winbars = {},
    winbar_targets = {},
    current_id = nil,
    disposed = false,
  }, Chat)
  chat.handoffs = Handoffs.new({
    submit = function(handoff, text, content)
      local target_view = chat.views[handoff.target_session_id]
      local contexts = {}
      if target_view ~= nil then
        local context_error = read_context_skills(target_view.draft)
        if context_error ~= nil then
          return false, context_error
        end
        content, contexts = target_view.draft:with_context(content, target_view.session:inspect().embedded_context)
      end
      local request_id, prompt_error = handoff.target_session:prompt(content)
      if request_id == nil then
        return false, prompt_error or "handoff prompt could not be sent"
      end
      if target_view ~= nil then
        if handoff.source_session_id ~= nil then
          record_handoff_prompt(target_view, text, handoff.source_session_id, contexts)
        end
        target_view.draft:clear_context()
      end
      return true
    end,
    focus = function(id)
      if chat.views[id] ~= nil then
        chat:switch(id)
      end
    end,
  })
  chat.recovery = Recovery.new({
    load_session = function(agent, acp_session_id, load_options, callback)
      return api:load_session(agent, acp_session_id, load_options, callback)
    end,
    find_run = function(id)
      for _, view in pairs(chat.views) do
        local run = view.session.owner_run
        if run ~= nil and run.id == id then
          return run
        end
      end
    end,
    is_live = function(session)
      local view = chat.views[session:inspect().id]
      return not chat.disposed and view ~= nil and view.session == session
    end,
    on_park = function(presentation)
      present_park(chat, presentation)
    end,
    on_error = function(message)
      nvim.notify("louiselm: " .. message, nvim.log.levels.ERROR)
    end,
  })
  chat.decisions = Decisions.new({
    is_live = function(session)
      local view = chat.views[session:inspect().id]
      return not chat.disposed
        and view ~= nil
        and view.session == session
        and nvim.api.nvim_buf_is_valid(view.renderer.buffer)
    end,
    report_error = function(session, message)
      local view = chat.views[session:inspect().id]
      if view ~= nil and view.session == session then
        view.renderer:append({ "Error: " .. message })
      end
    end,
    resolved = function(session, request_id)
      local state = session:inspect()
      if state.acp_session_id ~= nil then
        chat.attention:permission_resolved(state.acp_session_id, request_id)
      end
    end,
  })
  local limits_unsubscribe, limits_error = api:on_agent_limits(function(state)
    -- ACP notifications can arrive in a fast event; UI work belongs on the main loop.
    nvim.schedule(function()
      handle_limits_state(chat, state)
    end)
  end)
  if limits_unsubscribe == nil then
    return nil, limits_error or "could not observe Agent account limits"
  end
  chat.limits_unsubscribe = limits_unsubscribe
  chat.winbar_resize_autocmd = nvim.api.nvim_create_autocmd({ "VimResized", "WinResized" }, {
    callback = function()
      nvim.schedule(function()
        if not chat.disposed then
          render_winbars(chat)
        end
      end)
    end,
  })
  return chat, nil
end

---@param self louiselm.ui.Chat
---@param view louiselm.ui.ChatView
local function mark_view_seen(self, view)
  local state = view.session:inspect()
  if state.acp_session_id ~= nil then
    self.attention:seen(state.acp_session_id)
  end
end

---Attach a session to a scratch markdown buffer and optionally adopt its
---cold-resume event relay.
---@param self louiselm.ui.Chat
---@param session louiselm.session.Session Session to display.
---@param event_relay? louiselm.workflow.EventRelay Events captured before Run finalization.
---@return boolean attached
---@return string? error_message Validation or buffer creation error.
local function attach_session(self, session, event_relay)
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

  local restored_usage = {}
  local restore_error
  local replay_active = state.source == "loaded"
    and (state.status ~= "ready" or (event_relay ~= nil and event_relay.events ~= nil))
  if replay_active and state.acp_session_id ~= nil then
    local turns
    turns, restore_error = self.usage:turns(state.agent, state.acp_session_id)
    for _, record in ipairs(turns or {}) do
      restored_usage[record.turn] = record.usage
    end
  end

  local source_buffer = nvim.api.nvim_get_current_buf()
  local view = {
    session = session,
    source_buffer = source_buffer,
    draft = Draft.new(),
    setup_shown = false,
    unread_turn = false,
    replay_active = replay_active,
    replay_user_open = false,
    replay_turn = 0,
    restored_usage = restored_usage,
    replay_events = replay_active and {} or nil,
    transcript = Transcript.new(),
    unsubscribe = function() end,
    last_forensics_path = nil,
  }
  view.renderer = ChatBuffer.new(state, {
    markdown_highlighting = self.markdown_highlighting,
    on_prompt_edit = function()
      clear_queued_prompt(view)
    end,
    prompt_prefix = function()
      return view.draft.context_prefix
    end,
    on_enter = function()
      if not self.disposed and self.views[state.id] == view then
        mark_view_seen(self, view)
      end
    end,
    submit = function()
      self:submit()
    end,
  })
  local buffer = view.renderer.buffer
  if event_relay == nil then
    view.unsubscribe = session:on(function(event)
      observe_view_event(self, view, event)
    end)
  else
    view.unsubscribe = function()
      Recovery.discard_relay(event_relay)
    end
  end
  self.views[state.id] = view
  self.view_order[#self.view_order + 1] = state.id
  if event_relay ~= nil then
    local events = event_relay.events or {}
    event_relay.events = nil
    event_relay.deliver = function(event)
      observe_view_event(self, view, event)
    end
    for _, event in ipairs(events) do
      observe_view_event(self, view, event)
    end
  end
  view.renderer:header(view.session:inspect())
  self:switch(state.id)
  if state.status == "ready" and not replay_active then
    refresh_limits(self, state.agent)
  end
  if restore_error ~= nil then
    view.renderer:append({ "Error: " .. restore_error })
  end
  if replay_active then
    session:usage_history(function(records, err)
      if self.disposed or self.views[state.id] ~= view or not nvim.api.nvim_buf_is_valid(buffer) then
        local events = view.replay_events or {}
        view.replay_events = nil
        for _, queued in ipairs(events) do
          handle_event(self, view, queued.event, queued.state)
        end
        return
      end
      if err ~= nil then
        view.renderer:append({ "Recording error: " .. err.message })
      else
        local seen = {}
        for _, record in ipairs(records or {}) do
          -- Multiple durable IDs at one ordinal are ambiguous, not a choice
          -- to resolve with timestamp order or a legacy annotation.
          view.restored_usage[record.turn] = not seen[record.turn] and record.usage or nil
          seen[record.turn] = true
        end
      end
      local events = view.replay_events or {}
      view.replay_events = nil
      for _, queued in ipairs(events) do
        handle_event(self, view, queued.event, queued.state)
      end
    end)
  end
  if state.status == "ready" and not replay_active and #state.config_options > 0 then
    nvim.schedule(function()
      open_session_options(self, view, true)
    end)
  end
  return true
end

---Attach a session to a scratch markdown buffer and focus it.
---@param self louiselm.ui.Chat
---@param session louiselm.session.Session Session to display.
---@return boolean attached
---@return string? error_message Validation or buffer creation error.
function Chat:attach(session)
  return attach_session(self, session)
end

---Return the buffer for a session, or the current chat buffer.
---@param self louiselm.ui.Chat
---@param session_id? string Session id; defaults to the current session.
---@return integer? buffer
function Chat:buffer(session_id)
  local id = session_id or self.current_id
  local view = id and self.views[id]
  return view and view.renderer.buffer or nil
end

---Open an editable takeover brief from an attached source Session.
---@param self louiselm.ui.Chat
---@param target_session louiselm.session.Session
---@param source_session_id? string Defaults to the currently displayed Session.
---@return integer? buffer
---@return string? error_message Validation or Session state error.
function Chat:open_handoff(target_session, source_session_id)
  if self.disposed then
    return nil, "chat UI is disposed"
  end
  local source_id = source_session_id or self.current_id
  local source_view = source_id and self.views[source_id]
  if source_view == nil then
    return nil, "no source chat session is attached"
  end
  return self.handoffs:open(target_session, source_view.session:inspect(), source_view.transcript:snapshot())
end

---Submit the current contents of a Handoff review buffer.
---@param self louiselm.ui.Chat
---@param buffer integer
---@return boolean sent
---@return string? error_message Validation or target Session error.
function Chat:submit_handoff(buffer)
  return self.handoffs:submit(buffer)
end

---Close a Handoff review without sending.
---@param self louiselm.ui.Chat
---@param buffer integer
---@return boolean abandoned
---@return string? error_message Invalid review buffer.
function Chat:abandon_handoff(buffer)
  return self.handoffs:abandon(buffer)
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
  if view == nil or nvim.api.nvim_get_current_buf() ~= view.renderer.buffer then
    return false, "no chat session is open"
  end
  local id = view.renderer:tool_at_cursor()
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

---Report Staged context held by each attached live Session.
---@param self louiselm.ui.Chat
---@return table<louiselm.session.Session, louiselm.ui.StagedContext> staged
function Chat:staged_context()
  local staged = {}
  if self.disposed then
    return staged
  end
  for _, view in pairs(self.views) do
    if view.session:inspect().status ~= "disposed" then
      staged[view.session] = view.draft:staged_context()
    end
  end
  return staged
end

---Cold-Park the current Session through a newly admitted Run.
---@param self louiselm.ui.Chat
---@return boolean started
---@return string? error_message
function Chat:park()
  if self.disposed then
    return false, "chat UI is disposed"
  end
  local view = self.current_id and self.views[self.current_id]
  if view == nil then
    return false, "no chat session is attached"
  end
  local session = view.session
  local state = session:inspect()
  if state.acp_session_id == nil then
    return false, "current Session has no ACP Session id yet"
  end
  local staged = self:staged_context()[session]
  local staged_count = staged.contexts
  local has_staged = staged_count > 0 or staged.pending_skill or staged.queued_prompt

  local function begin()
    local started, park_error = self.recovery:park(session, function(run_id, error_message)
      if self.disposed or self.views[state.id] ~= view then
        return
      end
      if run_id == nil then
        nvim.notify("louiselm: " .. (error_message or "could not cold-Park Session"), nvim.log.levels.ERROR)
        return
      end
      self.attention:run_parked(run_id)
      nvim.notify("louiselm: Session cold-Parked", nvim.log.levels.INFO)
    end)
    if not started then
      nvim.notify("louiselm: " .. (park_error or "could not query live Beads claims"), nvim.log.levels.ERROR)
    end
  end

  if not has_staged then
    begin()
    return true
  end
  local reasons = {}
  if staged_count > 0 then
    reasons[#reasons + 1] = string.format("%d queued context item%s", staged_count, staged_count == 1 and "" or "s")
  end
  if staged.pending_skill then
    reasons[#reasons + 1] = "pending skill selection"
  end
  if staged.queued_prompt then
    reasons[#reasons + 1] = "queued prompt"
  end
  Picker.select({ "Park", "Cancel" }, {
    prompt = "louiselm: " .. table.concat(reasons, ", ") .. " will not survive cold Park; proceed? ",
  }, function(choice)
    if choice == "Park" and not self.disposed and self.views[state.id] == view then
      begin()
    end
  end)
  return true
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
  if not nvim.api.nvim_buf_is_valid(view.renderer.buffer) then
    return false, "session buffer is invalid"
  end
  local window = nvim.api.nvim_get_current_win()
  if nvim.api.nvim_win_get_config(window).relative ~= "" then
    -- A provider-owned picker can restore its buffer during BufWinEnter.
    -- Prefer the Chat's last normal window, never replace the picker buffer.
    for _, candidate in ipairs(nvim.api.nvim_tabpage_list_wins(0)) do
      if nvim.api.nvim_win_get_config(candidate).relative == "" then
        window = candidate
        if candidate == view.renderer.window then
          break
        end
      end
    end
  end
  restore_winbars(self)
  self.current_id = session_id
  view.unread_turn = false
  local state = view.session:inspect()
  mark_view_seen(self, view)
  view.renderer:show(window, self.start_insert_on_switch)
  render_winbars(self)
  return true
end

---Follow one click target from a window bar.
---@param self louiselm.ui.Chat
---@param target integer Numeric minwid encoded in the window bar.
---@param clicked_window? integer Window containing the clicked item.
---@return boolean followed
---@return string? error_message Invalid target or Session picker error.
function Chat:winbar_click(target, clicked_window)
  if self.disposed then
    return false, "chat UI is disposed"
  end
  local window = clicked_window or nvim.api.nvim_get_current_win()
  if not nvim.api.nvim_win_is_valid(window) then
    return false, "clicked window is unavailable"
  end
  local targets = self.winbar_targets[window]
  local destination = targets and targets[target]
  if destination == nil then
    return false, "window bar target is unavailable"
  end
  if window ~= nvim.api.nvim_get_current_win() then
    nvim.api.nvim_set_current_win(window)
  end
  if destination == false then
    return self:switch_session()
  end
  if type(destination) == "table" then
    return self:show_limits(destination.agent)
  end
  if type(destination) ~= "string" then
    return false, "window bar target is invalid"
  end
  return self:switch(destination)
end

---Pick one attached session using a compact state and telemetry row.
---@param self louiselm.ui.Chat
---@return boolean started
---@return string? error_message Lifecycle error.
function Chat:switch_session()
  if self.disposed then
    return false, "chat UI is disposed"
  end
  if self.decisions:is_active() then
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
  local states = {}
  for index, session in ipairs(sessions) do
    states[index] = session:inspect()
  end
  local rows = Status.session_labels(states)
  local labels = {}
  for index, session in ipairs(sessions) do
    labels[session] = rows[index]
  end
  Picker.select(sessions, {
    prompt = "louiselm session: ",
    format_item = function(session)
      return labels[session]
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

---Collect a private Forensics record and queue its path for the next prompt.
---@param self louiselm.ui.Chat
---@param callback? fun(path: string?, error_message?: string) Completion callback.
---@return boolean started
---@return string? error_message Validation or persistence failure.
function Chat:collect_forensics(callback)
  if self.disposed then
    return false, "chat UI is disposed"
  end
  local view = self.current_id and self.views[self.current_id]
  if view == nil then
    return false, "no chat session is open"
  end
  local state = view.session:inspect()
  if state.acp_session_id == nil then
    return false, "current session has no ACP session id yet"
  end
  local diagnosing_session_id = report_id(state.agent, state.acp_session_id)
  local started, start_error = self.api:collect_forensics(
    state.agent,
    state.acp_session_id,
    { diagnosing_session_id = diagnosing_session_id },
    function(path, error_message)
      nvim.schedule(function()
        if self.disposed or self.views[state.id] ~= view then
          return
        end
        if path ~= nil then
          view.last_forensics_path = path
          queue_context(self, view, { label = "forensics: " .. path, uri = "file://" .. path })
        end
        if callback ~= nil then
          callback(path, error_message)
        end
      end)
    end
  )
  return started, start_error
end

---Return the most recently collected Forensics record path for the current session.
---@param self louiselm.ui.Chat
---@return string? path
---@return string? error_message
function Chat:latest_forensics_path()
  if self.disposed then
    return nil, "chat UI is disposed"
  end
  local view = self.current_id and self.views[self.current_id]
  if view == nil then
    return nil, "no chat session is open"
  end
  if view.last_forensics_path == nil then
    return nil, "no Forensics record has been collected for this session yet"
  end
  return view.last_forensics_path, nil
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
  local ok, result = pcall(nvim.fn.writefile, nvim.split(markdown, "\n", { plain = true }), destination)
  if not ok or result ~= 0 then
    return nil, "could not write markdown file: " .. destination
  end
  return destination
end

---@param self louiselm.ui.Chat
---@param view louiselm.ui.ChatView
local function close_view(self, view)
  local id = view.session:inspect().id
  local state = view.session:inspect()
  if state.acp_session_id ~= nil then
    self.attention:session_disposed(state.acp_session_id)
  end
  restore_winbars(self)
  clear_queued_prompt(view)
  view.unsubscribe()
  self.views[id] = nil
  self.decisions:retire(view.session)
  for index, candidate in ipairs(self.view_order) do
    if candidate == id then
      table.remove(self.view_order, index)
      break
    end
  end
  local _, close_error = view.session:dispose()
  self.current_id = nil
  for _, candidate in ipairs(self.api:list_sessions()) do
    if self.views[candidate] ~= nil then
      self:switch(candidate)
      break
    end
  end
  -- Select the survivor before deleting the buffer in its host window.
  view.renderer:dispose()
  if close_error ~= nil then
    nvim.notify("louiselm: " .. close_error, nvim.log.levels.ERROR)
  end
  self.decisions:present_next()
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
  local has_queued_prompt = view.draft.queued_prompt ~= nil
  if
    has_queued_prompt
    or status == "configuring"
    or status == "preparing"
    or status == "prompting"
    or status == "waiting_permission"
    or status == "cancelling"
  then
    local prompt = has_queued_prompt and "close active louiselm session and discard queued prompt? "
      or "close active louiselm session? "
    -- A second ui.select can toggle away the permission picker without opening
    -- a confirmation. Native confirm leaves that decision alone on Keep/Escape.
    nvim.cmd.stopinsert()
    local choice = nvim.fn.confirm(prompt, "&Close\n&Keep", 2)
    if choice == 1 and not self.disposed and self.views[view.session:inspect().id] == view then
      close_view(self, view)
    end
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
  if self.decisions:is_active() then
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
    return false, "session has no standard ACP options"
  end
  open_session_options(self, view, false)
  return true
end

---Open and refresh the account-limit detail view for one configured Agent.
---@param self louiselm.ui.Chat
---@param agent_name? string Configured Agent; defaults to the active Session's Agent.
---@return boolean opened
---@return string? error_message Validation or API error.
function Chat:show_limits(agent_name)
  if self.disposed then
    return false, "chat UI is disposed"
  end
  if agent_name == nil then
    local view = self.current_id and self.views[self.current_id]
    if view == nil then
      return false, "no chat session is attached; pass an Agent name"
    end
    agent_name = view.session:inspect().agent
  end
  local state, inspect_error = self.api:inspect_agent_limits(agent_name)
  if state == nil then
    return false, inspect_error or "could not inspect Agent account limits"
  end
  local previous = self.limits_buffers[agent_name]
  if previous ~= nil and nvim.api.nvim_buf_is_valid(previous) then
    nvim.api.nvim_buf_delete(previous, { force = true })
  end
  local buffer = Limits.open(agent_name, state)
  self.limits_buffers[agent_name] = buffer
  refresh_limits(self, agent_name)
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
  if view.replay_events ~= nil then
    return nil, "Session history is still loading"
  end
  if text ~= nil and type(text) ~= "string" then
    return nil, "prompt must be a non-empty string"
  end
  local visible_text = view.draft:reconcile_skills(view.renderer:prompt_text())
  local from_buffer = text == nil
  if from_buffer then
    text = visible_text
  end
  local text_error
  text, text_error = view.draft:prompt_text(text, from_buffer)
  if text == nil then
    return nil, text_error
  end
  local status = view.session:inspect().status
  if ACTIVE_TURN_STATUS[status] then
    queue_prompt(view, text)
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

---Queue the source buffer's error and warning diagnostics for the next prompt.
---@param self louiselm.ui.Chat
---@return boolean queued
---@return string? error_message Context or lifecycle error.
function Chat:mention_diagnostics()
  local view = self.current_id and self.views[self.current_id]
  if view == nil then
    return false, "no chat session is attached"
  end
  return queue_context(self, view, Context.diagnostics.context(view.source_buffer))
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
  if self.decisions:is_active() then
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
  if self.decisions:is_active() then
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
        phase = skill.phase,
      }
      local queued, queue_error = queue_skill(self, view, selected, state.skills_policy == "native")
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
      if self.decisions:is_active() then
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
    self.views[session:inspect().id].draft:set_catalog(self.skill_catalog)
  end
  if self.instructions_context ~= nil then
    local queued, queue_error = self:queue_context(self.instructions_context)
    if not queued then
      return nil, queue_error
    end
  end
  return session
end

---Hand the current session's reviewed transcript off to a configured Agent:
---pick a target Agent, create and seed its session the same way
---`new_session` does, then open the editable transcript review buffer.
---@param self louiselm.ui.Chat
---@return boolean started
---@return string? error_message Validation or session-state error.
function Chat:hand_off()
  if self.disposed then
    return false, "chat UI is disposed"
  end
  if self.decisions:is_active() then
    return false, DECISION_OPEN_ERROR
  end
  local source_id = self.current_id
  local source_view = source_id and self.views[source_id]
  if source_view == nil then
    return false, "no chat session is attached"
  end
  local source_state = source_view.session:inspect()
  if (source_state.status ~= "ready" and source_state.status ~= "error") or source_view.draft.queued_prompt ~= nil then
    return false, "current session has an active turn; finish or cancel it before handing off"
  end
  if #self.agents < 2 then
    return false, "handoff requires at least two configured agents"
  end

  local candidates = self.agents

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
  if self.decisions:is_active() then
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

---Discover and load a durable cold-Parked Run through an operator picker.
---@param self louiselm.ui.Chat
---@return boolean started
---@return string? error_message
function Chat:resume_park()
  if self.disposed then
    return false, "chat UI is disposed"
  end
  return self.recovery:list(function(runs, list_error)
    if self.disposed then
      return
    end
    if list_error ~= nil or runs == nil then
      nvim.notify("louiselm: " .. (list_error or "could not list cold Parks"), nvim.log.levels.ERROR)
      return
    end
    if #runs == 0 then
      nvim.notify("louiselm: no durable cold Parks found", nvim.log.levels.INFO)
      return
    end
    Picker.select(runs, {
      prompt = "louiselm cold Park: ",
      format_item = function(run)
        return string.format("%s/%s · %s · cwd=%s", run.agent, run.acp_session_id, run.id, run.cwd)
      end,
    }, function(selected)
      if selected == nil or self.disposed then
        return
      end
      local started, resume_error = self.recovery:resume(selected, function(result, error_message)
        if result == nil then
          nvim.notify("louiselm: " .. (error_message or "could not resume cold Park"), nvim.log.levels.ERROR)
          return
        end
        local attached, attach_error = attach_session(self, result.session, result.relay)
        if not attached then
          Recovery.discard_relay(result.relay)
          result.session:dispose()
          nvim.notify("louiselm: " .. (attach_error or "could not attach loaded session"), nvim.log.levels.ERROR)
          return
        end
        self.attention:run_resumed(selected.id)
        nvim.notify("louiselm: cold Park resumed (recoverable, lossy)", nvim.log.levels.INFO)
      end)
      if not started then
        nvim.notify("louiselm: " .. (resume_error or "could not resume cold Park"), nvim.log.levels.ERROR)
      end
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
  if self.winbar_resize_autocmd ~= nil then
    nvim.api.nvim_del_autocmd(self.winbar_resize_autocmd)
    self.winbar_resize_autocmd = nil
  end
  self.attention:dispose()
  self.recovery:dispose()
  self.limits_unsubscribe()
  for agent_name in pairs(self.limits_timers) do
    close_limits_timer(self, agent_name)
  end
  restore_winbars(self)
  self.decisions:dispose()
  for window in pairs(self.tool_inspect_windows) do
    if nvim.api.nvim_win_is_valid(window) then
      nvim.api.nvim_win_close(window, true)
    end
  end
  self.tool_inspect_windows = {}
  for _, buffer in pairs(self.limits_buffers) do
    if nvim.api.nvim_buf_is_valid(buffer) then
      nvim.api.nvim_buf_delete(buffer, { force = true })
    end
  end
  self.limits_buffers = {}
  self.handoffs:dispose()
  for id, view in pairs(self.views) do
    clear_queued_prompt(view)
    view.unsubscribe()
    view.renderer:dispose()
    self.views[id] = nil
  end
  self.current_id = nil
  self.view_order = {}
  self.winbar_targets = {}
  return true
end

return M
