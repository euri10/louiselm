---Structured session transcript recording and markdown rendering.
---
---A transcript is a plain sequence of blocks reconstructed from a session's typed
---event stream plus the user's own submitted prompt text -- a live-turn prompt never
---arrives as an ACP event (the local client is what sends it), so `record_user` is
---the client-side capture point for it. Each block is one of:
---  - "user": text the local client submitted, or, for a resumed session, text the
---    agent replayed as `user_chunk` events.
---  - "assistant": agent response text, accumulated across consecutive `chunk`
---    events.
---  - "reasoning": agent reasoning text (`agent_thought_chunk` events), kept
---    separate from the answer it precedes.
---  - "tool_call": one ACP tool call, with every field the agent ever sent across
---    its `tool_call`/`tool_call_update` notifications merged together (a later
---    value for the same field wins), so the exported command/result is never
---    truncated.
---Consecutive events of the same kind merge into one block; a different kind (or a
---tool call with a different id) always starts a new block, so tool calls
---interleaved with assistant text render as separate, ordered blocks.
---
---`M.render` turns a snapshot of these blocks into markdown: one `## User`,
---`## Assistant`, or `## Reasoning` section per block holding the full text, or one
---`## Tool` section per tool call holding a `<sub>` line with its title/status and a
---`<details>` block (collapsed by default) with a `vim.inspect` dump of its full raw
---payload, in arrival order. This format is a dependency of later blog tooling
---(louiselm-mia); keep it a plain, deterministic mapping from entries to text
---rather than a templating system.

---@alias louiselm.session.TranscriptEntryKind "user"|"assistant"|"reasoning"|"tool_call"

---@class louiselm.session.TranscriptEntry
---@field kind louiselm.session.TranscriptEntryKind
---@field text? string Accumulated text for a "user", "assistant", or "reasoning" entry.
---@field handoff_source_session_id? string Source Session identity for a Handoff user entry.
---@field id? string ACP tool call id for a "tool_call" entry.
---@field raw? table Every field seen across this tool call's ACP notifications.

---@alias louiselm.session.TranscriptIdentity { id: string, agent: string, acp_session_id?: string }

---@class louiselm.session.Transcript
---@field entries louiselm.session.TranscriptEntry[] Recorded blocks, in arrival order.
---@field open_tool_calls table<string, louiselm.session.TranscriptEntry> Tool calls awaiting a terminal status, by ACP tool call id.
---@field record fun(self: louiselm.session.Transcript, event: louiselm.session.Event) Ingest one session event.
---@field record_user fun(self: louiselm.session.Transcript, text: string) Record a live, client-submitted prompt.
---@field record_handoff fun(self: louiselm.session.Transcript, text: string, source_session_id: string) Record a Handoff prompt and its source Session.
---@field snapshot fun(self: louiselm.session.Transcript): louiselm.session.TranscriptEntry[] Copy of recorded entries in order.

local M = {}
local Transcript = {}
Transcript.__index = Transcript

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

---@param value unknown
---@return string? text
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
---@return string? id
local function tool_call_id(value)
  if type(value) ~= "table" then
    return nil
  end
  if type(value.toolCallId) == "string" and value.toolCallId ~= "" then
    return value.toolCallId
  end
  if type(value.tool_call_id) == "string" and value.tool_call_id ~= "" then
    return value.tool_call_id
  end
  return nil
end

