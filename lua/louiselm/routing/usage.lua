---@class louiselm.routing.UsageCost
---@field amount number Agent-reported cost amount.
---@field currency string Agent-reported ISO 4217 currency code.

---@class louiselm.routing.UsageSummaryCost
---@field currency string
---@field samples integer
---@field average number

---@class louiselm.routing.UsageSummary
---@field samples integer Turns with any measured usage or cost.
---@field token_samples integer Turns with measured token counts.
---@field average_tokens? number Average measured tokens per turn.
---@field costs louiselm.routing.UsageSummaryCost[] Average reported cost by currency.

---@class louiselm.routing.UsageRecord
---@field agent string
---@field option string
---@field value string|boolean
---@field samples integer
---@field token_samples integer
---@field total_tokens integer
---@field costs table<string, { samples: integer, total: number }>
---@field updated_at integer

---@class louiselm.routing.UsageTurnRecord
---@field agent string
---@field session_id string
---@field turn integer
---@field usage louiselm.session.TurnUsage

---@class louiselm.routing.Usage
---@field path string Persistent JSON path.
---@field records fun(self: louiselm.routing.Usage): louiselm.routing.UsageRecord[]?, string? Return detached persistent records.
---@field turns fun(self: louiselm.routing.Usage, agent: unknown, session_id: unknown): louiselm.routing.UsageTurnRecord[]?, string? Return exact presentation usage for one Session.
---@field summary fun(self: louiselm.routing.Usage, agent: unknown, option: unknown, value: unknown): louiselm.routing.UsageSummary?, string? Summarize one option value.

local M = {}
local Usage = {}
Usage.__index = Usage

local VERSION = 1
local USAGE_FIELDS = {
  "input_tokens",
  "output_tokens",
  "thought_tokens",
  "cached_read_tokens",
  "cached_write_tokens",
}
local ALL_USAGE_FIELDS = {
  "total_tokens",
  "input_tokens",
  "output_tokens",
  "thought_tokens",
  "cached_read_tokens",
  "cached_write_tokens",
}

---@return table
local function nvim()
  ---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
  return vim
end

---@param value unknown
---@return boolean
local function is_integer(value)
  return type(value) == "number" and value % 1 == 0 and value >= 0
end

---@param value unknown
---@return boolean
local function valid_cost(value)
  return type(value) == "table"
    and type(value.amount) == "number"
    and value.amount >= 0
    and type(value.currency) == "string"
    and value.currency:match("^[A-Z][A-Z][A-Z]$") ~= nil
end

---@param value unknown
---@return number? tokens
---@return boolean valid
local function measured_tokens(value)
  if value == nil then
    return nil, true
  end
  if type(value) ~= "table" then
    return nil, false
  end
  if value.total_tokens ~= nil and not is_integer(value.total_tokens) then
    return nil, false
  end
  local total = value.total_tokens
  if total == nil then
    total = 0
    local measured = false
    for _, name in ipairs(USAGE_FIELDS) do
      if value[name] ~= nil then
        if not is_integer(value[name]) then
          return nil, false
        end
        total = total + value[name]
        measured = true
      end
    end
    if not measured then
      return nil, true
    end
  else
    for _, name in ipairs(USAGE_FIELDS) do
      if value[name] ~= nil and not is_integer(value[name]) then
        return nil, false
      end
    end
  end
  return total, true
end

---@param value unknown
---@return louiselm.session.TurnUsage? usage
local function normalize_usage(value)
  local _, valid = measured_tokens(value)
  if not valid or type(value) ~= "table" then
    return nil
  end
  local allowed = {}
  for _, name in ipairs(ALL_USAGE_FIELDS) do
    allowed[name] = true
  end
  for key in pairs(value) do
    if not allowed[key] then
      return nil
    end
  end
  local usage = {}
  for _, name in ipairs(ALL_USAGE_FIELDS) do
    if value[name] ~= nil then
      usage[name] = value[name]
    end
  end
  if next(usage) == nil then
    return nil
  end
  return usage
end

---@param record louiselm.routing.UsageRecord
---@return louiselm.routing.UsageRecord
local function copy_record(record)
  local costs = {}
  for currency, cost in pairs(record.costs) do
    costs[currency] = { samples = cost.samples, total = cost.total }
  end
  return {
    agent = record.agent,
    option = record.option,
    value = record.value,
    samples = record.samples,
    token_samples = record.token_samples,
    total_tokens = record.total_tokens,
    costs = costs,
    updated_at = record.updated_at,
  }
end

