---@class louiselm.session.ConfigValue
---@field value string Wire value identifier.
---@field name string Human-readable value name.
---@field description? string Optional agent-provided description.

---@class louiselm.session.ConfigOption
---@field id string Wire configuration identifier.
---@field name string Human-readable option name.
---@field description? string Optional agent-provided description.
---@field category? string Optional ACP semantic category.
---@field type "select"|"boolean" Supported input kind.
---@field current_value string|boolean Current agent-reported value.
---@field options? louiselm.session.ConfigValue[] Select values in agent order.

---@class louiselm.session.ContextUsage
---@field used number Tokens currently in context.
---@field size number Effective context window size.
---@field percentage number Derived percentage used, clamped to 100 when `used` exceeds `size`.
---@field pressure "normal"|"elevated"|"high"|"critical" Passive pressure state.
---@field stale boolean Whether a model change made this reading stale.

---@class louiselm.session.Cost
---@field amount number Agent-reported cumulative amount.
---@field currency string Agent-reported currency.

---@class louiselm.session.TurnUsage
---@field total_tokens? number
---@field input_tokens? number
---@field output_tokens? number
---@field thought_tokens? number
---@field cached_read_tokens? number
---@field cached_write_tokens? number

---@class louiselm.session.SessionFailure
---@field id string Stable Agent-provided failure identifier.
---@field revision integer Monotonic revision for this identifier.
---@field severity "warning"|"error" Agent-provided urgency.
---@field title string Human-readable status title.

---@class louiselm.session.DiscoveredSession
---@field agent string Configured agent definition name.
---@field session_id string Agent-side ACP session identifier.
---@field cwd string Authoritative primary workspace.
---@field title? string Agent-provided display title.
---@field updated_at? string Agent-provided ISO 8601 activity timestamp.

local M = {}

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

---@param value unknown
---@return boolean
local function non_empty_string(value)
  return type(value) == "string" and value ~= ""
end

---Normalize Copilot's opt-in root-agent lifecycle events, ignoring other Sessions/subagents.
---@param value unknown Native sessionEvent parameters.
---@param session_id string Current ACP Session identity.
---@return boolean? running Nil for unrelated/unknown events; false is authoritative idle.
---@return string? error_message Malformed consumed envelope fields.
function M.copilot_activity(value, session_id)
  if type(value) ~= "table" or not non_empty_string(value.sessionId) then
    return nil, "malformed Copilot activity Session identity"
  end
  if value.sessionId ~= session_id then
    return nil
  end
  if not non_empty_string(value.type) then
    return nil, "malformed Copilot activity event type"
  end
  if value.type ~= "assistant.turn_start" and value.type ~= "session.idle" then
    return nil
  end
  if value.agentId ~= nil and value.agentId ~= nvim.NIL then
    if not non_empty_string(value.agentId) then
      return nil, "malformed Copilot activity Agent identity"
    end
    return nil
  end
  return value.type == "assistant.turn_start"
end

---@param value unknown
---@return string? normalized
---@return boolean valid
local function optional_string(value)
  if value == nil or value == nvim.NIL then
    return nil, true
  end
  if type(value) == "string" then
    return value, true
  end
  return nil, false
end

---@param value unknown
---@return boolean
local function absolute_path(value)
  if not non_empty_string(value) then
    return false
  end
  local call_ok, absolute = pcall(nvim.fs.abspath, value)
  return call_ok and absolute == value
end

---@param value table
---@return boolean
local function dense_array(value)
  local count = 0
  local maximum = 0
  for key in pairs(value) do
    if type(key) ~= "number" or key < 1 or key % 1 ~= 0 then
      return false
    end
    count = count + 1
    maximum = math.max(maximum, key)
  end
  return count == maximum
end

---@param value unknown
---@return louiselm.session.ConfigValue[]? values
local function parse_values(value)
  if type(value) ~= "table" or not dense_array(value) then
    return nil
  end
  local values = {}
  local seen = {}
  for index, item in ipairs(value) do
    if
      type(item) ~= "table"
      or not non_empty_string(item.value)
      or not non_empty_string(item.name)
      or (item.description ~= nil and type(item.description) ~= "string")
    then
      return nil
    end
    if seen[item.value] then
      return nil
    end
    seen[item.value] = true
    values[index] = { value = item.value, name = item.name, description = item.description }
  end
  return values
end

