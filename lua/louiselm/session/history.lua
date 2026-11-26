---Bounded, closed reads over the exact live Session's transcript.

---@diagnostic disable-next-line: undefined-global -- Neovim supplies scheduling and JSON.
local nvim = vim
local Projection = require("louiselm.phone_control.projection")
local M = {}
local History = {}
History.__index = History

local MAX_INTEGER = 9007199254740991
local MAX_BYTES = 245760

---@class louiselm.session.HistoryRequest
---@field subject? string Incarnation from the first page; required for continuation.
---@field before integer Exclusive sequence cursor; zero starts at the newest entry.
---@field limit integer Maximum entries examined, from 1 through 50.
---@field revision integer Exact first-page revision for continuation, or zero for current state.

---@class louiselm.session.HistorySource
---@field replay "unverified"|"not_applicable" ACP replay has no completeness guarantee.
---@field archive "unavailable" No archive has been bound to this Session by its owner.
---@field redaction "present"|"unknown" Known exclusions, or no source redaction evidence.
---@field missing boolean Unsupported or missing source material was observed.
---@field truncated boolean An entry on this page was shortened.

---@class louiselm.session.HistoryMessage
---@field kind "message"
---@field seq integer
---@field role "user"|"assistant"
---@field text string
---@field truncated boolean

---@class louiselm.session.HistoryTool
---@field kind "tool"
---@field seq integer
---@field id string
---@field title string
---@field status "pending"|"in_progress"|"completed"|"failed"|"unknown"
---@field content string
---@field truncated boolean

---@class louiselm.session.HistoryOption
---@field label string
---@field description string
---@field preview string

---@class louiselm.session.HistoryQuestion
---@field kind "question"
---@field seq integer
---@field request_id string
---@field question string
---@field options louiselm.session.HistoryOption[]
---@field multi_select boolean
---@field truncated boolean

---@class louiselm.session.HistoryPlan
---@field kind "plan"
---@field seq integer
---@field request_id string
---@field text string
---@field truncated boolean

---@alias louiselm.session.HistoryEntry louiselm.session.HistoryMessage|louiselm.session.HistoryTool|louiselm.session.HistoryQuestion|louiselm.session.HistoryPlan

---@class louiselm.session.CurrentPermission
---@field title string Bounded tool title, or empty when unavailable.
---@field options string[] Offered labels, without response identifiers.
---@field incomplete boolean Some display information was unavailable or shortened.

---@class louiselm.session.CurrentInteractions
---@field entries (louiselm.session.HistoryQuestion|louiselm.session.HistoryPlan)[] Display-only entries, with their original history sequences.
---@field incomplete boolean Some current request information was unavailable or shortened.

---@class louiselm.session.HistoryPage
---@field subject string Exact Session incarnation; never authority on its own.
---@field revision integer Snapshot revision, invalidated by a projected update.
---@field status louiselm.session.Status Session status at this revision.
---@field current_permission? louiselm.session.CurrentPermission Active request at this revision, never response authority.
---@field current_interactions? louiselm.session.CurrentInteractions Pending questions and plans at this revision, never response authority.
---@field entries louiselm.session.HistoryEntry[] Closed variants, ascending by sequence.
---@field next_before integer Exclusive cursor, or zero when no older entries remain.
---@field complete boolean Whether the projected snapshot and this page have no known gaps.
---@field source louiselm.session.HistorySource Source and projection limitations.

---@alias louiselm.session.HistoryCallback fun(page: louiselm.session.HistoryPage?, error_code: string?)

---@class louiselm.session.History
---@field session louiselm.session.Session
---@field transcript louiselm.session.Transcript
---@field entries table[] References to owned transcript entries or bounded interaction projections.
---@field transcript_entries table<louiselm.session.TranscriptEntry, table> Timeline entries keyed by their owner-held transcript entry.
---@field recorded integer
---@field revision integer
---@field missing boolean
---@field redacted boolean
---@field record fun(self: louiselm.session.History, event: louiselm.session.Event)
---@field read fun(self: louiselm.session.History, request: louiselm.session.HistoryRequest, callback: louiselm.session.HistoryCallback): fun()

local relevant = {
  state_changed = true,
  config_options_changed = true,
  prompt_dispatched = true,
  user_chunk = true,
  chunk = true,
  thought_chunk = true,
  tool_call_started = true,
  tool_call_finished = true,
  interaction_requested = true,
  interaction_retired = true,
  interaction_cancelled = true,
  permission_requested = true,
  permission_resolved = true,
  permission_cancelled = true,
  compaction_updated = true,
}

