---Bounded, closed conversation projection owned by one live Session.

---@diagnostic disable-next-line: undefined-global -- Neovim provides JSON and UTF-8 facilities.
local nvim = vim
local M = {}
local Projection = {}
Projection.__index = Projection

local STRING_LIMIT = 8192
local ENTRY_LIMIT = 512

---@param value unknown
---@return string text
---@return boolean truncated
local function bounded(value)
  if type(value) ~= "string" then
    return "", true
  end
  local index = 1
  while index <= #value and index <= STRING_LIMIT do
    local first = value:byte(index)
    local width = first < 128 and 1
      or (first >= 194 and first <= 223 and 2)
      or (first >= 224 and first <= 239 and 3)
      or (first >= 240 and first <= 244 and 4)
    if first == 0 or not width or index + width - 1 > #value then
      return "", true
    end
    if index + width - 1 > STRING_LIMIT then
      return value:sub(1, index - 1), true
    end
    for offset = 1, width - 1 do
      local byte = value:byte(index + offset)
      if byte < 128 or byte > 191 then
        return "", true
      end
      if
        offset == 1
        and (
          (first == 224 and byte < 160)
          or (first == 237 and byte > 159)
          or (first == 240 and byte < 144)
          or (first == 244 and byte > 143)
        )
      then
        return "", true
      end
    end
    index = index + width
  end
  return value:sub(1, index - 1), index <= #value
end

-- Only these Agent-advertised categories belong in the phone status view.
local function settings(options)
  local result = { model = "", effort = "", mode = "" }
  local categories = { model = "model", thought_level = "effort", mode = "mode" }
  local seen = {}
  for _, option in ipairs(options or {}) do
    local key = categories[option.category]
    if key ~= nil then
      if seen[key] then
        result[key] = ""
      else
        seen[key] = true
        local value
        if option.type == "select" then
          for _, choice in ipairs(option.options or {}) do
            if choice.value == option.current_value then
              value = choice.name
              break
            end
          end
        elseif option.type == "boolean" and type(option.current_value) == "boolean" then
          value = option.name .. ": " .. tostring(option.current_value)
        end
        local label, invalid = bounded(value)
        if not invalid and #label <= 256 and not label:find("%c") then
          result[key] = label
        end
      end
    end
  end
  return result
end

