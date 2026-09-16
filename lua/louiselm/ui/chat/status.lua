---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local M = {}

---@param value string
---@return string line
local function single_line(value)
  return (value:gsub("[\r\n]", " "))
end

---Return a human-readable label for an option's current value.
---For select options, resolves the wire value to its ConfigValue name when available.
---@param option louiselm.session.ConfigOption
---@return string
function M.option_display_value(option)
  if option.type == "select" and type(option.options) == "table" then
    for _, v in ipairs(option.options) do
      if v.value == option.current_value then
        return v.name
      end
    end
  end
  return tostring(option.current_value)
end

local ACP_HIGHLIGHT = "LouiselmAcpValue"
local DERIVED_HIGHLIGHT = "LouiselmDerivedValue"

local STATUS_HIGHLIGHTS = {
  ready = "LouiselmStatusReady",
  preparing = "LouiselmStatusActive",
  prompting = "LouiselmStatusActive",
  running = "LouiselmStatusActive",
  configuring = "LouiselmStatusActive",
  starting = "LouiselmStatusActive",
  waiting_permission = "LouiselmStatusWarning",
  cancelling = "LouiselmStatusWarning",
  error = "LouiselmStatusError",
  disposed = "LouiselmStatusWarning",
}

-- Must be a plain dotted name Vim can resolve at click-dispatch time, not a
-- call expression: `v:lua.require('...').winbar_click` is silently never
-- invoked on a real click (louiselm-7ios). `command.lua` registers this
-- global as a stable forwarder to its own reassignable `M.winbar_click`.
local WINBAR_CLICK_HANDLER = "v:lua.__louiselm_winbar_click"
local LIMITS_CLICK_TARGET = 99
local OPTIONS_CLICK_TARGET = 98

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

---Render aligned picker labels from one opening's snapshots; inputs are unchanged.
---@param states louiselm.session.State[]
---@return string[] labels Labels in snapshot order; empty for no Sessions.
function M.session_labels(states)
  local columns = { "id", "name", "status", "skills", "agent", "loaded" }
  local seen_options = {}
  local rows = {}
  local ambiguous = {}
  -- Freeze one opening so filtering and repeated rendering use the same widths/values.
  for _, state in ipairs(states) do
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
      rows[index][column] = option.name .. "=" .. M.option_display_value(option)
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
    labels[index] = table.concat(parts, " · ")
  end
  return labels
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
  if state.status == "prompting" or state.status == "running" then
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
  if (state.status == "prompting" or state.status == "running") and state.session_failure ~= nil then
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

---Render four diagnostic header lines and byte-based highlights without editor mutations.
---@param state louiselm.session.State
---@return string[] lines
---@return louiselm.ui.HeaderHighlight[] highlights Empty where no values are reported.
function M.session_header(state)
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
    local option_text = single_line(option.name) .. "=" .. single_line(M.option_display_value(option))
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

---@class louiselm.ui.BackgroundStatus
---@field state louiselm.session.State Session snapshot.
---@field unread_turn boolean Whether a completed response is unseen.

---@param view louiselm.ui.BackgroundStatus
---@return louiselm.ui.WinbarEntry entry
local function background_winbar_entry(view)
  local state = view.state
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

---@class louiselm.ui.StatusLimits
---@field text string Rendered Limits summary.
---@field group string Highlight group.

---@class louiselm.ui.LimitsTarget
---@field agent string

---@class louiselm.ui.OptionsTarget
---@field session string Session whose options should be inspected.

