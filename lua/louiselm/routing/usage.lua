---@class louiselm.routing.UsageOption
---@field id string ACP option identifier.
---@field current_value string|boolean Value active for a completed turn.

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
---@field record fun(self: louiselm.routing.Usage, agent: unknown, options: unknown, usage: unknown, cost?: unknown): boolean, string? Record one completed turn.
---@field record_turn fun(self: louiselm.routing.Usage, agent: unknown, session_id: unknown, turn: unknown, usage: unknown): boolean, string? Persist exact presentation usage for one Session turn.
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
---@return boolean
local function valid_options(value)
  if type(value) ~= "table" then
    return false
  end
  local length = 0
  while value[length + 1] ~= nil do
    local option = value[length + 1]
    if
      type(option) ~= "table"
      or type(option.id) ~= "string"
      or option.id == ""
      or (type(option.current_value) ~= "string" and type(option.current_value) ~= "boolean")
    then
      return false
    end
    length = length + 1
  end
  for key in pairs(value) do
    if type(key) ~= "number" or key < 1 or key % 1 ~= 0 or key > length then
      return false
    end
  end
  return true
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

---@param path string
---@param records louiselm.routing.UsageRecord[]
---@param turns louiselm.routing.UsageTurnRecord[]
---@return boolean written
---@return string? error_message
local function write_store(path, records, turns)
  local editor = nvim()
  local directory = editor.fs.dirname(path)
  if editor.fn.mkdir(directory, "p", 448) == 0 and editor.fn.isdirectory(directory) ~= 1 then
    return false, "could not create usage history directory"
  end
  local encoded_ok, content = pcall(editor.json.encode, { version = VERSION, records = records, turns = turns })
  if not encoded_ok then
    return false, "could not encode usage history"
  end
  local file, temporary_or_error = editor.uv.fs_mkstemp(path .. ".tmp-XXXXXX")
  if file == nil then
    return false, "could not create temporary usage history: " .. tostring(temporary_or_error)
  end
  local temporary = temporary_or_error
  local written, write_error = editor.uv.fs_write(file, content, 0)
  if written ~= #content then
    editor.uv.fs_close(file)
    editor.uv.fs_unlink(temporary)
    return false, "could not write usage history: " .. tostring(write_error or "short write")
  end
  local synced, sync_error = editor.uv.fs_fsync(file)
  if not synced then
    editor.uv.fs_close(file)
    editor.uv.fs_unlink(temporary)
    return false, "could not sync usage history: " .. tostring(sync_error)
  end
  local closed, close_error = editor.uv.fs_close(file)
  if not closed then
    editor.uv.fs_unlink(temporary)
    return false, "could not close usage history: " .. tostring(close_error)
  end
  local renamed, rename_error = editor.uv.fs_rename(temporary, path)
  if not renamed then
    editor.uv.fs_unlink(temporary)
    return false, "could not replace usage history: " .. tostring(rename_error)
  end
  return true, nil
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

---@param turns louiselm.routing.UsageTurnRecord[]
---@param agent string
---@param session_id string
---@param turn integer
---@return louiselm.routing.UsageTurnRecord?
local function find_turn(turns, agent, session_id, turn)
  for _, record in ipairs(turns) do
    if record.agent == agent and record.session_id == session_id and record.turn == turn then
      return record
    end
  end
  return nil
end

---@param agent unknown
---@param options unknown
---@param usage unknown
---@param cost unknown
---@return string? normalized_agent
---@return louiselm.routing.UsageOption[]? normalized_options
---@return number? tokens
---@return louiselm.routing.UsageCost? normalized_cost
---@return string? error_message
local function normalize_input(agent, options, usage, cost)
  if type(agent) ~= "string" or agent == "" then
    return nil, nil, nil, nil, "usage agent must be a non-empty string"
  end
  if not valid_options(options) then
    return nil, nil, nil, nil, "usage options must contain string ids and string or boolean values"
  end
  local tokens, usage_valid = measured_tokens(usage)
  if not usage_valid then
    return nil, nil, nil, nil, "usage measurement is malformed"
  end
  if cost ~= nil and not valid_cost(cost) then
    return nil, nil, nil, nil, "usage cost must contain a non-negative amount and ISO currency"
  end
  if tokens == nil and cost == nil then
    return agent, options, nil, nil, nil
  end
  return agent, options, tokens, cost, nil
end

---Create a persistent measured-usage store.
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

---Record one turn for every option value active during that turn.
---@param self louiselm.routing.Usage
---@param agent unknown Configured Agent name.
---@param options unknown Complete active ACP options.
---@param usage unknown Validated turn usage, when reported.
---@param cost? unknown Per-turn agent-reported cost delta, when available.
---@return boolean recorded
---@return string? error_message
function Usage:record(agent, options, usage, cost)
  local normalized_agent, normalized_options, tokens, normalized_cost, input_error =
    normalize_input(agent, options, usage, cost)
  if normalized_agent == nil then
    return false, input_error
  end
  ---@cast normalized_options louiselm.routing.UsageOption[]
  if tokens == nil and normalized_cost == nil then
    return true, nil
  end
  local records, turns, read_error = read_store(self.path)
  if records == nil then
    return false, read_error
  end
  ---@cast turns louiselm.routing.UsageTurnRecord[]
  for _, option in ipairs(normalized_options) do
    local record = find_record(records, normalized_agent, option.id, option.current_value)
    if record == nil then
      record = {
        agent = normalized_agent,
        option = option.id,
        value = option.current_value,
        samples = 0,
        token_samples = 0,
        total_tokens = 0,
        costs = {},
        updated_at = os.time(),
      }
      records[#records + 1] = record
    end
    record.samples = record.samples + 1
    if tokens ~= nil then
      record.token_samples = record.token_samples + 1
      record.total_tokens = record.total_tokens + tokens
    end
    if normalized_cost ~= nil then
      local measured = record.costs[normalized_cost.currency]
      if measured == nil then
        measured = { samples = 0, total = 0 }
        record.costs[normalized_cost.currency] = measured
      end
      measured.samples = measured.samples + 1
      measured.total = measured.total + normalized_cost.amount
    end
    record.updated_at = os.time()
  end
  table.sort(records, before)
  return write_store(self.path, records, turns)
end

---Persist exact usage for one Agent-scoped ACP Session turn.
---@param self louiselm.routing.Usage
---@param agent unknown Configured Agent name.
---@param session_id unknown Agent-side persistent Session identifier.
---@param turn unknown Positive Session turn ordinal.
---@param usage unknown Validated completed-turn usage.
---@return boolean recorded
---@return string? error_message
function Usage:record_turn(agent, session_id, turn, usage)
  if type(agent) ~= "string" or agent == "" then
    return false, "usage agent must be a non-empty string"
  end
  if type(session_id) ~= "string" or session_id == "" then
    return false, "usage Session id must be a non-empty string"
  end
  if not is_integer(turn) or turn == 0 then
    return false, "usage turn must be a positive integer"
  end
  local normalized_usage = normalize_usage(usage)
  if normalized_usage == nil then
    return false, "usage measurement is malformed"
  end
  local records, turns, read_error = read_store(self.path)
  if records == nil then
    return false, read_error
  end
  ---@cast turns louiselm.routing.UsageTurnRecord[]
  local record = find_turn(turns, agent, session_id, turn)
  if record == nil then
    turns[#turns + 1] = { agent = agent, session_id = session_id, turn = turn, usage = normalized_usage }
  else
    record.usage = normalized_usage
  end
  table.sort(turns, turn_before)
  return write_store(self.path, records, turns)
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