local function project(event)
  local projection = Projection.new(function()
    return "ready"
  end)
  projection:record(event)
  local page = projection:page(0, 50, 0)
  return page.entries, projection.omitted
end

local function interaction(self, event)
  local data = event.data
  local first = #self.entries + 1
  local selected = data
  if data.kind == "question" and type(data.questions) == "table" and #data.questions > 50 then
    selected = { kind = data.kind, request_id = data.request_id, questions = {} }
    for index = 1, 50 do
      selected.questions[index] = data.questions[index]
    end
    self.missing = true
  end
  local entries, omitted = project({ type = "interaction_requested", data = selected })
  self.missing = self.missing or omitted
  for _, entry in ipairs(entries) do
    entry.seq = nil
    self.entries[#self.entries + 1] = { projection = entry }
  end
  for _, pending in ipairs(self.session.interactions) do
    if pending.event == event then
      pending.history_first = first
      pending.history_last = #self.entries
      pending.history_incomplete = omitted or selected ~= data
      break
    end
  end
end

---Record ordering and source gaps without retaining another copy of raw tool data.
---@param event louiselm.session.Event
function History:record(event)
  if not relevant[event.type] then
    return
  end
  local transcript = self.transcript.entries
  for index = self.recorded + 1, #transcript do
    local entry = transcript[index]
    if entry.kind == "user" or entry.kind == "assistant" or entry.kind == "tool_call" then
      local hidden = false
      if event.type == "prompt_dispatched" and event.data.display_text == nil then
        local prompt = event.data.prompt
        hidden = #prompt ~= 1
          or type(prompt[1]) ~= "table"
          or prompt[1].type ~= "text"
          or type(prompt[1].text) ~= "string"
      end
      local item = { transcript = entry, hidden = hidden }
      self.entries[#self.entries + 1] = item
      self.transcript_entries[entry] = item
    elseif entry.kind == "reasoning" or entry.kind == "compaction" then
      self.redacted = true
    end
  end
  self.recorded = #transcript
  if event.type == "interaction_requested" then
    interaction(self, event)
  elseif event.type == "tool_call_started" or event.type == "tool_call_finished" then
    local data = event.data
    local id = type(data) == "table" and (data.toolCallId or data.tool_call_id) or nil
    if type(id) ~= "string" or id == "" then
      self.missing = true
    end
  elseif event.type == "chunk" or event.type == "user_chunk" then
    local data = event.data
    local content = type(data) == "table" and data.content or nil
    local text = type(data) == "table" and (data.text or (type(content) == "table" and content.text)) or nil
    if type(text) ~= "string" then
      self.missing = true
    elseif type(content) == "table" and content.type ~= nil and content.type ~= "text" then
      local last = transcript[#transcript]
      local item = last and self.transcript_entries[last] or nil
      if item ~= nil then
        item.hidden = true
      end
      self.redacted = true
    end
  end
  self.revision = self.revision + 1
end

local function integer(value, minimum, maximum)
  return type(value) == "number" and value >= minimum and value <= maximum and value % 1 == 0
end

local function valid(request)
  if type(request) ~= "table" then
    return false
  end
  for key in pairs(request) do
    if key ~= "subject" and key ~= "before" and key ~= "limit" and key ~= "revision" then
      return false
    end
  end
  return (request.subject == nil or type(request.subject) == "string")
    and integer(request.before, 0, MAX_INTEGER)
    and integer(request.limit, 1, 50)
    and integer(request.revision, 0, MAX_INTEGER)
end

local function entry_for(item, sequence)
  if item.projection ~= nil then
    local entry = nvim.deepcopy(item.projection)
    entry.seq = sequence
    return entry, false, false
  end
  local entry = item.transcript
  local redacted = item.hidden
  if entry.kind == "user" and type(entry.text) == "string" and entry.text:sub(1, 8192):find("[resource]", 1, true) then
    redacted = true
  end
  local event
  local missing = false
  if entry.kind == "user" or entry.kind == "assistant" then
    event = {
      type = entry.kind == "user" and "user_chunk" or "chunk",
      data = { text = redacted and "" or entry.text },
    }
  else
    local raw = entry.raw or {}
    local input = raw.rawInput
    if input == nil then
      input = raw.raw_input
    end
    local output = raw.rawOutput
    if output == nil then
      output = raw.raw_output
    end
    missing = type(raw.title) ~= "string"
      or type(raw.status) ~= "string"
      or (raw.status == "completed" and output == nil and raw.content == nil)
    event = {
      type = "tool_call_started",
      data = {
        toolCallId = entry.id,
        title = raw.title,
        status = raw.status,
        rawInput = input,
        rawOutput = output,
        content = raw.content,
      },
    }
    local allowed = {
      toolCallId = true,
      tool_call_id = true,
      title = true,
      status = true,
      rawInput = true,
      raw_input = true,
      rawOutput = true,
      raw_output = true,
      content = true,
    }
    for key in pairs(raw) do
      if not allowed[key] then
        redacted = true
        break
      end
    end
  end
  local projected, omitted = project(event)
  local result = projected[1]
  if result ~= nil then
    result.seq = sequence
  end
  return result, redacted or omitted, missing or result == nil
end

local function current_interactions(self)
  if #self.session.interactions == 0 then
    return nil
  end
  local current = { entries = {}, incomplete = false }
  local bytes = 0
  for _, pending in ipairs(self.session.interactions) do
    current.incomplete = current.incomplete or pending.history_incomplete or pending.history_first == nil
    for index = pending.history_first or 1, pending.history_last or 0 do
      local entry = entry_for(self.entries[index], index)
      if entry == nil then
        current.incomplete = true
      else
        local size = #nvim.json.encode(entry) + 1
        if #current.entries >= 50 or bytes + size > 65536 then
          current.incomplete = true
          break
        end
        current.entries[#current.entries + 1] = entry
        bytes = bytes + size
        current.incomplete = current.incomplete or entry.truncated
      end
    end
  end
  return current
end

local function page(self, request)
  local session = self.session
  local subject = session.option_observer_id
  if session.state.status == "disposed" or session.owner.sessions[session.state.id] ~= session or subject == "" then
    return nil, "stale_subject"
  end
  if not valid(request) then
    return nil, "malformed"
  end
  if
    (request.subject ~= nil and request.subject ~= subject)
    or (request.before ~= 0 and (request.subject ~= subject or request.revision == 0))
    or (request.revision ~= 0 and request.subject ~= subject)
    or (request.revision ~= 0 and request.revision ~= self.revision)
    or request.before > #self.entries
  then
    return nil, "stale_subject"
  end
  local permission = session.permission_event and Projection.permission(session.permission_event.data) or nil
  local current = current_interactions(self)
  local reserved = (permission and #nvim.json.encode(permission) or 0) + (current and #nvim.json.encode(current) or 0)
  local last = request.before == 0 and #self.entries or request.before - 1
  local entries, bytes = {}, 0
  local first = last + 1
  local redacted, missing, truncated = self.redacted, self.missing, false
  for index = last, math.max(1, last - request.limit + 1), -1 do
    local entry, hidden, absent = entry_for(self.entries[index], index)
    if entry ~= nil then
      local size = #nvim.json.encode(entry) + 1
      if bytes + size + reserved > MAX_BYTES then
        break
      end
      bytes = bytes + size
      table.insert(entries, 1, entry)
      truncated = truncated or entry.truncated
    end
    first = index
    redacted = redacted or hidden
    missing = missing or absent
  end
  if first > last and last > 0 then
    return nil, "unavailable"
  end
  local replay = session.state.source == "loaded" and "unverified" or "not_applicable"
  return {
    subject = subject,
    revision = self.revision,
    status = session.state.status,
    current_permission = permission,
    current_interactions = current,
    entries = entries,
    next_before = first > 1 and first or 0,
    complete = replay == "not_applicable"
      and not redacted
      and not missing
      and not truncated
      and (permission == nil or not permission.incomplete)
      and (current == nil or not current.incomplete),
    source = {
      replay = replay,
      archive = "unavailable",
      redaction = redacted and "present" or "unknown",
      missing = missing,
      truncated = truncated,
    },
  }
end

---Schedule one bounded read; cancellation suppresses its callback.
---@param request louiselm.session.HistoryRequest
---@param callback louiselm.session.HistoryCallback
---@return fun() cancel
function History:read(request, callback)
  if type(callback) ~= "function" then
    error("history callback must be a function")
  end
  local input
  if valid(request) then
    input = {
      subject = request.subject,
      before = request.before,
      limit = request.limit,
      revision = request.revision,
    }
  end
  local cancelled = false
  nvim.schedule(function()
    if not cancelled then
      local result, err = page(self, input)
      callback(result, err)
    end
  end)
  return function()
    cancelled = true
  end
end

---Create history bound to one Session and its sole transcript.
---@param session louiselm.session.Session
---@return louiselm.session.History
function M.new(session)
  return setmetatable({
    session = session,
    transcript = session.transcript,
    entries = {},
    transcript_entries = {},
    recorded = 0,
    revision = 1,
    missing = false,
    redacted = false,
  }, History)
end

return M