---@param record louiselm.routing.UsageTurnRecord
---@return louiselm.routing.UsageTurnRecord
local function copy_turn_record(record)
  return {
    agent = record.agent,
    session_id = record.session_id,
    turn = record.turn,
    usage = normalize_usage(record.usage),
  }
end

---@param value unknown
---@return louiselm.routing.UsageRecord?
local function validate_record(value)
  if type(value) ~= "table" then
    return nil
  end
  local allowed = {
    agent = true,
    option = true,
    value = true,
    samples = true,
    token_samples = true,
    total_tokens = true,
    costs = true,
    updated_at = true,
  }
  for key in pairs(value) do
    if not allowed[key] then
      return nil
    end
  end
  if
    type(value.agent) ~= "string"
    or value.agent == ""
    or type(value.option) ~= "string"
    or value.option == ""
    or (type(value.value) ~= "string" and type(value.value) ~= "boolean")
    or not is_integer(value.samples)
    or not is_integer(value.token_samples)
    or value.token_samples > value.samples
    or not is_integer(value.total_tokens)
    or type(value.costs) ~= "table"
    or not is_integer(value.updated_at)
  then
    return nil
  end
  local costs = {}
  for currency, cost in pairs(value.costs) do
    if
      type(currency) ~= "string"
      or not valid_cost({ amount = type(cost) == "table" and cost.total or nil, currency = currency })
      or type(cost) ~= "table"
      or not is_integer(cost.samples)
      or cost.samples == 0
    then
      return nil
    end
    costs[currency] = { samples = cost.samples, total = cost.total }
  end
  return {
    agent = value.agent,
    option = value.option,
    value = value.value,
    samples = value.samples,
    token_samples = value.token_samples,
    total_tokens = value.total_tokens,
    costs = costs,
    updated_at = value.updated_at,
  }
end

---@param value unknown
---@return louiselm.routing.UsageTurnRecord?
local function validate_turn_record(value)
  if type(value) ~= "table" then
    return nil
  end
  local allowed = { agent = true, session_id = true, turn = true, usage = true }
  for key in pairs(value) do
    if not allowed[key] then
      return nil
    end
  end
  local usage = normalize_usage(value.usage)
  if
    type(value.agent) ~= "string"
    or value.agent == ""
    or type(value.session_id) ~= "string"
    or value.session_id == ""
    or not is_integer(value.turn)
    or value.turn == 0
    or usage == nil
  then
    return nil
  end
  return { agent = value.agent, session_id = value.session_id, turn = value.turn, usage = usage }
end

---@param path string
---@return louiselm.routing.UsageRecord[]? records
---@return louiselm.routing.UsageTurnRecord[]? turns
---@return string? error_message
local function read_store(path)
  local editor = nvim()
  local stat = editor.uv.fs_stat(path)
  if stat == nil then
    return {}, {}, nil
  end
  if stat.type ~= "file" then
    return nil, nil, "usage history path is not a regular file"
  end
  local file, open_error = editor.uv.fs_open(path, "r", 384)
  if file == nil then
    return nil, nil, "could not read usage history: " .. tostring(open_error)
  end
  local content, read_error = editor.uv.fs_read(file, stat.size, 0)
  local closed, close_error = editor.uv.fs_close(file)
  if content == nil then
    return nil, nil, "could not read usage history: " .. tostring(read_error)
  end
  if not closed then
    return nil, nil, "could not close usage history: " .. tostring(close_error)
  end
  local decoded_ok, decoded = pcall(editor.json.decode, content)
  if not decoded_ok then
    return nil, nil, "usage history is not valid JSON"
  end
  if type(decoded) ~= "table" then
    return nil, nil, "usage history has invalid schema"
  end
  for key in pairs(decoded) do
    if key ~= "version" and key ~= "records" and key ~= "turns" then
      return nil, nil, "usage history has invalid schema"
    end
  end
  if
    decoded.version ~= VERSION
    or type(decoded.records) ~= "table"
    or (decoded.turns ~= nil and type(decoded.turns) ~= "table")
  then
    return nil, nil, "usage history has invalid schema"
  end
  local records = {}
  for index, value in ipairs(decoded.records) do
    local record = validate_record(value)
    if record == nil then
      return nil, nil, "usage history has invalid schema"
    end
    records[index] = record
  end
  for key in pairs(decoded.records) do
    if type(key) ~= "number" or key < 1 or key % 1 ~= 0 or key > #records then
      return nil, nil, "usage history has invalid schema"
    end
  end
  local turns = {}
  for index, value in ipairs(decoded.turns or {}) do
    local record = validate_turn_record(value)
    if record == nil then
      return nil, nil, "usage history has invalid schema"
    end
    turns[index] = record
  end
  for key in pairs(decoded.turns or {}) do
    if type(key) ~= "number" or key < 1 or key % 1 ~= 0 or key > #turns then
      return nil, nil, "usage history has invalid schema"
    end
  end
  return records, turns, nil
