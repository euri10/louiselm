local Context = require("louiselm.ui.context")
local Attention = require("louiselm.ui.attention")
local Diff = require("louiselm.ui.diff")
local Gates = require("louiselm.permission.gates")
local Limits = require("louiselm.ui.limits")
local Picker = require("louiselm.ui.picker")
local Skills = require("louiselm.skills")
local Transcript = require("louiselm.session.transcript")
local Usage = require("louiselm.routing.usage")
local Workflow = require("louiselm.workflow")
local ParkObserver = require("louiselm.workflow.park_observer")
local ResumeController = require("louiselm.workflow.resume_controller")
local RunClient = require("louiselm.workflow.run_client")
local WorkflowService = require("louiselm.workflow.service")

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
---@field pending_terminal_completion? string Text from a terminal completion tool, flushed at turn end or immediately if it arrives after turn end.
---@field turn_prose string Assistant chunk text rendered during the current turn; suppresses a terminal-completion echo only when that exact text was already shown.
---@field turn_done_fired boolean Whether `turn_done` already ran for the current turn; a terminal completion arriving after this flushes immediately instead of waiting for a `turn_done` that already passed.
---@field last_block_kind ("prose"|"tool"|"reasoning")? Kind of the most recently rendered transcript block; separates adjacent prose, reasoning, and tool blocks with a blank line.
---@field trailing_blank boolean Whether the line at `transcript_tail` is already a blank separator, counted as part of `transcript_tail` itself; meaningful only while `last_block_kind == "prose"`, since a completed prose block is the only thing that always leaves one behind.
---@field tool_lines table<string, integer> Zero-based rendered tool lines by ID.
---@field tool_ids table<integer, string> Tool-call IDs by zero-based rendered line.
---@field tool_statuses table<string, string> Latest tool status by ID.
---@field tool_titles table<string, string> Tool titles by ID.
---@field contexts louiselm.ui.ContextItem[] Context items queued for the next prompt.
---@field context_prefix string Visible context markers prefixed to the prompt.
---@field skill_catalog? string Hidden catalog pending for this new inject session.
---@field pending_skill? louiselm.skills.Skill Native-mode skill selection, resolved against advertised commands only at actual submission.
---@field workflow_phase? louiselm.routing.PhaseMetadata Last phase-tagged skill used by this view.
---@field context_folds louiselm.ui.ContextFold[] Submitted context fold ranges in this live buffer.
---@field fold_counts table<integer, integer> Number of context folds installed in each window.
---@field tool_folds louiselm.ui.ToolFold[] Completed tool-call fold ranges in this live buffer.
---@field tool_fold_counts table<integer, integer> Number of tool folds installed in each window.
---@field tool_fold_run louiselm.ui.ToolFoldRun? Contiguous rendered tool paragraph awaiting a boundary.
---@field thought_folds louiselm.ui.ThoughtFold[] Reasoning fold ranges in this live buffer.
---@field thought_run louiselm.ui.ThoughtFoldRun? Contiguous reasoning paragraph awaiting a boundary; first is its header line, last its final content line.
---@field tool_inspect_windows table<integer, boolean> Floating raw-payload windows owned by this chat.
---@field queued_prompt louiselm.ui.QueuedPrompt? Prompt committed for the next completed turn.
---@field queue_mark integer? Extmark showing queued prompt state.
---@field queue_namespace integer Extmark namespace for queued prompt state.
---@field setup_shown boolean Whether the initial options overview was offered.
---@field options_revision? integer Invalidates pending option-history pickers.
---@field unread_turn boolean Whether a completed background response has not been focused.
---@field replay_active boolean Whether session/load history is still arriving.
---@field replay_user_open boolean Whether consecutive replayed user chunks belong to the current historical turn.
---@field replay_prompt_mark integer? Range extmark for the current replayed user turn.
---@field replay_turn integer Number of historical user turns replayed into this view.
---@field restored_usage table<integer, louiselm.session.TurnUsage> Persisted presentation usage by historical turn.
---@field replay_events? { event: louiselm.session.Event, state: louiselm.session.State? }[] UI events waiting for asynchronous usage history.
---@field transcript louiselm.session.Transcript Full, untruncated record of this session's turns.
---@field unsubscribe fun() Session event listener removal function.
---@field last_forensics_path string? Path of the most recently collected Forensics record for this session.

---@class louiselm.ui.QueuedPrompt
---@field text string User-authored prompt text without visible context markers.

---@class louiselm.ui.StagedContext
---@field contexts integer Number of queued context items.
---@field pending_skill boolean Whether a native-mode skill selection is pending.
---@field queued_prompt boolean Whether a prompt is queued behind the active turn.

---@class louiselm.ui.ChatEventRelay
---@field events louiselm.session.Event[]? Events waiting for a cold-resumed view.
---@field chat louiselm.ui.Chat? Bound chat after Run finalization.
---@field view louiselm.ui.ChatView? Bound view after Run finalization.

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
---@field diff louiselm.ui.Diff File-edit review UI.
---@field usage louiselm.routing.Usage Persistent measured usage ledger.
---@field workflow? louiselm.routing.Coordinator Phase-aware routing coordinator.
---@field decision_active? louiselm.ui.ChatDecision Permission decision currently presented.
---@field decision_picker? louiselm.ui.ChatDecision Provider-owned picker awaiting its callback, even after retirement.
---@field decision_queue louiselm.ui.ChatDecision[] Permission decisions waiting for the open one.
---@field queue_namespace integer Extmark namespace for queued prompt indicators.
---@field prompt_namespace integer Extmark namespace for prompt boundaries.
---@field header_namespace integer Highlight namespace for session diagnostics.
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
---@field handoffs table<integer, louiselm.ui.Handoff>
---@field resume_client? louiselm.workflow.RunClient Client for durable Run mutations.
---@field resume_controller? louiselm.workflow.ResumeController Operator resume orchestration.
---@field park_observer? louiselm.workflow.ParkObserver Reconciles durable Park snapshots into live Runs.
---@field resume_initializing boolean Whether the durable Run client is connecting.
---@field resume_waiters fun(controller: louiselm.workflow.ResumeController?, error_message?: string)[] Callbacks waiting for the durable Run client.
---@field resume_revisions table<string, integer> Revisions from the authoritative Run socket snapshot.
---@field resume_summaries table<string, louiselm.workflow.ParkSummary> Selected durable Run metadata.
---@field pending_resume_runs table<string, louiselm.workflow.Run> Locally reconstructed cold Runs awaiting finalization.
---@field pending_resume_sessions table<string, louiselm.session.Session> Loaded Sessions awaiting finalization.
---@field pending_resume_relays table<string, louiselm.ui.ChatEventRelay> Session event relays awaiting finalization.
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

local PARK_GENERATED_WORK_MAX = 1
local PARK_TTL_MS = 24 * 60 * 60 * 1000

---@return string run_id
local function park_run_id()
  local seed = table.concat({ tostring(nvim.uv.hrtime()), tostring(nvim.fn.getpid()), nvim.fn.tempname() }, ":")
  local hex = nvim.fn.sha256(seed)
  local variant = string.format("%x", 8 + (tonumber(hex:sub(17, 17), 16) % 4))
  return table.concat({
    hex:sub(1, 8),
    hex:sub(9, 12),
    "4" .. hex:sub(14, 16),
    variant .. hex:sub(18, 20),
    hex:sub(21, 32),
  }, "-")
end

local function capture_state_root()
  local root = nvim.env.LOUISELM_CAPTURE_STATE_DIR
  if root == nil or root == "" then
    root = nvim.env.XDG_STATE_HOME
  end
  if root == nil or root == "" then
    root = nvim.fs.joinpath(nvim.fn.expand("~"), ".local", "state")
  end
  return root
end

local function resume_paths()
  local root = capture_state_root()
  local workflow = nvim.fs.joinpath(root, "louiselm", "workflow")
  return nvim.fs.joinpath(workflow, "run.sock"), nvim.fs.joinpath(workflow, "operator-capability")
end