---@param self louiselm.session.Transcript
---@param kind "user"|"assistant"|"reasoning"
---@param text string
local function append_text(self, kind, text)
  local last = self.entries[#self.entries]
  if last ~= nil and last.kind == kind then
    last.text = last.text .. text
    return
  end
  self.entries[#self.entries + 1] = { kind = kind, text = text }
end

---Merge one ACP `tool_call`/`tool_call_update` payload into its block, creating the
---block on first sight and keeping its position where the call started.
---@param self louiselm.session.Transcript
---@param data unknown Raw ACP payload.
local function merge_tool_call(self, data)
  local id = tool_call_id(data)
  if id == nil then
    return
  end
  local entry = self.open_tool_calls[id]
  if entry == nil then
    entry = { kind = "tool_call", id = id, raw = {} }
    self.entries[#self.entries + 1] = entry
    self.open_tool_calls[id] = entry
  end
  local fields = data --[[@as table<string, unknown>]]
  for key, value in pairs(fields) do
    if key ~= "sessionUpdate" then
      entry.raw[key] = nvim.deepcopy(value)
    end
  end
end

---Ingest one typed session event. Only the events a transcript export needs carry
---content here (`chunk`, `user_chunk`, `thought_chunk`, `tool_call_started`,
---`tool_call_finished`); state/permission/usage events are silently ignored.
---@param self louiselm.session.Transcript
---@param event louiselm.session.Event
function Transcript:record(event)
  if event.type == "chunk" then
    local text = chunk_text(event.data)
    if text ~= nil then
      append_text(self, "assistant", text)
    end
  elseif event.type == "user_chunk" then
    local text = chunk_text(event.data)
    if text ~= nil then
      append_text(self, "user", text)
    end
  elseif event.type == "thought_chunk" then
    local text = chunk_text(event.data)
    if text ~= nil then
      append_text(self, "reasoning", text)
    end
  elseif event.type == "tool_call_started" or event.type == "tool_call_finished" then
    merge_tool_call(self, event.data)
    if event.type == "tool_call_finished" then
      local id = tool_call_id(event.data)
      if id ~= nil then
        self.open_tool_calls[id] = nil
      end
    end
  end
end

---Record a prompt the local client just submitted. Live-turn user prompts never
---arrive as ACP events -- the local client is what sends them -- so this is the only
---capture point for them; resumed-session history arrives as `user_chunk` events
---through `record` instead.
---@param self louiselm.session.Transcript
---@param text string Prompt text as authored, without injected context markers.
function Transcript:record_user(text)
  if type(text) ~= "string" or text == "" then
    return
  end
  append_text(self, "user", text)
end

---Record a Handoff prompt submitted by the local client. Handoff entries remain
---separate from ordinary user prompts so their source Session provenance stays
---attached to the exact reviewed text.
---@param self louiselm.session.Transcript
---@param text string Reviewed prompt text as sent to the target Session.
---@param source_session_id string Source Session identity.
function Transcript:record_handoff(text, source_session_id)
  if type(text) ~= "string" or text == "" or type(source_session_id) ~= "string" or source_session_id == "" then
    return
  end
  self.entries[#self.entries + 1] = {
    kind = "user",
    text = text,
    handoff_source_session_id = source_session_id,
  }
end

---Return a deep copy of the recorded entries, in arrival order.
---@param self louiselm.session.Transcript
---@return louiselm.session.TranscriptEntry[] entries
function Transcript:snapshot()
  return nvim.deepcopy(self.entries)
end

---Create an empty transcript recorder.
---@return louiselm.session.Transcript transcript
function M.new()
  return setmetatable({ entries = {}, open_tool_calls = {} }, Transcript)
end

---Collapse a value onto one line so it can be interpolated into a single-line
---construct. Shell tools carry their whole command as the title, so a real
---payload's title is routinely multi-line; leaving those newlines in place
---splits the `<sub>` caption across the document, leaves the element unclosed,
---and renders the command body as markdown -- a `##` line inside it fabricates
---a section indistinguishable from this format's own (louiselm-cmr).
---@param value string
---@return string collapsed
local function single_line(value)
  local collapsed = (value:gsub("%s*[\r\n]+%s*", " "))
  local without_leading = (collapsed:gsub("^%s+", ""))
  return (without_leading:gsub("%s+$", ""))
end

---Longest tool title kept in the `<sub>` caption, in characters. A caption is a
---label, not the payload: shell tools carry their whole command as the title and
---routinely run to thousands of characters, which a fixed-width renderer cannot
---show without horizontal overflow. Nothing is lost by cutting it here -- the
---untruncated title is always a few lines below inside `<details>`.
local CAPTION_TITLE_LIMIT = 120

---Shorten a caption to `CAPTION_TITLE_LIMIT`, counting characters rather than
---bytes so a multi-byte character is never split in half.
---@param value string
---@return string shortened
local function truncate(value)
  if nvim.fn.strchars(value) <= CAPTION_TITLE_LIMIT then
    return value
  end
  return nvim.fn.strcharpart(value, 0, CAPTION_TITLE_LIMIT) .. "…"
end

---@param entry louiselm.session.TranscriptEntry
---@return string[] lines
local function render_entry(entry)
  if entry.kind == "user" then
    local lines = { "## User", "" }
    if entry.handoff_source_session_id ~= nil then
      lines[#lines + 1] = "<sub>Handoff from Session: `" .. entry.handoff_source_session_id .. "`</sub>"
      lines[#lines + 1] = ""
    end
    lines[#lines + 1] = entry.text or ""
    lines[#lines + 1] = ""
    return lines
  end
  if entry.kind == "assistant" then
    return { "## Assistant", "", entry.text or "", "" }
  end
  if entry.kind == "reasoning" then
    return { "## Reasoning", "", entry.text or "", "" }
  end
  local raw = entry.raw or {}
  local title = type(raw.title) == "string" and truncate(single_line(raw.title)) or entry.id
  local status = type(raw.status) == "string" and single_line(raw.status) or "unknown"
  return {
    "## Tool",
    "",
    "<sub>**" .. tostring(title) .. "** — " .. status .. "</sub>",
    "",
    "<details>",
    "<summary>payload</summary>",
    "",
    "```",
    nvim.inspect(raw),
    "```",
    "",
    "</details>",
    "",
  }
end

---Render a transcript snapshot as markdown. Pure and deterministic: the same
---entries and session state always produce the same text. See the module doc
---comment above for the exact format.
---@param entries louiselm.session.TranscriptEntry[] Transcript snapshot, in order.
---@param state louiselm.session.TranscriptIdentity Session identity to label the export with; a full `louiselm.session.State` satisfies this.
---@return string markdown
function M.render(entries, state)
  local lines = {
    "# louiselm session transcript",
    "",
    "- session: " .. state.id,
    "- agent: " .. state.agent,
    "- acp session: " .. (state.acp_session_id or "none"),
    "",
  }
  for _, entry in ipairs(entries) do
    nvim.list_extend(lines, render_entry(entry))
  end
  return table.concat(lines, "\n")
end

return M