---Render the active Session segment from a snapshot and optional Limits summary.
---@param state louiselm.session.State
---@param limits? louiselm.ui.StatusLimits
---@param available? integer Display columns reserved for the active Session.
---@return string winbar
---@return string? limits_agent Absent when no Limits summary is supplied.
---@return boolean options_visible
function M.session_winbar(state, limits, available)
  local options = state.config_options or {}
  local model, effort
  for _, option in ipairs(options) do
    if option.category == "model" and model == nil then
      model = M.option_display_value(option)
    elseif option.category == "thought_level" and effort == nil then
      effort = "e=" .. M.option_display_value(option)
    end
  end
  local name = state.name ---@type string?
  if
    name == ""
    or name == state.id
    or name == state.acp_session_id
    or name == state.agent
    or (state.acp_session_id ~= nil and name == report_id(state.agent, state.acp_session_id))
  then
    name = nil
  end
  -- Remove telemetry, then effort/model, then limits/Agent/name. Critical turn
  -- state is last; native truncation only handles physically impossible widths.
  for stage = 0, 8 do
    local fields = { winbar_segment(turn_highlight(state), turn_label(state)) }
    local function add(text, group, target)
      if text ~= nil then
        fields[#fields + 1] = target and clickable_winbar_segment(target, group, text) or winbar_segment(group, text)
      end
    end
    if stage < 7 then
      add(name, "Normal")
    end
    if stage < 6 then
      add(state.agent, "Normal")
    end
    local limits_visible = limits ~= nil and stage < 5
    if limits ~= nil and limits_visible then
      add(limits.text, limits.group, LIMITS_CLICK_TARGET)
    end
    local options_visible = #options > 0 and stage < 8
    if options_visible then
      local tokens = {}
      local hidden = #options
      if stage < 4 and model ~= nil then
        tokens[#tokens + 1] = model
        hidden = hidden - 1
      end
      if stage < 3 and effort ~= nil then
        tokens[#tokens + 1] = effort
        hidden = hidden - 1
      end
      if #tokens == 0 then
        tokens[1] = "opts"
      elseif hidden > 0 then
        tokens[#tokens + 1] = "+" .. hidden
      end
      add(table.concat(tokens, " "), ACP_HIGHLIGHT, OPTIONS_CLICK_TARGET)
    end
    if stage < 2 and state.context ~= nil then
      local _, derived = context_display(state)
      add("ctx " .. derived, DERIVED_HIGHLIGHT)
    end
    if stage < 1 then
      add(cost_display(state), ACP_HIGHLIGHT)
    end
    if fields[2] ~= nil then
      fields[2] = "%<" .. fields[2]
    end
    local bar = table.concat(fields, " · ")
    if
      available == nil
      or nvim.api.nvim_eval_statusline(bar, { maxwidth = 100000 }).width <= available
      or stage == 8
    then
      return bar, limits_visible and state.agent or nil, options_visible
    end
  end
  error("winbar layout exhausted")
end

---Return the minimum space needed for collapsed background summaries.
---@param backgrounds louiselm.ui.BackgroundStatus[]
---@return integer width
function M.background_width(backgrounds)
  local entries = {}
  for _, background in ipairs(backgrounds) do
    entries[#entries + 1] = background_winbar_entry(background)
  end
  return winbar_entries_width(collapsed_winbar_entries(entries, 0))
end

---Lay out background summaries within the available width; inputs are unchanged.
---@param base string Active Session winbar segment.
---@param limits_agent string? Agent targeted by the base Limits segment.
---@param backgrounds louiselm.ui.BackgroundStatus[] Background Sessions in view order.
---@param available integer Display columns left after the base segment.
---@param options_session? string Active Session with a visible options block.
---@return string winbar
---@return table<integer, string|false|louiselm.ui.LimitsTarget|louiselm.ui.OptionsTarget> targets Session IDs, picker overflow, Limits or options targets.
function M.layout_winbar(base, limits_agent, backgrounds, available, options_session)
  local quiet = {} ---@type louiselm.ui.WinbarEntry[]
  local attention = {} ---@type louiselm.ui.WinbarEntry[]
  for _, background in ipairs(backgrounds) do
    local entry = background_winbar_entry(background)
    local entries = entry.attention and attention or quiet
    entries[#entries + 1] = entry
  end
  local targets = {} ---@type table<integer, string|false|louiselm.ui.LimitsTarget|louiselm.ui.OptionsTarget>
  if options_session ~= nil then
    targets[OPTIONS_CLICK_TARGET] = { session = options_session }
  end
  if limits_agent ~= nil then
    targets[LIMITS_CLICK_TARGET] = { agent = limits_agent }
  end
  if #quiet == 0 and #attention == 0 then
    return base, targets
  end

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
    local target = index >= OPTIONS_CLICK_TARGET and index + 2 or index
    targets[target] = entry.id ~= "" and entry.id or false
    segments[index] = clickable_winbar_segment(target, entry.group, entry.text)
  end
  return base .. "%=" .. table.concat(segments, " · "), targets
end

return M