---@param session_id string Agent-scoped Session identity.
---@param cwd string Beads workspace directory.
---@param callback fun(claims: string[], error_message?: string)
---@return boolean started
---@return string? error_message
local function live_claims(session_id, cwd, callback)
  local started = pcall(nvim.system, { "br", "list", "--assignee", session_id, "--status", "in_progress", "--json" }, {
    text = true,
    cwd = cwd,
  }, function(result)
    nvim.schedule(function()
      if result.code ~= 0 then
        callback({}, result.stderr ~= "" and result.stderr or "could not query live Beads claims")
        return
      end
      local decoded_ok, decoded = pcall(nvim.json.decode, result.stdout)
      if not decoded_ok or type(decoded) ~= "table" or type(decoded.issues) ~= "table" then
        callback({}, "br returned malformed claim data")
        return
      end
      local claims = {}
      for _, issue in ipairs(decoded.issues) do
        if type(issue) ~= "table" or type(issue.id) ~= "string" or issue.id == "" then
          callback({}, "br returned malformed claim data")
          return
        end
        claims[#claims + 1] = issue.id
      end
      callback(claims)
    end)
  end)
  if not started then
    return false, "could not start Beads claim query"
  end
  return true
end

---@class louiselm.ui.Handoff
---@field target_session louiselm.session.Session
---@field target_session_id string
---@field source_session_id? string Durable source Session identity.

---@class louiselm.ui.LimitsTarget
---@field agent string

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
  preparing = "LouiselmStatusActive",
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

-- Must be a plain dotted name Vim can resolve at click-dispatch time, not a
-- call expression: `v:lua.require('...').winbar_click` is silently never
-- invoked on a real click (louiselm-7ios). `command.lua` registers this
-- global as a stable forwarder to its own reassignable `M.winbar_click`.
local WINBAR_CLICK_HANDLER = "v:lua.__louiselm_winbar_click"
local LIMITS_CLICK_TARGET = 99

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
  preparing = true,
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
---@return table<string, string>
local function session_cells(state)
  local cells = {
    id = state.id,
    name = state.name ~= state.id and state.name or "",
    status = state.status or "unknown",
    skills = state.skills_policy and "skills: " .. state.skills_policy or "",
    agent = state.agent,
    loaded = state.source == "loaded" and "loaded" or "",
    identity = state.acp_session_id and report_id(state.agent, state.acp_session_id) or "",
  }
  if state.activity ~= nil then
    cells.activity = "activity=" .. state.activity
  end
  if state.context ~= nil then
    local stale = state.context.stale and " stale" or ""
    cells.context = string.format(
      "context=%s/%s (%.0f%%%s)",
      format_number(state.context.used),
      format_number(state.context.size),
      state.context.percentage,
      stale
    )
  end
  if state.cost ~= nil then
    cells.cost = "cost=" .. format_number(state.cost.amount) .. " " .. state.cost.currency
  end
  return cells
end

---@param option louiselm.session.ConfigOption
---@return string
local function session_option_column(option)
  -- Agents use different ids/labels for effort, but advertise its shared meaning.
  if option.category == "thought_level" then
    return "category:thought_level"
  end
  -- model_config groups distinct controls; only matching labels share a column.
  if option.category == "model_config" then
    return "category:model_config:" .. option.name
  end
  return "option:" .. option.id
end

---@param sessions louiselm.session.Session[]
---@return fun(session: louiselm.session.Session): string
local function session_formatter(sessions)
  local columns = { "id", "name", "status", "skills", "agent", "loaded" }
  local seen_options = {}
  local rows = {}
  local states = {}
  local ambiguous = {}
  -- Freeze one opening so filtering and repeated rendering use the same widths/values.
  for index, session in ipairs(sessions) do
    local state = session:inspect()
    states[index] = state
    local seen = {}
    for _, option in ipairs(state.config_options or {}) do
      local column = session_option_column(option)
      if seen[column] then
        ambiguous[column] = true
      end
      seen[column] = true
    end
  end
  for index, state in ipairs(states) do
    rows[index] = session_cells(state)
    for _, option in ipairs(state.config_options or {}) do
      local column = session_option_column(option)
      -- Ambiguous semantic matches fall back for the whole opening; retain every control.
      if ambiguous[column] then
        column = "option:" .. option.id
      end
      rows[index][column] = option.name .. "=" .. option_display_value(option)
      if not seen_options[column] then
        seen_options[column] = true
        columns[#columns + 1] = column
      end
    end
  end
  nvim.list_extend(columns, { "activity", "context", "cost", "identity" })

  local widths = {}
  for _, row in ipairs(rows) do
    for _, column in ipairs(columns) do
      -- Tabs depend on their starting column; normalize them before measuring cells.
      local value = single_line(row[column] or ""):gsub("\t", " ")
      row[column] = value
      widths[column] = math.max(widths[column] or 0, nvim.fn.strdisplaywidth(value))
    end
  end

  local labels = {}
  for index, row in ipairs(rows) do
    local parts = {}
    for _, column in ipairs(columns) do
      local width = widths[column]
      if width > 0 then
        local value = row[column]
        parts[#parts + 1] = value .. string.rep(" ", width - nvim.fn.strdisplaywidth(value))
      end
    end
    labels[sessions[index]] = table.concat(parts, " · ")
  end
  return function(session)
    return labels[session]
  end
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

---@param state louiselm.session.State
---@return string
local function turn_label(state)
  if state.recording_error ~= nil then
    return "Recording failed — retry to recover"
  end
  if state.status == "preparing" then
    return "Preparing turn"
  end
  if state.status == "ready" then
    return "Your turn"
  end
  if state.status == "prompting" then
    if state.session_failure ~= nil then
      return single_line(state.session_failure.title)
    end
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
---@return string group
local function turn_highlight(state)
  if state.status == "prompting" and state.session_failure ~= nil then
    return state.session_failure.severity == "error" and "LouiselmStatusError" or "LouiselmStatusWarning"
  end
  return STATUS_HIGHLIGHTS[state.status] or "LouiselmStatusWarning"
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
  add_header_highlight(highlights, 1, display_start, display_text, turn_highlight(state))

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

---@class louiselm.ui.WinbarEntry
---@field id string
---@field group string
---@field text string
---@field attention boolean
---@field overflow_glyph? string Glyph for a collapsible entry; absent for pinned permission/error entries.

---@param view louiselm.ui.ChatView
---@return louiselm.ui.WinbarEntry entry
local function background_winbar_entry(view)
  local state = view.session:inspect()
  local label = state.name ~= nil and state.name ~= "" and state.name or state.agent
  label = single_line(label)
  if state.status == "waiting_permission" then
    return { id = state.id, group = "LouiselmStatusWarning", text = "! " .. label, attention = true }
  end
  if state.status == "error" then
    return { id = state.id, group = "LouiselmStatusError", text = "✗ " .. label, attention = true }
  end
  if view.unread_turn then
    return {
      id = state.id,
      group = "LouiselmStatusWarning",
      text = "● " .. label,
      attention = true,
      overflow_glyph = "●",
    }
  end
  if state.status == "ready" then
    return {
      id = state.id,
      group = "LouiselmStatusReady",
      text = "● " .. label,
      attention = false,
      overflow_glyph = "●",
    }
  end
  if state.status == "disposed" then
    return { id = state.id, group = "LouiselmStatusWarning", text = "✗ " .. label, attention = true }
  end
  return {
    id = state.id,
    group = STATUS_HIGHLIGHTS[state.status] or "LouiselmStatusWarning",
    text = "… " .. label,
    attention = false,
    overflow_glyph = "…",
  }
end

---@param entries louiselm.ui.WinbarEntry[]
---@param visible integer Number of collapsible entries to keep individually labelled.
---@return louiselm.ui.WinbarEntry[]
local function collapsed_winbar_entries(entries, visible)
  local result = {} ---@type louiselm.ui.WinbarEntry[]
  local summaries = {} ---@type louiselm.ui.WinbarEntry[]
  local by_state = {} ---@type table<string, louiselm.ui.WinbarEntry>
  local counts = {} ---@type table<string, integer>
  local pinned = {} ---@type louiselm.ui.WinbarEntry[]
  for _, entry in ipairs(entries) do
    if entry.overflow_glyph == nil then
      pinned[#pinned + 1] = entry
    elseif visible > 0 then
      result[#result + 1] = entry
      visible = visible - 1
    else
      -- Color distinguishes unseen/seen dots; glyph distinguishes cancellation from completion.
      local key = entry.group .. entry.overflow_glyph
      local summary = by_state[key]
      if summary == nil then
        summary = { id = "", group = entry.group, text = "", attention = entry.attention }
        by_state[key] = summary
        summaries[#summaries + 1] = summary
      end
      counts[key] = (counts[key] or 0) + 1
      summary.text = "+" .. counts[key] .. entry.overflow_glyph
    end
  end
  nvim.list_extend(result, summaries)
  nvim.list_extend(result, pinned)
  return result
end

---@param entries louiselm.ui.WinbarEntry[]
---@return integer width
local function winbar_entries_width(entries)
  local width = math.max(0, #entries - 1) * 3
  for _, entry in ipairs(entries) do
    width = width + nvim.fn.strdisplaywidth(entry.text)
  end
  return width
end

---@param target integer
---@param group string
---@param text string
---@return string
local function clickable_winbar_segment(target, group, text)
  return "%" .. target .. "@" .. WINBAR_CLICK_HANDLER .. "@" .. winbar_segment(group, text) .. "%X"
end

---@param self louiselm.ui.Chat
---@param state louiselm.session.State
---@return string winbar
---@return string? limits_agent
local function session_winbar(self, state)
  local identity = state.acp_session_id and report_id(state.agent, state.acp_session_id) or state.agent
  local fields = {
    winbar_segment(turn_highlight(state), turn_label(state)),
    -- Truncate identity/telemetry before clipping background attention on the right.
    "%<" .. winbar_segment("Normal", identity),
  }
  local limits_state = self.api:inspect_agent_limits(state.agent)
  local limits_text
  local limits_group
  if limits_state ~= nil then
    limits_text, limits_group = Limits.summary(limits_state)
  end
  if limits_text ~= nil and limits_group ~= nil then
    fields[#fields + 1] = clickable_winbar_segment(LIMITS_CLICK_TARGET, limits_group, limits_text)
  end
  if
    state.name ~= nil
    and state.name ~= ""
    and state.name ~= state.id
    and state.name ~= state.acp_session_id
    and state.name ~= state.agent
    and state.name ~= identity
  then
    fields[#fields + 1] = winbar_segment("Normal", state.name)
  end
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
  return table.concat(fields, " · "), limits_text ~= nil and state.agent or nil
end

---@param self louiselm.ui.Chat
---@param view louiselm.ui.ChatView
---@param win integer
---@return string
local function chat_winbar(self, view, win)
  local base, limits_agent = session_winbar(self, view.session:inspect())
  local quiet = {} ---@type louiselm.ui.WinbarEntry[]
  local attention = {} ---@type louiselm.ui.WinbarEntry[]
  local current_id = view.session:inspect().id
  for _, id in ipairs(self.view_order) do
    local background = self.views[id]
    if id ~= current_id and background ~= nil then
      local entry = background_winbar_entry(background)
      local entries = entry.attention and attention or quiet
      entries[#entries + 1] = entry
    end
  end
  local targets = {} ---@type table<integer, string|false|louiselm.ui.LimitsTarget>
  if limits_agent ~= nil then
    targets[LIMITS_CLICK_TARGET] = { agent = limits_agent }
  end
  if #quiet == 0 and #attention == 0 then
    self.winbar_targets[win] = targets
    return base
  end

  local available = nvim.api.nvim_win_get_width(win)
    - nvim.api.nvim_eval_statusline(base, { winid = win, use_winbar = true }).width
  local entries = {} ---@type louiselm.ui.WinbarEntry[]
  for _, entry in ipairs(quiet) do
    entries[#entries + 1] = entry
  end
  for _, entry in ipairs(attention) do
    entries[#entries + 1] = entry
  end
  if winbar_entries_width(entries) > available then
    local all = entries
    entries = collapsed_winbar_entries(all, 0)
    local visible = 0
    for _, entry in ipairs(all) do
      if entry.overflow_glyph ~= nil then
        visible = visible + 1
        local candidate = collapsed_winbar_entries(all, visible)
        if winbar_entries_width(candidate) > available then
          break
        end
        entries = candidate
      end
    end
  end

  local segments = {}
  for index, entry in ipairs(entries) do
    targets[index] = entry.id ~= "" and entry.id or false
    segments[index] = clickable_winbar_segment(index, entry.group, entry.text)
  end
  self.winbar_targets[win] = targets
  return base .. "%=" .. table.concat(segments, " · ")
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
  nvim.api.nvim_set_option_value("winbar", chat_winbar(self, view, win), { win = win })
end

---@param self louiselm.ui.Chat
local function render_winbars(self)
  for _, win in ipairs(nvim.api.nvim_list_wins()) do
    local buffer = nvim.api.nvim_win_get_buf(win)
    for _, id in ipairs(self.view_order) do
      local view = self.views[id]
      if view ~= nil and view.buffer == buffer then
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
  view.workflow_phase = skill.phase
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
  view.workflow_phase = skill.phase
  if skill.content == nil then
    return true, "could not read selected skill: " .. skill.path
  end
  return true
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

---@class louiselm.ui.ThoughtFoldRun
---@field first integer Zero-based rendered reasoning header line.
---@field last integer Zero-based last rendered reasoning content line.

---@class louiselm.ui.ThoughtFold
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
---@param folds louiselm.ui.ContextFold[]
---@param counts table<integer, integer>
local function apply_incremental_folds(view, win, folds, counts)
  if not nvim.api.nvim_win_is_valid(win) or nvim.api.nvim_win_get_buf(win) ~= view.buffer then
    return
  end
  nvim.api.nvim_set_option_value("foldmethod", "manual", { win = win })
  nvim.api.nvim_set_option_value("foldenable", true, { win = win })
  local applied = counts[win] or 0
  nvim.api.nvim_win_call(win, function()
    for index = applied + 1, #folds do
      local fold = folds[index]
      nvim.api.nvim_cmd({ cmd = "fold", range = { fold.first + 1, fold.last + 1 } }, {})
    end
  end)
  counts[win] = #folds
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
    apply_incremental_folds(view, view.window, view.tool_folds, view.tool_fold_counts)
  end
  view.tool_fold_run = nil
end

---Reinstall every recorded reasoning fold that is not currently a real closed
---fold in `win`. Unlike the incremental context/tool fold appliers, this
---rescans the full `thought_folds` history on every call instead of trusting
---a "folds already installed" counter: something outside this module's
---control can silently drop a manual fold (observed for replayed reasoning
---paragraphs during `session/load`, louiselm-9wjm), and a counter that only
---ever advances has no way to notice or recover from that. Checking
---`foldclosed` first keeps repeated calls idempotent -- re-issuing `:fold` on
---a range that is already folded nests a second fold inside the first rather
---than being a no-op.
---@param view louiselm.ui.ChatView
---@param win integer
local function apply_thought_folds(view, win)
  if not nvim.api.nvim_win_is_valid(win) or nvim.api.nvim_win_get_buf(win) ~= view.buffer then
    return
  end
  nvim.api.nvim_set_option_value("foldmethod", "manual", { win = win })
  nvim.api.nvim_set_option_value("foldenable", true, { win = win })
  nvim.api.nvim_win_call(win, function()
    for _, fold in ipairs(view.thought_folds) do
      if nvim.fn.foldclosed(fold.first + 1) == -1 then
        nvim.api.nvim_cmd({ cmd = "fold", range = { fold.first + 1, fold.last + 1 } }, {})
      end
    end
  end)
end

---Close the current reasoning paragraph: fold the `[thinking]` header together
---with its content lines. Neovim cannot close a fold spanning a single line, so
---the header is folded along with the content rather than left outside it;
---Neovim's default foldtext then renders the fold's first line, `[thinking]`,
---as the closed summary. A paragraph with no content lines yet has nothing to
---fold.
---@param view louiselm.ui.ChatView
local function close_thought_fold_run(view)
  local run = view.thought_run
  if run == nil then
    return
  end
  if run.last > run.first then
    view.thought_folds[#view.thought_folds + 1] = { first = run.first, last = run.last }
    apply_thought_folds(view, view.window)
  end
  view.thought_run = nil
end

---@param view louiselm.ui.ChatView
local function toggle_chat_fold(view)
  local line = nvim.api.nvim_win_get_cursor(view.window)[1] - 1
  local run = view.thought_run
  if run ~= nil and line == run.first and nvim.fn.foldlevel(line + 1) == 0 then
    return
  end
  nvim.cmd("normal! za")
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
  local lines = nvim.split(nvim.inspect(entry.raw or {}), "\n", { plain = true })
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
  local function close()
    self.tool_inspect_windows[window] = nil
    if nvim.api.nvim_buf_is_valid(buffer) then
      nvim.api.nvim_buf_delete(buffer, { force = true })
    end
  end
  nvim.keymap.set("n", "q", close, { buffer = buffer, silent = true, nowait = true, desc = "Close tool inspector" })
  nvim.keymap.set("n", "<Esc>", close, { buffer = buffer, silent = true, nowait = true, desc = "Close tool inspector" })
  return true
end

---@param view louiselm.ui.ChatView
---@param first_line integer Inclusive zero-based start.
---@param end_line integer Exclusive zero-based end.
---@param id? integer Existing range to extend.
---@return integer mark_id
local function mark_submitted_prompt(view, first_line, end_line, id)
  return nvim.api.nvim_buf_set_extmark(view.buffer, view.prompt_namespace, first_line, 0, {
    id = id,
    end_row = end_line,
    end_col = 0,
    right_gravity = false,
    end_right_gravity = false,
    invalidate = true,
  })
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
        nvim.list_extend(lines, nvim.split(item.text, "\n", { plain = true }))
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
  local first_prompt_line = view.prompt_line + #lines
  for _, line in ipairs(nvim.split(text, "\n", { plain = true })) do
    lines[#lines + 1] = "> " .. line
  end
  nvim.api.nvim_buf_set_lines(view.buffer, view.prompt_line, -1, false, lines)
  mark_submitted_prompt(view, first_prompt_line, view.prompt_line + #lines)
  apply_incremental_folds(view, view.window, view.context_folds, view.fold_counts)
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
local function reconcile_prompt_boundary(view)
  -- Undo restores the extmark but not these Lua-side indexes.
  if view.prompt_line < nvim.api.nvim_buf_line_count(view.buffer) then
    return
  end
  local prompt_line = current_prompt_line(view)
  view.transcript_tail = prompt_line - 1
  view.response_line = nil
  view.response_tail = nil
  view.response_started = false
  view.last_block_kind = nil
  view.trailing_blank = false
  view.thought_run = nil
  view.tool_fold_run = nil
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
  local lines = nvim.split(text, "\n", { plain = true })
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
---@param title string? Tool title from this update or its preceding start event.
---@return string? text
local function terminal_completion_text(value, title)
  if type(value) ~= "table" or title ~= "task_complete" then
    return nil
  end
  local raw_output = value.rawOutput
  if type(raw_output) ~= "table" or type(raw_output.content) ~= "string" or raw_output.content == "" then
    return nil
  end
  return raw_output.content
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
    if sent then
      local state = decision.view.session:inspect()
      if state.acp_session_id ~= nil then
        self.attention:permission_resolved(state.acp_session_id, decision.data.request_id)
      end
    end
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
  self.decision_picker = decision
  local function picker_respond(result, rpc_error)
    if self.decision_picker == decision then
      self.decision_picker = nil
    end
    local sent, send_error = respond(result, rpc_error)
    pump_decisions(self)
    return sent, send_error
  end
  local call_ok, prompt_error = pcall(prompt_permission, self, view, data, picker_respond)
  if not call_ok then
    insert_transcript(self, view, { "Error: permission picker failed: " .. tostring(prompt_error) })
    cancel_permission(self, view, picker_respond)
  end
end

---Present the next queued decision while none is open.
---One picker or review hosts one decision at a time across every attached session,
---because an async select provider closes the picker a new one replaces, which answers
---that request without the user choosing and can leave the replacement unanswered.
---@param self louiselm.ui.Chat
pump_decisions = function(self)
  while self.decision_active == nil and (self.decision_picker == nil or self.disposed) do
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
---@return integer insertion_line
insert_transcript = function(self, view, lines)
  local replacement = {}
  for _, line in ipairs(lines) do
    for _, part in ipairs(nvim.split(line, "\n", { plain = true })) do
      replacement[#replacement + 1] = part
    end
  end
  local insertion_line = view.transcript_tail == nil and view.prompt_line or view.transcript_tail + 1
  nvim.api.nvim_buf_set_lines(view.buffer, insertion_line, insertion_line, false, replacement)
  view.transcript_tail = insertion_line + #replacement - 1
  mark_prompt(view, view.prompt_line + #replacement)
  return insertion_line
end

---Render a terminal-completion tool's retained text inline unless that exact
---text was already streamed during the current turn.
---@param self louiselm.ui.Chat
---@param view louiselm.ui.ChatView
---@param text string
local function flush_terminal_completion(self, view, text)
  if view.turn_prose:find(text, 1, true) ~= nil then
    return
  end
  local lines = {}
  if view.last_block_kind == "tool" or view.last_block_kind == "reasoning" then
    lines[#lines + 1] = ""
  end
  nvim.list_extend(lines, nvim.split(text, "\n", { plain = true }))
  lines[#lines + 1] = ""
  insert_transcript(self, view, lines)
  view.last_block_kind = "prose"
  view.trailing_blank = true
end

---@param view louiselm.ui.ChatView
---@return table[] content
---@return louiselm.ui.ContextItem[] contexts
---@return string? error_message
local function build_context_content(view)
  local content = {}
  local contexts = {}
  if view.skill_catalog ~= nil then
    local catalog = { label = "skill-index", text = view.skill_catalog }
    contexts[#contexts + 1] = catalog
    if view.session:inspect().embedded_context then
      content[#content + 1] = {
        type = "resource",
        resource = {
          uri = "louiselm://skills/index",
          mimeType = "text/plain",
          text = view.skill_catalog,
        },
      }
    else
      content[#content + 1] = context_content(catalog)
    end
  end
  for _, item in ipairs(view.contexts) do
    if item.text == nil and item.skill_path ~= nil then
      local skill_content = Skills.read(item.skill_path)
      if skill_content == nil then
        return {}, {}, "could not read selected skill: " .. item.skill_path
      end
      item.text = skill_content
    end
    contexts[#contexts + 1] = item
    content[#content + 1] = context_content(item)
  end
  return content, contexts, nil
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
  local content, contexts, context_error = build_context_content(view)
  if context_error ~= nil then
    return nil, context_error, {}
  end
  if #content == 0 then
    return final_text, nil, {}
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

---@param view louiselm.ui.ChatView
local function clear_prompt_context(view)
  view.contexts = {}
  view.context_prefix = ""
  view.skill_catalog = nil
  view.pending_skill = nil
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
  local phase = view.pending_skill and view.pending_skill.phase
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
  local prompt_line_count = replace_submitted_prompt(view, text, contexts)
  local response_line = view.prompt_line + prompt_line_count
  local next_prefix = slash_prompt and view.context_prefix or ""
  nvim.api.nvim_buf_set_lines(view.buffer, response_line, response_line, false, { "", "> " .. next_prefix })
  view.response_line = response_line
  view.response_tail = view.response_line
  view.response_started = false
  view.pending_terminal_completion = nil
  view.last_block_kind = nil
  view.transcript_tail = view.response_tail
  mark_prompt(view, response_line + 1)
  if nvim.api.nvim_get_current_buf() == view.buffer then
    nvim.api.nvim_win_set_cursor(0, { view.prompt_line + 1, 2 + #next_prefix })
  end
  if not slash_prompt then
    clear_prompt_context(view)
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
  local prompt_line_count = replace_submitted_prompt(view, text, contexts)
  local response_line = view.prompt_line + prompt_line_count
  nvim.api.nvim_buf_set_lines(view.buffer, response_line, response_line, false, { "", "> " })
  view.response_line = response_line
  view.response_tail = response_line
  view.response_started = false
  view.pending_terminal_completion = nil
  view.last_block_kind = nil
  view.transcript_tail = response_line
  mark_prompt(view, response_line + 1)
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
  view.options_revision = (view.options_revision or 0) + 1
  local revision = view.options_revision
  local function current()
    return not self.disposed
      and self.views[state.id] == view
      and self.current_id == state.id
      and not self.decision_active
      and view.options_revision == revision
      and view.session:inspect().status == "ready"
      and nvim.deep_equal(view.session:inspect().config_options, state.config_options)
  end
  Picker.select(state.config_options, {
    prompt = "louiselm session options: ",
    format_item = function(option)
      return option.name .. ": " .. option_display_value(option)
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
---@param line string
local function insert_usage(self, view, line)
  local insertion_line = view.transcript_tail == nil and view.prompt_line or view.transcript_tail + 1
  local before = insertion_line > 0
      and nvim.api.nvim_buf_get_lines(view.buffer, insertion_line - 1, insertion_line, false)[1]
    or nil
  local after = nvim.api.nvim_buf_get_lines(view.buffer, insertion_line, insertion_line + 1, false)[1]
  local lines = {}
  if before ~= nil and before ~= "" then
    lines[#lines + 1] = ""
  end
  lines[#lines + 1] = line
  if after ~= nil and after ~= "" then
    lines[#lines + 1] = ""
  end
  insert_transcript(self, view, lines)
end

---@param self louiselm.ui.Chat
---@param view louiselm.ui.ChatView
---@param turn integer
local function restore_turn_usage(self, view, turn)
  local usage = view.restored_usage[turn]
  view.restored_usage[turn] = nil
  local line = usage_line(usage)
  if line ~= nil then
    insert_usage(self, view, line)
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
        insert_transcript(self, view, { "Error: " .. (error_message or "could not record workflow feedback") })
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
    insert_transcript(self, view, { "[workflow] approved CONTINUE: " .. candidate.label })
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
              insert_transcript(self, view, { "Error: " .. error_message })
            else
              insert_transcript(self, view, { "[workflow] approved Model: " .. candidate.model })
              render_header(self, view)
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
          insert_transcript(self, view, { "Error: " .. (rejection_error or "recommendation could not be rejected") })
        end
        return
      end
      local approved, approval_error = self.workflow:approve(candidate)
      if approved == nil then
        insert_transcript(self, view, { "Error: " .. (approval_error or "recommendation could not be approved") })
        return
      end
      local applied, apply_error = apply_recommendation(self, view, approved)
      if not applied then
        insert_transcript(self, view, { "Error: " .. (apply_error or "recommendation could not be applied") })
      end
    end)
  end)
end

---@param self louiselm.ui.Chat
---@param view louiselm.ui.ChatView
---@param event louiselm.session.Event
---@param completed_state? louiselm.session.State Immutable completion snapshot captured before queued UI work.
local function handle_event(self, view, event, completed_state)
  if self.disposed or self.views[event.session_id] ~= view or not nvim.api.nvim_buf_is_valid(view.buffer) then
    if event.type == "permission_requested" and type(event.respond) == "function" then
      -- Scheduled before teardown, delivered after it: no view is left to host the choice,
      -- and no other consumer will answer. The result has nowhere left to be reported.
      pcall(event.respond, { outcome = { outcome = "cancelled" } })
    end
    return
  end

  reconcile_prompt_boundary(view)

  if
    event.type == "state_changed"
    or event.type == "config_options_changed"
    or event.type == "usage_updated"
    or event.type == "recording_changed"
  then
    render_header(self, view)
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
    close_thought_fold_run(view)
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
    close_thought_fold_run(view)
    local continuing_prompt = view.replay_active and view.replay_user_open
    if view.replay_active and not view.replay_user_open then
      restore_turn_usage(self, view, view.replay_turn)
      view.replay_turn = view.replay_turn + 1
      view.replay_user_open = true
    end
    local text = chunk_text(event.data)
    if text == nil then
      return
    end
    local lines = nvim.split(text, "\n", { plain = true })
    for index, line in ipairs(lines) do
      lines[index] = "> " .. line
    end
    local prompt_line_count = #lines
    lines[#lines + 1] = ""
    local insertion_line = insert_transcript(self, view, lines)
    local first_prompt_line = insertion_line
    local prompt_mark
    if continuing_prompt and view.replay_prompt_mark ~= nil then
      local position = nvim.api.nvim_buf_get_extmark_by_id(
        view.buffer,
        view.prompt_namespace,
        view.replay_prompt_mark,
        { details = true }
      )
      local details = position[3]
      if #position == 3 and type(details) == "table" and details.invalid ~= true then
        first_prompt_line = position[1]
        prompt_mark = view.replay_prompt_mark
      end
    end
    local mark = mark_submitted_prompt(view, first_prompt_line, insertion_line + prompt_line_count, prompt_mark)
    if view.replay_active then
      view.replay_prompt_mark = mark
    end
    view.response_line = nil
    view.response_tail = nil
    view.response_started = false
    view.turn_prose = ""
    view.turn_done_fired = false
    view.last_block_kind = nil
  elseif event.type == "chunk" then
    view.replay_user_open = false
    local text = chunk_text(event.data)
    if text == nil then
      return
    end
    view.turn_prose = view.turn_prose .. text
    close_tool_fold_run(view)
    close_thought_fold_run(view)
    if not view.response_started then
      local lines = nvim.split(text, "\n", { plain = true })
      local insertion_line = view.response_tail
        or (view.transcript_tail == nil and view.prompt_line or view.transcript_tail + 1)
      if view.response_tail ~= nil then
        insertion_line = insertion_line + 1
      end
      local separator = 0
      if view.last_block_kind == "tool" or view.last_block_kind == "reasoning" then
        nvim.api.nvim_buf_set_lines(view.buffer, insertion_line, insertion_line, false, { "" })
        insertion_line = insertion_line + 1
        separator = 1
      end
      local response_line_count = #lines
      lines[#lines + 1] = ""
      nvim.api.nvim_buf_set_lines(view.buffer, insertion_line, insertion_line, false, lines)
      view.response_line = insertion_line
      view.response_tail = insertion_line + response_line_count - 1
      view.transcript_tail = view.response_tail + 1
      mark_prompt(view, view.prompt_line + separator + #lines)
      view.response_started = true
      view.last_block_kind = "prose"
      view.trailing_blank = true
      return
    end
    local current = nvim.api.nvim_buf_get_lines(view.buffer, view.response_tail, view.response_tail + 1, false)[1] or ""
    local lines = nvim.split(current .. text, "\n", { plain = true })
    nvim.api.nvim_buf_set_lines(view.buffer, view.response_tail, view.response_tail + 1, false, lines)
    local added = #lines - 1
    view.response_tail = view.response_tail + added
    view.transcript_tail = view.response_tail + 1
    mark_prompt(view, view.prompt_line + added)
  elseif event.type == "thought_chunk" then
    view.replay_user_open = false
    local text = chunk_text(event.data)
    if text == nil or text == "" then
      return
    end
    view.pending_terminal_completion = nil
    close_tool_fold_run(view)
    local run = view.thought_run
    if run == nil then
      -- A reasoning paragraph starts with a `[thinking]` header; its content lines
      -- are folded beneath the header once the paragraph ends (the assistant's
      -- answer, a tool call, or the turn boundary). Until then it streams like a
      -- response: later chunks append to the paragraph's last content line.
      local lines = { "[thinking]" }
      if view.last_block_kind == "prose" and not view.trailing_blank then
        table.insert(lines, 1, "")
      end
      insert_transcript(self, view, lines)
      run = { first = view.transcript_tail, last = view.transcript_tail }
      view.thought_run = run
      -- Inserting below the submitted prompt invalidates its response bookkeeping,
      -- exactly like a tool paragraph does.
      view.response_line = nil
      view.response_tail = nil
      view.response_started = false
      view.last_block_kind = "reasoning"
    end
    if run.last == run.first then
      local lines = nvim.split(text, "\n", { plain = true })
      nvim.api.nvim_buf_set_lines(view.buffer, run.first + 1, run.first + 1, false, lines)
      run.last = run.first + #lines
      view.transcript_tail = run.last
      mark_prompt(view, view.prompt_line + #lines)
    else
      local current = nvim.api.nvim_buf_get_lines(view.buffer, run.last, run.last + 1, false)[1] or ""
      local lines = nvim.split(current .. text, "\n", { plain = true })
      nvim.api.nvim_buf_set_lines(view.buffer, run.last, run.last + 1, false, lines)
      local added = #lines - 1
      run.last = run.last + added
      view.transcript_tail = run.last
      mark_prompt(view, view.prompt_line + added)
    end
  elseif event.type == "tool_call_started" or event.type == "tool_call_finished" then
    view.replay_user_open = false
    close_thought_fold_run(view)
    local status = field(event.data, "status")
    local id = tool_id(event.data)
    local title = field(event.data, "title")
    if title ~= nil then
      title = single_line(title)
    end
    if title ~= nil then
      view.tool_titles[id] = title
    else
      title = view.tool_titles[id]
    end
    local completion_text
    if event.type == "tool_call_started" then
      view.tool_statuses[id] = status or "started"
      local detail = id
      if title ~= nil then
        detail = detail .. ": " .. title
      end
      local line_text = "[tool] " .. detail .. " (started)"
      local existing_line = view.tool_lines[id]
      if existing_line ~= nil and existing_line < nvim.api.nvim_buf_line_count(view.buffer) then
        set_line(view.buffer, existing_line, line_text)
      else
        local lines = { line_text }
        if view.last_block_kind == "prose" and not view.trailing_blank then
          table.insert(lines, 1, "")
        end
        insert_transcript(self, view, lines)
        view.tool_lines[id] = view.transcript_tail
        view.tool_ids[view.transcript_tail] = id
        record_tool_line(view, view.transcript_tail)
      end
      view.last_block_kind = "tool"
    else
      local detail = id
      if title ~= nil then
        detail = detail .. ": " .. title
      end
      detail = detail .. " (" .. (status or "finished") .. ")"
      if status == "completed" then
        completion_text = terminal_completion_text(event.data, title)
        if tool_has_image(event.data) then
          detail = detail .. " · image result — use :LouiselmInspectTool"
        end
      end
      local line = view.tool_lines[id]
      local rendered_line
      if line ~= nil and line < nvim.api.nvim_buf_line_count(view.buffer) then
        set_line(view.buffer, line, "[tool] " .. detail)
        rendered_line = line
      else
        local lines = { "[tool] " .. detail }
        if view.last_block_kind == "prose" and not view.trailing_blank then
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
    if completion_text ~= nil then
      -- Copilot in Autopilot can emit the completed task_complete update after
      -- turn_done already ran (louiselm-zufj): that flush point will not fire
      -- again for this turn, so a late completion must flush immediately here.
      if view.turn_done_fired then
        flush_terminal_completion(self, view, completion_text)
      else
        view.pending_terminal_completion = completion_text
      end
    end
    view.response_line = nil
    view.response_tail = nil
    view.response_started = false
  elseif event.type == "prompt_rejected" then
    insert_transcript(self, view, { "Prompt not sent: " .. event.data.message })
  elseif event.type == "recording_changed" then
    if event.data.error ~= nil then
      clear_queued_prompt(view)
      insert_transcript(self, view, { "Recording error: " .. event.data.error.message })
    end
  elseif event.type == "error" then
    view.replay_user_open = false
    view.pending_terminal_completion = nil
    close_thought_fold_run(view)
    clear_queued_prompt(view)
    local message = field(event.data, "message") or "unknown session error"
    insert_transcript(self, view, { "Error: " .. message })
    view.response_line = nil
    view.response_tail = nil
    view.response_started = false
    view.last_block_kind = nil
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
    local state = view.session:inspect()
    if state.acp_session_id ~= nil and type(event.data) == "table" then
      self.attention:permission_cancelled(state.acp_session_id, event.data.request_ids)
    end
    cancel_decisions(self, view, type(event.data) == "table" and event.data.request_ids or nil)
  elseif event.type == "turn_done" then
    close_tool_fold_run(view)
    close_thought_fold_run(view)
    local terminal_completion = view.pending_terminal_completion
    view.pending_terminal_completion = nil
    if terminal_completion ~= nil then
      flush_terminal_completion(self, view, terminal_completion)
    end
    view.turn_done_fired = true
    local state = completed_state or view.session:inspect()
    self.attention:turn_done(state, nvim.api.nvim_get_current_buf() == view.buffer)
    local line = usage_line(state.usage)
    if line ~= nil then
      insert_usage(self, view, line)
    end
    if self.workflow ~= nil and view.workflow_phase ~= nil and view.queued_prompt == nil then
      local outcome = type(event.data) == "table" and event.data.stopReason == "cancelled" and "cancelled"
        or "completed"
      local observed, observe_error = self.workflow:observe(view.workflow_phase, state, outcome)
      if not observed then
        insert_transcript(self, view, { "Error: " .. (observe_error or "could not record workflow evidence") })
      end
      present_workflow_feedback(self, view, view.workflow_phase, state, function()
        if self.disposed or self.views[state.id] ~= view then
          return
        end
        local pending, _, recommend_error = self.workflow:recommend(view.workflow_phase, state)
        if pending ~= nil then
          present_recommendations(self, view, view.workflow_phase, pending)
        elseif recommend_error ~= nil and recommend_error ~= "phase already has an approved choice" then
          insert_transcript(self, view, { "Error: " .. recommend_error })
        end
      end)
    end
    view.response_line = nil
    view.response_tail = nil
    view.response_started = false
    view.last_block_kind = nil
    release_queued_prompt(self, view)
    view.unread_turn = state.status == "ready" and nvim.api.nvim_get_current_buf() ~= view.buffer
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

---@param relay? louiselm.ui.ChatEventRelay
local function deactivate_event_relay(relay)
  if relay == nil then
    return
  end
  relay.events = nil
  relay.chat = nil
  relay.view = nil
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
    diff = Diff.new(),
    decision_queue = {},
    queue_namespace = nvim.api.nvim_create_namespace("louiselm.chat.queued_prompt"),
    prompt_namespace = nvim.api.nvim_create_namespace("louiselm.chat.prompt"),
    header_namespace = nvim.api.nvim_create_namespace("louiselm.chat.header"),
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
    handoffs = {},
    resume_client = nil,
    resume_controller = nil,
    park_observer = nil,
    resume_initializing = false,
    resume_waiters = {},
    resume_revisions = {},
    resume_summaries = {},
    pending_resume_runs = {},
    pending_resume_sessions = {},
    pending_resume_relays = {},
    current_id = nil,
    disposed = false,
  }, Chat)
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

---Prefix of the submitted-context block header line; the block runs from this
---line to the first prompt range recorded after it.
local CONTEXTS_HEADER_PREFIX = "> [contexts:"

---Collect the Normal-mode navigation targets in the transcript history above
---the live prompt, in document order. A `prompt` target is the first line of
---each range recorded at a trusted user-prompt render boundary. A `reply`
---target is the first prose line of a turn's response: thinking content is
---excluded via the recorded thought-fold runs (text alone cannot distinguish
---it from prose), and so are blank lines, `[…` marker lines, and
---`Error:`/`Warning:` lines. A tool-only turn contributes no reply target.
---@param view louiselm.ui.ChatView
---@param kind string "prompt" or "reply"
---@return integer[] targets Zero-based lines in document order.
local function navigation_targets(view, kind)
  local lines = nvim.api.nvim_buf_get_lines(view.buffer, 0, current_prompt_line(view), false)
  local prompt_lines = {}
  local prompt_starts = {}
  for _, mark in ipairs(nvim.api.nvim_buf_get_extmarks(view.buffer, view.prompt_namespace, 0, -1, { details = true })) do
    local details = mark[4]
    if details.end_row ~= nil and details.invalid ~= true then
      prompt_starts[mark[2]] = true
      for line = mark[2], details.end_row - 1 do
        prompt_lines[line] = true
      end
    end
  end
  local thinking_lines = {}
  for _, fold in ipairs(view.thought_folds) do
    for line = fold.first, fold.last do
      thinking_lines[line] = true
    end
  end
  local run = view.thought_run
  if run ~= nil then
    for line = run.first, run.last do
      thinking_lines[line] = true
    end
  end
  local targets = {}
  local in_context_block = false
  local after_prompt = false
  local seen_reply = false
  for index, line in ipairs(lines) do
    local zero_based = index - 1
    if line:sub(1, #CONTEXTS_HEADER_PREFIX) == CONTEXTS_HEADER_PREFIX then
      in_context_block = true
    elseif prompt_lines[zero_based] then
      in_context_block = false
      if prompt_starts[zero_based] and kind == "prompt" then
        targets[#targets + 1] = zero_based
      end
      after_prompt = true
      seen_reply = false
    else
      if
        kind == "reply"
        and after_prompt
        and not seen_reply
        and not in_context_block
        and line ~= ""
        and line:sub(1, 1) ~= "["
        and not thinking_lines[zero_based]
        and line:sub(1, 6) ~= "Error:"
        and line:sub(1, 8) ~= "Warning:"
      then
        targets[#targets + 1] = zero_based
        seen_reply = true
      end
    end
  end
  return targets
end

---Move the cursor to the count-th target of `kind` in `direction` from the
---cursor. A motion with no further target is a silent no-op; landing is on
---the target's first non-blank column.
---@param view louiselm.ui.ChatView
---@param kind string "prompt" or "reply"
---@param direction integer 1 forward, -1 backward
---@param count integer
local function navigate_transcript(view, kind, direction, count)
  local targets = navigation_targets(view, kind)
  local cursor_line = nvim.api.nvim_win_get_cursor(view.window)[1] - 1
  local remaining = count
  local selected
  if direction > 0 then
    for _, target in ipairs(targets) do
      if target > cursor_line then
        remaining = remaining - 1
        if remaining == 0 then
          selected = target
          break
        end
      end
    end
  else
    for index = #targets, 1, -1 do
      local target = targets[index]
      if target < cursor_line then
        remaining = remaining - 1
        if remaining == 0 then
          selected = target
          break
        end
      end
    end
  end
  if selected == nil then
    return
  end
  local line = nvim.api.nvim_buf_get_lines(view.buffer, selected, selected + 1, false)[1] or ""
  nvim.api.nvim_win_set_cursor(view.window, { selected + 1, #(line:match("^%s*")) })
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
---@param event_relay? louiselm.ui.ChatEventRelay Events captured before Run finalization.
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
  if self.markdown_highlighting then
    nvim.treesitter.start(buffer, "markdown")
  end
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
    turn_prose = "",
    turn_done_fired = false,
    last_block_kind = nil,
    trailing_blank = false,
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
    thought_folds = {},
    thought_run = nil,
    queued_prompt = nil,
    queue_mark = nil,
    queue_namespace = self.queue_namespace,
    prompt_namespace = self.prompt_namespace,
    setup_shown = false,
    unread_turn = false,
    replay_active = replay_active,
    replay_user_open = false,
    replay_prompt_mark = nil,
    replay_turn = 0,
    restored_usage = restored_usage,
    replay_events = replay_active and {} or nil,
    transcript = Transcript.new(),
    unsubscribe = function() end,
    last_forensics_path = nil,
  }
  mark_prompt(view, view.prompt_line)
  nvim.api.nvim_buf_attach(buffer, false, {
    on_lines = function(_, _, _, first_line, last_line)
      if view.queued_prompt ~= nil and first_line <= view.prompt_line and last_line > view.prompt_line then
        clear_queued_prompt(view)
      end
    end,
  })
  nvim.keymap.set("n", "za", function()
    toggle_chat_fold(view)
  end, { buffer = buffer, silent = true, desc = "Toggle chat fold" })
  nvim.keymap.set("n", "]u", function()
    navigate_transcript(view, "prompt", 1, nvim.v.count1)
  end, { buffer = buffer, silent = true, desc = "Next submitted prompt" })
  nvim.keymap.set("n", "[u", function()
    navigate_transcript(view, "prompt", -1, nvim.v.count1)
  end, { buffer = buffer, silent = true, desc = "Previous submitted prompt" })
  nvim.keymap.set("n", "]r", function()
    navigate_transcript(view, "reply", 1, nvim.v.count1)
  end, { buffer = buffer, silent = true, desc = "Next assistant reply" })
  nvim.keymap.set("n", "[r", function()
    navigate_transcript(view, "reply", -1, nvim.v.count1)
  end, { buffer = buffer, silent = true, desc = "Previous assistant reply" })
  nvim.keymap.set("n", "<CR>", function()
    local prompt_line = current_prompt_line(view)
    nvim.api.nvim_win_set_cursor(view.window, { prompt_line + 1, 2 + #view.context_prefix })
    if #nvim.api.nvim_list_uis() > 0 then
      nvim.cmd.startinsert()
    end
  end, { buffer = buffer, silent = true, desc = "Jump to the louiselm prompt" })
  if event_relay == nil then
    view.unsubscribe = session:on(function(event)
      observe_view_event(self, view, event)
    end)
  else
    view.unsubscribe = function()
      if event_relay.view == view then
        deactivate_event_relay(event_relay)
      end
    end
  end
  self.views[state.id] = view
  self.view_order[#self.view_order + 1] = state.id
  if event_relay ~= nil then
    local events = event_relay.events or {}
    event_relay.events = nil
    event_relay.chat = self
    event_relay.view = view
    for _, event in ipairs(events) do
      observe_view_event(self, view, event)
    end
  end
  nvim.api.nvim_create_autocmd({ "BufEnter", "WinEnter" }, {
    buffer = buffer,
    callback = function()
      if not self.disposed and self.views[state.id] == view then
        mark_view_seen(self, view)
      end
    end,
    desc = "Mark viewed LouiseLM Session Attention as seen",
  })
  render_header(self, view)
  self:switch(state.id)
  if state.status == "ready" and not replay_active then
    refresh_limits(self, state.agent)
  end
  if restore_error ~= nil then
    insert_transcript(self, view, { "Error: " .. restore_error })
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
        insert_transcript(self, view, { "Recording error: " .. err.message })
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
  nvim.keymap.set("i", "<CR>", function()
    self:submit()
  end, { buffer = buffer, silent = true, desc = "Submit louiselm prompt" })
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
  return view and view.buffer or nil
end

---Placeholder marking an unfilled takeover task in a Handoff review buffer;
---`submit_handoff` refuses to send while this exact text is still present.
local HANDOFF_TASK_PLACEHOLDER = "<replace with the concrete action the target must take>"

---Read the filled takeover task from a Handoff review buffer's contents, or nil
---when the line is missing, blank, or still holds the template placeholder.
---@param text string
---@return string? task
local function takeover_task(text)
  for line in text:gmatch("[^\n]+") do
    local value = line:match("^%s*%-%s*takeover task:%s*(.-)%s*$")
    if value ~= nil then
      if value == "" or value:find(HANDOFF_TASK_PLACEHOLDER, 1, true) ~= nil then
        return nil
      end
      return value
    end
  end
  return nil
end

---Open a Handoff review buffer for another session: an editable three-section
---brief — a `## Handoff` takeover-task template first, the compacted source
---transcript as `## Context`, and `## Source` metadata — that `submit_handoff`
---validates and sends.
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
  local source_state = source_view.session:inspect()
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
  local entries = source_view.transcript:snapshot()
  local user_turns = 0
  for _, entry in ipairs(entries) do
    if entry.kind == "user" then
      user_turns = user_turns + 1
    end
  end
  local context = Transcript.render_compact(entries, source_view.session:inspect())
  local source_ref = source_state.acp_session_id ~= nil and report_id(source_state.agent, source_state.acp_session_id)
    or source_state.agent
  local brief = table.concat({
    "## Handoff",
    "",
    "- source: `" .. source_ref .. "`",
    "- takeover task: " .. HANDOFF_TASK_PLACEHOLDER,
    "- constraints: (none)",
    "",
    "## Context",
    "",
    context,
    "## Source",
    "",
    "- agent: " .. source_state.agent,
    "- acp session: " .. (source_state.acp_session_id or "none"),
    "- user turns: " .. tostring(user_turns),
    "",
  }, "\n")
  nvim.api.nvim_buf_set_lines(buffer, 0, -1, false, nvim.split(brief, "\n", { plain = true }))
  self.handoffs[buffer] = {
    target_session = target_session,
    target_session_id = target_state.id,
    source_session_id = source_state.acp_session_id and report_id(source_state.agent, source_state.acp_session_id)
      or nil,
    source_id = source_id,
  }
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

---Split a reviewed Handoff brief at its `## Context` header, or nil when the
---header is absent (the operator edited it out). The Handoff section — the
---takeover instruction — travels as the prompt's text block; everything from
---`## Context` on, including `## Source`, travels as the resource block.
---@param text string
---@return string? handoff_section
---@return string? context_section
local function split_handoff_brief(text)
  local marker = text:find("\n## Context", 1, true)
  if marker == nil then
    return nil, nil
  end
  return text:sub(1, marker - 1), text:sub(marker + 1)
end

---Build the prompt content for a reviewed Handoff brief. Any context staged
---for the target's first prompt travels with the Handoff and is consumed.
---Targets advertising `embeddedContext` receive the Handoff instruction as a
---text block and the compacted context as a typed resource block; everyone
---else receives the whole brief as one flattened text prompt when no staged
---context is present. A brief whose `## Context` header was edited out
---degrades to the flattened form rather than failing the Handoff.
---@param handoff table Review-buffer record from `open_handoff`.
---@param text string Reviewed brief text.
---@param target_view louiselm.ui.ChatView? Handoff target's attached view.
---@return string|table? content
---@return louiselm.ui.ContextItem[] contexts
---@return string? error_message
local function handoff_content(handoff, text, target_view)
  ---@type string|table
  local content = text
  if handoff.target_session:inspect().embedded_context == true then
    local handoff_section, context_section = split_handoff_brief(text)
    if handoff_section ~= nil then
      local source_ref = handoff.source_session_id or handoff.source_id
      content = {
        { type = "text", text = handoff_section },
        {
          type = "resource",
          resource = {
            uri = "louiselm://handoff/" .. source_ref,
            mimeType = "text/markdown",
            text = context_section,
          },
        },
      }
    end
  end
  if target_view == nil then
    return content, {}, nil
  end
  local context_blocks, contexts, context_error = build_context_content(target_view)
  if context_error ~= nil then
    return nil, {}, context_error
  end
  if #context_blocks == 0 then
    return content, {}, nil
  end
  if type(content) == "string" then
    context_blocks[#context_blocks + 1] = { type = "text", text = content }
  else
    nvim.list_extend(context_blocks, content)
  end
  return context_blocks, contexts, nil
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
  if takeover_task(text) == nil then
    return false, "handoff takeover task must be filled in before submitting"
  end
  local target_view = self.views[handoff.target_session_id]
  local content, contexts, content_error = handoff_content(handoff, text, target_view)
  if content == nil then
    return false, content_error or "handoff prompt could not be built"
  end
  local request_id, prompt_error = handoff.target_session:prompt(content)
  if request_id == nil then
    return false, prompt_error or "handoff prompt could not be sent"
  end
  if target_view ~= nil then
    if handoff.source_session_id ~= nil then
      record_handoff_prompt(target_view, text, handoff.source_session_id, contexts)
    end
    clear_prompt_context(target_view)
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
      staged[view.session] = {
        contexts = #view.contexts,
        pending_skill = view.pending_skill ~= nil,
        queued_prompt = view.queued_prompt ~= nil,
      }
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
    local session_id = state.agent .. "/" .. state.acp_session_id
    local claims_started, claims_error = live_claims(session_id, state.working_dir, function(claims, claim_error)
      if self.disposed or self.views[state.id] ~= view then
        return
      end
      if claim_error ~= nil then
        nvim.notify("louiselm: " .. claim_error, nvim.log.levels.ERROR)
        return
      end

      local run = session.owner_run
      local run_id = run and run.id or park_run_id()
      if run == nil then
        run = assert(Workflow.new_run({ id = run_id }))
      end

      local function cold_park()
        local started, park_error = run:park_cold({ id = run_id, claims = claims }, function(ok, error_message)
          if not ok then
            nvim.notify("louiselm: " .. (error_message or "could not cold-Park Session"), nvim.log.levels.ERROR)
            return
          end
          self.attention:run_parked(run_id)
          nvim.notify("louiselm: Session cold-Parked", nvim.log.levels.INFO)
        end)
        if not started then
          nvim.notify("louiselm: " .. (park_error or "could not cold-Park Session"), nvim.log.levels.ERROR)
        end
      end

      if session.owner_run ~= nil then
        cold_park()
        return
      end

      local admitted, admission_error = WorkflowService.admit({
        id = run_id,
        generated_work_max = PARK_GENERATED_WORK_MAX,
        park_ttl_ms = PARK_TTL_MS,
      }, function()
        local attached, attach_error = WorkflowService.attach({
          id = run_id,
          session_id = session_id,
          agent = state.agent,
          acp_session_id = state.acp_session_id,
          cwd = state.working_dir,
          load_session = true,
        }, function(ok, error_message)
          if not ok then
            nvim.notify("louiselm: " .. (error_message or "could not attach Session to Run"), nvim.log.levels.ERROR)
            return
          end
          local adopted, adopt_error = run:adopt_session(session)
          if not adopted then
            nvim.notify("louiselm: " .. (adopt_error or "could not attach Session to Run"), nvim.log.levels.ERROR)
            return
          end
          cold_park()
        end)
        if not attached then
          nvim.notify("louiselm: " .. (attach_error or "could not attach Session to Run"), nvim.log.levels.ERROR)
        end
      end)
      if not admitted then
        nvim.notify("louiselm: " .. (admission_error or "could not admit Run"), nvim.log.levels.ERROR)
      end
    end)
    if not claims_started then
      nvim.notify("louiselm: " .. (claims_error or "could not query live Beads claims"), nvim.log.levels.ERROR)
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
  if not nvim.api.nvim_buf_is_valid(view.buffer) then
    return false, "session buffer is invalid"
  end
  local window = nvim.api.nvim_get_current_win()
  if nvim.api.nvim_win_get_config(window).relative ~= "" then
    -- A provider-owned picker can restore its buffer during BufWinEnter.
    -- Prefer the Chat's last normal window, never replace the picker buffer.
    for _, candidate in ipairs(nvim.api.nvim_tabpage_list_wins(0)) do
      if nvim.api.nvim_win_get_config(candidate).relative == "" then
        window = candidate
        if candidate == view.window then
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
  view.window = window
  nvim.api.nvim_set_current_win(window)
  nvim.api.nvim_win_set_buf(window, view.buffer)
  nvim.api.nvim_win_set_cursor(window, { view.prompt_line + 1, 2 })
  if self.start_insert_on_switch and #nvim.api.nvim_list_uis() > 0 then
    nvim.cmd.startinsert()
    nvim.api.nvim_win_set_cursor(window, { view.prompt_line + 1, 2 })
  end
  apply_incremental_folds(view, view.window, view.context_folds, view.fold_counts)
  apply_incremental_folds(view, view.window, view.tool_folds, view.tool_fold_counts)
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
    format_item = session_formatter(sessions),
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
  -- Retire authority now; a provider-owned picker may return much later. Keep
  -- other Sessions queued until that callback, so a new selector cannot toggle it.
  for index = #self.decision_queue, 1, -1 do
    local decision = self.decision_queue[index]
    if decision.view == view then
      table.remove(self.decision_queue, index)
      if not decision.answered then
        decision.answered = true
        cancel_permission(self, view, decision.respond)
      end
    end
  end
  local active = self.decision_active
  if active ~= nil and active.view == view then
    self.decision_active = nil
    active.answered = true
    cancel_permission(self, view, active.respond)
    self.diff:close()
  end
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
  if nvim.api.nvim_buf_is_valid(view.buffer) then
    nvim.api.nvim_buf_delete(view.buffer, { force = true })
  end
  if close_error ~= nil then
    nvim.notify("louiselm: " .. close_error, nvim.log.levels.ERROR)
  end
  pump_decisions(self)
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
        phase = skill.phase,
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
  if self.decision_active then
    return false, DECISION_OPEN_ERROR
  end
  local source_id = self.current_id
  local source_view = source_id and self.views[source_id]
  if source_view == nil then
    return false, "no chat session is attached"
  end
  local source_state = source_view.session:inspect()
  if (source_state.status ~= "ready" and source_state.status ~= "error") or source_view.queued_prompt ~= nil then
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

---@param self louiselm.ui.Chat
---@param id string Durable Run UUID.
---@return louiselm.workflow.Run? run
local function resume_run(self, id)
  local pending = self.pending_resume_runs[id]
  if pending ~= nil then
    return pending
  end
  for _, view in pairs(self.views) do
    local run = view.session.owner_run
    if run ~= nil and run.id == id then
      return run
    end
  end
  return nil
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

---@param self louiselm.ui.Chat
---@param run louiselm.workflow.RunView
---@param callback fun(worker: louiselm.workflow.RunWorker?, error_message?: string)
local function load_cold_run(self, run, callback)
  local summary = self.resume_summaries[run.id]
  if summary == nil then
    callback(nil, "cold Run metadata is unavailable")
    return
  end
  ---@type louiselm.ui.ChatEventRelay
  local event_relay = { events = {} }
  self.pending_resume_relays[run.id] = event_relay
  local session, load_error = self.api:load_session(summary.agent, summary.acp_session_id, {
    cwd = summary.cwd,
    name = summary.acp_session_id,
    on_event = function(event)
      if event_relay.chat ~= nil and event_relay.view ~= nil then
        observe_view_event(event_relay.chat, event_relay.view, event)
        return
      end
      local events = event_relay.events
      if events ~= nil then
        events[#events + 1] = event
      end
    end,
  }, function(loaded_session, ready_error)
    if self.pending_resume_relays[run.id] ~= event_relay then
      deactivate_event_relay(event_relay)
      if loaded_session ~= nil then
        loaded_session:dispose()
      end
      callback(nil, "cold Park load was cancelled")
      return
    end
    if ready_error ~= nil then
      self.pending_resume_relays[run.id] = nil
      deactivate_event_relay(event_relay)
      if loaded_session ~= nil then
        loaded_session:dispose()
      end
      callback(nil, ready_error)
      return
    end
    if loaded_session == nil then
      self.pending_resume_relays[run.id] = nil
      deactivate_event_relay(event_relay)
      callback(nil, "cold Park load returned no Session")
      return
    end
    local local_run, run_error = Workflow.new_run({
      id = summary.id,
      claims = summary.claims,
      generated_work = summary.generated_work,
    })
    if local_run == nil then
      self.pending_resume_relays[run.id] = nil
      deactivate_event_relay(event_relay)
      loaded_session:dispose()
      callback(nil, run_error or "could not reconstruct resumed Run")
      return
    end
    local adopted, adopt_error = local_run:adopt_session(loaded_session)
    if not adopted then
      self.pending_resume_relays[run.id] = nil
      deactivate_event_relay(event_relay)
      loaded_session:dispose()
      callback(nil, adopt_error or "could not reconstruct resumed Run")
      return
    end
    self.pending_resume_runs[run.id] = local_run
    self.pending_resume_sessions[run.id] = loaded_session
    ---@diagnostic disable-next-line: param-type-mismatch -- Session is the production RunWorker implementation.
    callback(loaded_session)
  end)
  if session == nil then
    if self.pending_resume_relays[run.id] == event_relay then
      self.pending_resume_relays[run.id] = nil
    end
    deactivate_event_relay(event_relay)
    callback(nil, load_error or "could not load cold Park")
  end
end

---@param self louiselm.ui.Chat
---@param callback fun(controller: louiselm.workflow.ResumeController?, error_message?: string)
---@return boolean started
---@return string? error_message
local function ensure_resume_controller(self, callback)
  if self.resume_controller ~= nil then
    callback(self.resume_controller)
    return true
  end
  self.resume_waiters[#self.resume_waiters + 1] = callback
  if self.resume_initializing then
    return true
  end
  self.resume_initializing = true
  local settled = false
  local function finish(controller, error_message)
    if settled then
      return
    end
    settled = true
    self.resume_initializing = false
    local waiters = self.resume_waiters
    self.resume_waiters = {}
    for _, waiter in ipairs(waiters) do
      waiter(controller, error_message)
    end
  end
  local observer, observer_error = ParkObserver.new({
    find_run = function(id)
      return resume_run(self, id)
    end,
    on_park = function(presentation)
      present_park(self, presentation)
    end,
  })
  if observer == nil then
    finish(nil, observer_error or "could not create Park observer")
    return false, observer_error
  end
  local socket_path, capability_path = resume_paths()
  local capability_started, capability_error = RunClient.read_operator_capability(
    capability_path,
    function(capability, read_error)
      if read_error ~= nil or capability == nil then
        finish(nil, read_error or "could not read operator capability")
        return
      end
      local connect_client, connect_error
      connect_client, connect_error = RunClient.connect(socket_path, function(runs)
        if self.disposed then
          return
        end
        local observed, observe_error = observer:observe(runs)
        if not observed then
          if not settled then
            finish(nil, observe_error or "could not reconcile Park snapshot")
          else
            nvim.notify("louiselm: " .. (observe_error or "could not reconcile Park snapshot"), nvim.log.levels.ERROR)
          end
          return
        end
        for _, run in ipairs(runs) do
          self.resume_revisions[run.id] = run.revision
        end
        if settled then
          return
        end
        if connect_client == nil then
          finish(nil, "Run service connected without a client")
          return
        end
        local controller, controller_error = ResumeController.new({
          client = connect_client,
          find_run = function(id)
            return resume_run(self, id)
          end,
          load_cold = function(run, load_callback)
            load_cold_run(self, run, load_callback)
          end,
        })
        if controller == nil then
          finish(nil, controller_error or "could not create resume controller")
          return
        end
        self.resume_client = connect_client
        self.resume_controller = controller
        self.park_observer = observer
        finish(controller)
      end, {
        operator_capability = capability,
        on_error = function(message)
          if not settled then
            finish(nil, message)
          elseif not self.disposed then
            nvim.notify("louiselm: " .. message, nvim.log.levels.ERROR)
          end
        end,
      })
      if connect_client == nil then
        finish(nil, connect_error or "could not connect to Run service")
      end
    end
  )
  if not capability_started then
    self.resume_waiters = {}
    self.resume_initializing = false
    return false, capability_error
  end
  return true
end

---Discover and load a durable cold-Parked Run through an operator picker.
---@param self louiselm.ui.Chat
---@return boolean started
---@return string? error_message
function Chat:resume_park()
  if self.disposed then
    return false, "chat UI is disposed"
  end
  local started, setup_error = ensure_resume_controller(self, function(controller, controller_error)
    if self.disposed then
      return
    end
    if controller == nil then
      nvim.notify("louiselm: " .. (controller_error or "could not connect to Run service"), nvim.log.levels.ERROR)
      return
    end
    WorkflowService.list(function(runs, list_error)
      if self.disposed then
        return
      end
      if list_error ~= nil then
        nvim.notify("louiselm: " .. list_error, nvim.log.levels.ERROR)
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
        local revision = self.resume_revisions[selected.id]
        if revision == nil then
          nvim.notify("louiselm: selected Run revision is unavailable", nvim.log.levels.ERROR)
          return
        end
        self.resume_summaries[selected.id] = selected
        local resume_view = {
          id = selected.id,
          revision = revision,
          state = selected.state,
          generated_work_ceiling = selected.generated_work.ceiling,
          generated_work_consumed = selected.generated_work.consumed,
          generated_work_reserved = selected.generated_work.reserved,
          pending_mutation_ids = {},
          park_expires_at_ms = selected.expires_at_ms,
        }
        local resume_started, resume_error = controller:resume(resume_view, function(_, error_message)
          local session = self.pending_resume_sessions[selected.id]
          local event_relay = self.pending_resume_relays[selected.id]
          self.pending_resume_sessions[selected.id] = nil
          self.pending_resume_relays[selected.id] = nil
          self.pending_resume_runs[selected.id] = nil
          self.resume_summaries[selected.id] = nil
          if error_message ~= nil then
            deactivate_event_relay(event_relay)
            nvim.notify("louiselm: " .. error_message, nvim.log.levels.ERROR)
            return
          end
          if session == nil then
            deactivate_event_relay(event_relay)
            nvim.notify("louiselm: resumed Run has no loaded Session", nvim.log.levels.ERROR)
            return
          end
          if event_relay == nil or event_relay.events == nil then
            session:dispose()
            nvim.notify("louiselm: resumed Run has no Session event relay", nvim.log.levels.ERROR)
            return
          end
          local attached, attach_error = attach_session(self, session, event_relay)
          if not attached then
            deactivate_event_relay(event_relay)
            session:dispose()
            nvim.notify("louiselm: " .. (attach_error or "could not attach loaded session"), nvim.log.levels.ERROR)
            return
          end
          self.attention:run_resumed(selected.id)
          nvim.notify("louiselm: cold Park resumed (recoverable, lossy)", nvim.log.levels.INFO)
        end)
        if not resume_started then
          self.resume_summaries[selected.id] = nil
          nvim.notify("louiselm: " .. (resume_error or "could not resume cold Park"), nvim.log.levels.ERROR)
        end
      end)
    end)
  end)
  return started, setup_error
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
  if self.park_observer ~= nil then
    self.park_observer:dispose()
  end
  if self.resume_controller ~= nil then
    self.resume_controller:dispose()
  end
  if self.resume_client ~= nil then
    self.resume_client:dispose()
  end
  for _, event_relay in pairs(self.pending_resume_relays) do
    deactivate_event_relay(event_relay)
  end
  self.pending_resume_relays = {}
  for _, session in pairs(self.pending_resume_sessions) do
    session:dispose()
  end
  self.pending_resume_sessions = {}
  self.pending_resume_runs = {}
  self.resume_summaries = {}
  self.limits_unsubscribe()
  for agent_name in pairs(self.limits_timers) do
    close_limits_timer(self, agent_name)
  end
  restore_winbars(self)
  self.diff:dispose()
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
  self.view_order = {}
  self.winbar_targets = {}
  return true
end

return M