---Validate and copy a complete agent-provided configuration option list.
---Unknown future option kinds are omitted; malformed supported kinds fail.
---@param value unknown
---@return louiselm.session.ConfigOption[]? options
---@return string? error_message
function M.config_options(value)
  if value == nil then
    return {}
  end
  if type(value) ~= "table" or not dense_array(value) then
    return nil, "configOptions must be an array"
  end
  local options = {}
  local seen = {}
  for _, item in ipairs(value) do
    if type(item) ~= "table" or not non_empty_string(item.type) then
      return nil, "config option must contain a type"
    end
    if item.type == "select" or item.type == "boolean" then
      if
        not non_empty_string(item.id)
        or not non_empty_string(item.name)
        or (item.description ~= nil and type(item.description) ~= "string")
        or (item.category ~= nil and not non_empty_string(item.category))
      then
        return nil, "config option has malformed common fields"
      end
      if seen[item.id] then
        return nil, "config option id is duplicated"
      end
      seen[item.id] = true
      local option = {
        id = item.id,
        name = item.name,
        description = item.description,
        category = item.category,
        type = item.type,
        current_value = item.currentValue,
      }
      if item.type == "boolean" then
        if type(item.currentValue) ~= "boolean" or item.options ~= nil then
          return nil, "boolean config option is malformed"
        end
      else
        local values = parse_values(item.options)
        if type(item.currentValue) ~= "string" or values == nil then
          return nil, "select config option is malformed"
        end
        local current_exists = false
        for _, current in ipairs(values) do
          if current.value == item.currentValue then
            current_exists = true
            break
          end
        end
        if not current_exists then
          return nil, "select config option currentValue is not available"
        end
        option.options = values
      end
      options[#options + 1] = option
    end
  end
  return options
end

---Validate and copy one ACP session/list page, ignoring unknown optional fields.
---@param value unknown ACP ListSessionsResponse value.
---@param agent string Configured agent definition name.
---@param cwd_filter? string Exact workspace to retain defensively.
---@return louiselm.session.DiscoveredSession[]? sessions
---@return string? next_cursor
---@return string? error_message
function M.discovery_page(value, agent, cwd_filter)
  if type(value) ~= "table" or type(value.sessions) ~= "table" or not dense_array(value.sessions) then
    return nil, nil, "response must contain a sessions array"
  end
  local next_cursor, cursor_valid = optional_string(value.nextCursor)
  if not cursor_valid then
    return nil, nil, "nextCursor must be a string"
  end

  local sessions = {}
  for _, item in ipairs(value.sessions) do
    if type(item) ~= "table" or not non_empty_string(item.sessionId) or not absolute_path(item.cwd) then
      return nil, nil, "session entry has malformed fields"
    end
    local title, title_valid = optional_string(item.title)
    local updated_at, updated_at_valid = optional_string(item.updatedAt)
    if not title_valid or not updated_at_valid then
      return nil, nil, "session entry has malformed fields"
    end
    if cwd_filter == nil or item.cwd == cwd_filter then
      sessions[#sessions + 1] = {
        agent = agent,
        session_id = item.sessionId,
        cwd = item.cwd,
        title = title,
        updated_at = updated_at,
      }
    end
  end
  return sessions, next_cursor
end

---Return whether a session discovery workspace is an absolute non-empty path.
---@param value unknown
---@return boolean valid
function M.discovery_cwd(value)
  return absolute_path(value)
end

---Find a supported configuration option by id.
---@param options louiselm.session.ConfigOption[]
---@param id string
---@return louiselm.session.ConfigOption? option
function M.find_option(options, id)
  for _, option in ipairs(options) do
    if option.id == id then
      return option
    end
  end
end

---Return the current model-category value, when advertised.
---@param options louiselm.session.ConfigOption[]
---@return string|boolean|nil value
function M.model_value(options)
  for _, option in ipairs(options) do
    if option.category == "model" then
      return option.current_value
    end
  end
end

---Validate and copy an agent-advertised command list, skipping malformed entries.
---A malformed top-level value is treated as empty rather than rejected, matching the wire
---format's own tolerance for a noisy or partially malformed command stream: the cache is always
---replaced with whatever is valid instead of failing the whole notification and going stale.
---@param value unknown
---@return louiselm.session.AvailableCommand[] commands Valid entries in agent order; duplicates are kept.
---@return string[] diagnostics One message per skipped malformed entry.
function M.available_commands(value)
  local diagnostics = {}
  if type(value) ~= "table" or not dense_array(value) then
    if value ~= nil then
      diagnostics[1] = "availableCommands must be an array"
    end
    return {}, diagnostics
  end
  local commands = {}
  for index, item in ipairs(value) do
    if type(item) == "table" and non_empty_string(item.name) and non_empty_string(item.description) then
      commands[#commands + 1] = { name = item.name, description = item.description }
    else
      diagnostics[#diagnostics + 1] = "skipped malformed available command at index " .. index
    end
  end
  return commands, diagnostics
end

---Whether Codex's ACP thread-status extension reports a fatal error for the current turn.
---Codex's own `session/prompt` response still claims `stopReason = "end_turn"` when this fires
---(observed for a revoked auth token), with no error text anywhere in the ACP stream, so this
---flag is the only signal available that the turn actually failed.
---@param value unknown ACP session update `_meta` value.
---@return boolean
function M.codex_system_error(value)
  if type(value) ~= "table" then
    return false
  end
  local codex = value.codex
  local thread_status = type(codex) == "table" and codex.threadStatus or nil
  return type(thread_status) == "table" and thread_status.type == "systemError"
end

---Validate the supported JetBrains AIR session-failure metadata fields.
---Unknown extension fields are deliberately ignored.
---@param value unknown ACP session update `_meta` value.
---@return louiselm.session.SessionFailure? failure
function M.session_failure(value)
  if type(value) ~= "table" then
    return nil
  end
  local jetbrains = value.jetbrains
  local air = type(jetbrains) == "table" and jetbrains.air or nil
  local failure = type(air) == "table" and air.sessionFailure or nil
  if
    type(air) ~= "table"
    or air.version ~= 1
    or type(failure) ~= "table"
    or not non_empty_string(failure.id)
    or type(failure.revision) ~= "number"
    or failure.revision < 1
    or failure.revision % 1 ~= 0
    or (failure.severity ~= "warning" and failure.severity ~= "error")
    or not non_empty_string(failure.title)
  then
    return nil
  end
  return {
    id = failure.id,
    revision = failure.revision,
    severity = failure.severity,
    title = failure.title,
  }
end

---Validate one context/cost usage update.
---`used` above `size` is accepted rather than rejected: an over-full window is a
---real state agents report, notably on the first mid-turn update after
---session/load, where the agent streams its default window size until the turn
---result corrects it to the model's real one. Such an update is reported as a
---saturated context, not as a protocol violation.
---@param value unknown
---@return louiselm.session.ContextUsage? context
---@return louiselm.session.Cost? cost
---@return boolean cost_present
function M.usage_update(value)
  if
    type(value) ~= "table"
    or type(value.used) ~= "number"
    or type(value.size) ~= "number"
    or value.used < 0
    or value.size <= 0
  then
    return nil, nil, false
  end
  local percentage = math.min(value.used / value.size * 100, 100)
  local pressure = "normal"
  if percentage >= 95 then
    pressure = "critical"
  elseif percentage >= 90 then
    pressure = "high"
  elseif percentage >= 75 then
    pressure = "elevated"
  end
  local context = {
    used = value.used,
    size = value.size,
    percentage = percentage,
    pressure = pressure,
    stale = false,
  }
  if value.cost == nil then
    return context, nil, false
  end
  if value.cost == nvim.NIL then
    return context, nil, true
  end
  if
    type(value.cost) ~= "table"
    or type(value.cost.amount) ~= "number"
    or value.cost.amount < 0
    or type(value.cost.currency) ~= "string"
    or value.cost.currency:match("^[A-Z][A-Z][A-Z]$") == nil
  then
    return nil, nil, false
  end
  return context, { amount = value.cost.amount, currency = value.cost.currency }, true
end

local USAGE_FIELDS = {
  { wire = "totalTokens", internal = "total_tokens" },
  { wire = "inputTokens", internal = "input_tokens" },
  { wire = "outputTokens", internal = "output_tokens" },
  { wire = "thoughtTokens", internal = "thought_tokens" },
  { wire = "cachedReadTokens", internal = "cached_read_tokens" },
  { wire = "cachedWriteTokens", internal = "cached_write_tokens" },
}

---Validate and copy optional agent-reported turn usage fields.
---The ACP wire shape is camelCase; the internal TurnUsage shape is snake_case.
---Absent or JSON-null usage means no usage was reported.
---Unknown optional fields are ignored; malformed known fields fail.
---@param value unknown
---@return louiselm.session.TurnUsage? usage
---@return boolean valid
function M.turn_usage(value)
  if value == nil or value == nvim.NIL then
    return nil, true
  end
  if type(value) ~= "table" then
    return nil, false
  end
  local usage = {}
  for _, field in ipairs(USAGE_FIELDS) do
    local amount = value[field.wire]
    if amount ~= nil then
      if type(amount) ~= "number" or amount < 0 or amount % 1 ~= 0 then
        return nil, false
      end
      usage[field.internal] = amount
    end
  end
  return usage, true
end

return M