local function add(self, entry)
  entry.seq = self.next_sequence
  self.next_sequence = self.next_sequence + 1
  self.entries[#self.entries + 1] = entry
  if #self.entries > ENTRY_LIMIT then
    table.remove(self.entries, 1)
    self.omitted = true
  end
end

local function message(self, role, text, merge)
  local incoming, incoming_truncated = bounded(text)
  local last = self.entries[#self.entries]
  if merge and last and last.kind == "message" and last.role == role then
    local value, truncated = bounded(last.text .. incoming)
    last.text = value
    last.truncated = last.truncated or truncated or incoming_truncated
  else
    add(self, { kind = "message", role = role, text = incoming, truncated = incoming_truncated })
  end
end

local function tool_fields(self, value, names)
  if type(value) == "string" then
    return bounded(value)
  end
  if type(value) ~= "table" then
    self.omitted = true
    return "", false
  end
  local parts, allowed, truncated = {}, {}, false
  for _, name in ipairs(names) do
    allowed[name] = true
    if value[name] ~= nil then
      if type(value[name]) == "string" then
        local text, shortened = bounded(value[name])
        parts[#parts + 1] = name .. ": " .. text
        truncated = truncated or shortened
      else
        self.omitted = true
      end
    end
  end
  for name in pairs(value) do
    if not allowed[name] then
      self.omitted = true
      break
    end
  end
  local text, shortened = bounded(table.concat(parts, "\n"))
  return text, truncated or shortened
end

local function tool(self, data)
  local id = data.toolCallId or data.tool_call_id
  if type(id) ~= "string" or id == "" then
    self.omitted = true
    return
  end
  local safe_id, invalid_id = bounded(id)
  if invalid_id then
    self.omitted = true
    return
  end
  id = safe_id
  local entry
  for _, current in ipairs(self.entries) do
    if current.kind == "tool" and current.id == id then
      entry = current
      break
    end
  end
  if entry == nil then
    entry = { kind = "tool", id = id, title = "", status = "unknown", content = "", truncated = false }
    add(self, entry)
  end
  if data.title ~= nil then
    local truncated
    entry.title, truncated = bounded(data.title)
    entry.truncated = entry.truncated or truncated
  end
  if data.status ~= nil then
    local statuses = { pending = true, in_progress = true, completed = true, failed = true }
    entry.status = statuses[data.status] and data.status or "unknown"
    if entry.status == "unknown" then
      self.omitted = true
    end
  end
  local raw_input = data.rawInput or data.raw_input
  if raw_input ~= nil then
    local truncated
    entry._input, truncated =
      tool_fields(self, raw_input, { "command", "path", "filePath", "file_path", "query", "content" })
    entry.truncated = entry.truncated or truncated
  end
  local raw_output = data.rawOutput or data.raw_output
  if raw_output ~= nil then
    local truncated
    entry._output, truncated = tool_fields(self, raw_output, { "stdout", "stderr", "content", "text" })
    entry.truncated = entry.truncated or truncated
  end
  if data.content ~= nil then
    local parts, size = {}, 0
    if type(data.content) == "table" then
      for index, block in ipairs(data.content) do
        local content = type(block) == "table" and block.content or nil
        if index > 50 then
          entry.truncated = true
          break
        elseif type(content) == "table" and content.type == "text" and type(content.text) == "string" then
          local part, truncated = bounded(content.text)
          size = size + #part + (#parts > 0 and 1 or 0)
          parts[#parts + 1] = part
          entry.truncated = entry.truncated or truncated
          if size > STRING_LIMIT then
            entry.truncated = true
            break
          end
        else
          self.omitted = true
        end
      end
    else
      self.omitted = true
    end
    local truncated
    entry._details, truncated = bounded(table.concat(parts, "\n"))
    entry.truncated = entry.truncated or truncated
  end
  local parts = {}
  if entry._input and entry._input ~= "" then
    parts[#parts + 1] = "Input: " .. entry._input
  end
  if entry._output and entry._output ~= "" then
    parts[#parts + 1] = "Output: " .. entry._output
  end
  if entry._details and entry._details ~= "" then
    parts[#parts + 1] = entry._details
  end
  local truncated
  entry.content, truncated = bounded(table.concat(parts, "\n"))
  entry.truncated = entry.truncated or truncated
end

local function interaction(self, data)
  local request_id, id_truncated = bounded(tostring(data.request_id or ""))
  if data.kind == "plan_review" then
    if data.plan_content == nil then
      self.omitted = true
    end
    local text, truncated = bounded(data.plan_content or "")
    add(self, { kind = "plan", request_id = request_id, text = text, truncated = id_truncated or truncated })
  elseif data.kind == "question" and type(data.questions) == "table" then
    for _, question in ipairs(data.questions) do
      local text, truncated = bounded(question.question)
      local entry = {
        kind = "question",
        request_id = request_id,
        question = text,
        options = {},
        multi_select = question.multi_select == true,
        truncated = id_truncated or truncated,
      }
      local size = 0
      for index, option in ipairs(question.options or {}) do
        if index > 50 then
          entry.truncated = true
          break
        end
        local label, label_truncated = bounded(option.label)
        local description, description_truncated = bounded(option.description or "")
        local preview, preview_truncated = bounded(option.preview or "")
        size = size + #label + #description + #preview
        if size > 16384 then
          entry.truncated = true
          break
        end
        entry.options[#entry.options + 1] = { label = label, description = description, preview = preview }
        entry.truncated = entry.truncated or label_truncated or description_truncated or preview_truncated
      end
      add(self, entry)
    end
  else
    self.omitted = true
  end
end

---Record a typed Session event without retaining raw ACP objects or drafts.
---@param event table Typed Session event.
function Projection:record(event)
  local data = event.data
  if event.type == "config_options_changed" then
    self.settings = settings(type(data) == "table" and data or {})
    self.revision = self.revision + 1
    return
  end
  if event.type == "state_changed" then
    self.revision = self.revision + 1
    return
  end
  if type(data) ~= "table" then
    return
  end
  if event.type == "prompt_dispatched" then
    local visible = data.display_text
    if visible == nil and type(data.prompt) == "table" and #data.prompt == 1 then
      local block = data.prompt[1]
      if type(block) == "table" and block.type == "text" then
        visible = block.text
      end
    end
    if type(visible) == "string" and visible ~= "" then
      message(self, "user", visible, false)
    else
      self.omitted = true
    end
  elseif event.type == "chunk" or event.type == "user_chunk" then
    local content = data.content
    local text = data.text or (type(content) == "table" and content.text)
    if type(text) == "string" then
      message(self, event.type == "chunk" and "assistant" or "user", text, true)
    else
      self.omitted = true
    end
  elseif event.type == "tool_call_started" or event.type == "tool_call_finished" then
    tool(self, data)
  elseif event.type == "interaction_requested" then
    interaction(self, data)
  else
    return
  end
  self.revision = self.revision + 1
end

---Return an exact-revision page in ascending sequence order.
---@param before integer Exclusive cursor, or zero for newest.
---@param limit integer Maximum number of entries.
---@param expected_revision integer Exact revision, or zero for current.
---@return table? page Closed page fields.
---@return string? error_code Stale cursor, revision, or disposed Session.
function Projection:page(before, limit, expected_revision)
  local status = self.status()
  if status == "disposed" then
    return nil, "stale_subject"
  end
  if expected_revision ~= 0 and expected_revision ~= self.revision then
    return nil, "stale_subject"
  end
  local last = #self.entries
  if before ~= 0 then
    local found = false
    for index, entry in ipairs(self.entries) do
      if entry.seq == before then
        last = index - 1
        found = true
        break
      end
    end
    if not found then
      return nil, "stale_subject"
    end
  end
  local first = math.max(1, last - limit + 1)
  local entries = {}
  local complete = not self.omitted
  for index = first, last do
    local entry = self.entries[index]
    local copy = nvim.deepcopy(entry)
    copy._input, copy._output, copy._details = nil, nil, nil
    entries[#entries + 1] = copy
    complete = complete and not entry.truncated
  end
  return {
    revision = self.revision,
    status = status,
    settings = nvim.deepcopy(self.settings),
    entries = entries,
    next_before = first > 1 and self.entries[first].seq or 0,
    complete = complete,
  }
end

---Create an empty projection with the owning Session's current status and confirmed options.
---@param status fun(): string Current Session status.
---@param options? louiselm.session.ConfigOption[] Current Agent-confirmed options.
---@return table projection
function M.new(status, options)
  return setmetatable({
    status = status,
    settings = settings(options),
    entries = {},
    next_sequence = 1,
    revision = 1,
    omitted = false,
  }, Projection)
end

return M