end

---@param left louiselm.routing.UsageRecord
---@param right louiselm.routing.UsageRecord
---@return boolean
local function before(left, right)
  if left.agent ~= right.agent then
    return left.agent < right.agent
  end
  if left.option ~= right.option then
    return left.option < right.option
  end
  return tostring(left.value) < tostring(right.value)
end

---@param left louiselm.routing.UsageTurnRecord
---@param right louiselm.routing.UsageTurnRecord
---@return boolean
local function turn_before(left, right)
  if left.agent ~= right.agent then
    return left.agent < right.agent
  end
  if left.session_id ~= right.session_id then
    return left.session_id < right.session_id
  end
  return left.turn < right.turn
end

---@param records louiselm.routing.UsageRecord[]
---@param agent string
---@param option string
---@param value string|boolean
---@return louiselm.routing.UsageRecord?
local function find_record(records, agent, option, value)
  for _, record in ipairs(records) do
    if record.agent == agent and record.option == option and record.value == value then
      return record
    end
  end
  return nil
end

---Open the legacy usage ledger for reading only; Session recording owns all new facts.
---@param path? string JSON path. Defaults to stdpath("state")/louiselm/usage.json.
---@return louiselm.routing.Usage? store
---@return string? error_message
function M.new(path)
  if path ~= nil and (type(path) ~= "string" or path == "") then
    return nil, "usage history path must be a non-empty string"
  end
  local editor = nvim()
  local resolved = path or editor.fs.joinpath(editor.fn.stdpath("state"), "louiselm", "usage.json")
  return setmetatable({ path = editor.fs.normalize(resolved) }, Usage), nil
end

---Return detached persistent records.
---@param self louiselm.routing.Usage
---@return louiselm.routing.UsageRecord[]? records
---@return string? error_message
function Usage:records()
  local records, _, read_error = read_store(self.path)
  if records == nil then
    return nil, read_error
  end
  table.sort(records, before)
  local copies = {}
  for index, record in ipairs(records) do
    copies[index] = copy_record(record)
  end
  return copies, nil
end

---Return exact completed-turn usage for one Agent-scoped ACP Session.
---@param self louiselm.routing.Usage
---@param agent unknown Configured Agent name.
---@param session_id unknown Agent-side persistent Session identifier.
---@return louiselm.routing.UsageTurnRecord[]? turns
---@return string? error_message
function Usage:turns(agent, session_id)
  if type(agent) ~= "string" or agent == "" or type(session_id) ~= "string" or session_id == "" then
    return nil, "usage turns require non-empty Agent and Session ids"
  end
  local _, turns, read_error = read_store(self.path)
  if turns == nil then
    return nil, read_error
  end
  table.sort(turns, turn_before)
  local copies = {}
  for _, record in ipairs(turns) do
    if record.agent == agent and record.session_id == session_id then
      copies[#copies + 1] = copy_turn_record(record)
    end
  end
  return copies, nil
end

---Summarize observed usage for one option value; absent data returns nil.
---@param self louiselm.routing.Usage
---@param agent unknown Configured Agent name.
---@param option unknown ACP option identifier.
---@param value unknown Active option value.
---@return louiselm.routing.UsageSummary? summary
---@return string? error_message
function Usage:summary(agent, option, value)
  if type(agent) ~= "string" or agent == "" or type(option) ~= "string" or option == "" then
    return nil, "usage summary requires non-empty agent and option"
  end
  if type(value) ~= "string" and type(value) ~= "boolean" then
    return nil, "usage summary value must be a string or boolean"
  end
  local records, _, read_error = read_store(self.path)
  if records == nil then
    return nil, read_error
  end
  local record = find_record(records, agent, option, value)
  if record == nil then
    return nil, nil
  end
  local summary = {
    samples = record.samples,
    token_samples = record.token_samples,
    costs = {},
  }
  if record.token_samples > 0 then
    summary.average_tokens = record.total_tokens / record.token_samples
  end
  for currency, cost in pairs(record.costs) do
    summary.costs[#summary.costs + 1] = {
      currency = currency,
      samples = cost.samples,
      average = cost.total / cost.samples,
    }
  end
  table.sort(summary.costs, function(left, right)
    return left.currency < right.currency
  end)
  return summary, nil
end

return M
