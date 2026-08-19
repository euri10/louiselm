local MiniTest = require("mini.test")
local Transcript = require("louiselm.session.transcript")

local T = MiniTest.new_set()

---@return louiselm.session.TranscriptIdentity
local function state()
  return { id = "session-1", agent = "claude", acp_session_id = "acp-1" }
end

-- A fixture literal naming one union member's `type` (e.g. "tool_call_started") still
-- has to satisfy every member's field types for lua-language-server to accept it
-- in-place; casting once here is simpler than annotating every fixture below.
---@param fields table
---@return louiselm.session.Event
local function event(fields)
  return fields --[[@as louiselm.session.Event]]
end

---@param haystack string
---@param needle string
---@return integer count
local function count_occurrences(haystack, needle)
  local count = 0
  local start = 1
  while true do
    local from = haystack:find(needle, start, true)
    if from == nil then
      return count
    end
    count = count + 1
    start = from + #needle
  end
end

T["record"] = MiniTest.new_set()

T["record"]["merges consecutive assistant chunks into one block"] = function()
  local transcript = Transcript.new()
  transcript:record(event({
    type = "chunk",
    session_id = "session-1",
    data = { content = { type = "text", text = "hello " } },
  }))
  transcript:record(
    event({ type = "chunk", session_id = "session-1", data = { content = { type = "text", text = "world" } } })
  )

  MiniTest.expect.equality(transcript:snapshot(), {
    { kind = "assistant", text = "hello world" },
  })
end

T["record"]["merges consecutive user_chunk events into one block, separate from assistant text"] = function()
  local transcript = Transcript.new()
  transcript:record(event({
    type = "user_chunk",
    session_id = "session-1",
    data = { content = { type = "text", text = "hi " } },
  }))
  transcript:record(event({
    type = "user_chunk",
    session_id = "session-1",
    data = { content = { type = "text", text = "there" } },
  }))
  transcript:record(
    event({ type = "chunk", session_id = "session-1", data = { content = { type = "text", text = "hello" } } })
  )

  MiniTest.expect.equality(transcript:snapshot(), {
    { kind = "user", text = "hi there" },
    { kind = "assistant", text = "hello" },
  })
end

T["record"]["captures a live-submitted prompt via record_user"] = function()
  local transcript = Transcript.new()
  transcript:record_user("do the thing")

  MiniTest.expect.equality(transcript:snapshot(), {
    { kind = "user", text = "do the thing" },
  })
end

T["record"]["ignores an empty or non-string record_user call"] = function()
  local transcript = Transcript.new()
  transcript:record_user("")
  ---@diagnostic disable-next-line: param-type-mismatch -- exercising the guard against a non-string caller
  transcript:record_user(nil)

  MiniTest.expect.equality(transcript:snapshot(), {})
end

T["record"]["merges a tool call's started and finished payloads into one block, keyed by id"] = function()
  local transcript = Transcript.new()
  transcript:record(event({
    type = "tool_call_started",
    session_id = "session-1",
    data = {
      toolCallId = "tool-1",
      title = "Run tests",
      status = "in_progress",
      rawInput = { command = { "make", "test" } },
    },
  }))
  transcript:record(event({
    type = "tool_call_finished",
    session_id = "session-1",
    data = { toolCallId = "tool-1", status = "completed", rawOutput = { stdout = "ok\n" } },
  }))

  MiniTest.expect.equality(transcript:snapshot(), {
    {
      kind = "tool_call",
      id = "tool-1",
      raw = {
        toolCallId = "tool-1",
        title = "Run tests",
        status = "completed",
        rawInput = { command = { "make", "test" } },
        rawOutput = { stdout = "ok\n" },
      },
    },
  })
end

T["record"]["keeps two interleaved tool calls as separate ordered blocks"] = function()
  local transcript = Transcript.new()
  transcript:record(event({
    type = "tool_call_started",
    session_id = "session-1",
    data = { toolCallId = "tool-1", title = "First", status = "in_progress" },
  }))
  transcript:record(event({
    type = "tool_call_started",
    session_id = "session-1",
    data = { toolCallId = "tool-2", title = "Second", status = "in_progress" },
  }))
  transcript:record(event({
    type = "tool_call_finished",
    session_id = "session-1",
    data = { toolCallId = "tool-1", status = "completed" },
  }))
  transcript:record(event({
    type = "tool_call_finished",
    session_id = "session-1",
    data = { toolCallId = "tool-2", status = "failed" },
  }))

  local snapshot = transcript:snapshot()
  MiniTest.expect.equality(#snapshot, 2)
  MiniTest.expect.equality(snapshot[1].id, "tool-1")
  MiniTest.expect.equality(snapshot[1].raw.status, "completed")
  MiniTest.expect.equality(snapshot[2].id, "tool-2")
  MiniTest.expect.equality(snapshot[2].raw.status, "failed")
end

T["record"]["starts a new assistant block after a tool call interrupts the previous one"] = function()
  local transcript = Transcript.new()
  transcript:record(event({
    type = "chunk",
    session_id = "session-1",
    data = { content = { type = "text", text = "before" } },
  }))
  transcript:record(event({
    type = "tool_call_started",
    session_id = "session-1",
    data = { toolCallId = "tool-1", status = "completed" },
  }))
  transcript:record(
    event({ type = "chunk", session_id = "session-1", data = { content = { type = "text", text = "after" } } })
  )

  local snapshot = transcript:snapshot()
  MiniTest.expect.equality(#snapshot, 3)
  MiniTest.expect.equality(snapshot[1], { kind = "assistant", text = "before" })
  MiniTest.expect.equality(snapshot[2].kind, "tool_call")
  MiniTest.expect.equality(snapshot[3], { kind = "assistant", text = "after" })
end

T["record"]["ignores unrelated event types"] = function()
  local transcript = Transcript.new()
  transcript:record(event({ type = "state_changed", session_id = "session-1", data = { status = "ready" } }))
  transcript:record(event({ type = "turn_done", session_id = "session-1", data = {} }))
  transcript:record(event({ type = "usage_updated", session_id = "session-1", data = {} }))

  MiniTest.expect.equality(transcript:snapshot(), {})
end

T["render"] = MiniTest.new_set()

T["render"]["renders user, assistant, and tool blocks in order with full text"] = function()
  local entries = {
    { kind = "user", text = "run the tests" },
    { kind = "assistant", text = "Running them now." },
    {
      kind = "tool_call",
      id = "tool-1",
      raw = {
        toolCallId = "tool-1",
        title = "Run tests",
        status = "completed",
        rawInput = { command = { "make", "test" } },
        rawOutput = { stdout = string.rep("line\n", 50) },
      },
    },
    { kind = "assistant", text = "All green." },
  }

  local markdown = Transcript.render(entries, state())

  MiniTest.expect.equality(markdown:find("## User", 1, true) ~= nil, true)
  MiniTest.expect.equality(markdown:find("run the tests", 1, true) ~= nil, true)
  MiniTest.expect.equality(markdown:find("## Assistant", 1, true) ~= nil, true)
  MiniTest.expect.equality(markdown:find("Running them now.", 1, true) ~= nil, true)
  MiniTest.expect.equality(markdown:find("## Tool", 1, true) ~= nil, true)
  MiniTest.expect.equality(markdown:find("<sub>**Run tests** — completed</sub>", 1, true) ~= nil, true)
  MiniTest.expect.equality(markdown:find("<details>", 1, true) ~= nil, true)
  MiniTest.expect.equality(markdown:find("<summary>payload</summary>", 1, true) ~= nil, true)
  MiniTest.expect.equality(markdown:find("</details>", 1, true) ~= nil, true)
  -- Full, untruncated tool output: all 50 lines of stdout must survive into the export.
  MiniTest.expect.equality(count_occurrences(markdown, "line"), 50)
  MiniTest.expect.equality(markdown:find("All green.", 1, true) ~= nil, true)

  local user_pos = markdown:find("## User", 1, true)
  local assistant_pos = markdown:find("## Assistant", 1, true)
  local tool_pos = markdown:find("## Tool", 1, true)
  local last_assistant_pos = markdown:find("All green.", 1, true)
  MiniTest.expect.equality(user_pos < assistant_pos, true)
  MiniTest.expect.equality(assistant_pos < tool_pos, true)
  MiniTest.expect.equality(tool_pos < last_assistant_pos, true)
end

T["render"]["identifies the session by state and reports a missing acp session id"] = function()
  local markdown = Transcript.render({}, { id = "session-1", agent = "claude" })

  MiniTest.expect.equality(markdown:find("session: session-1", 1, true) ~= nil, true)
  MiniTest.expect.equality(markdown:find("agent: claude", 1, true) ~= nil, true)
  MiniTest.expect.equality(markdown:find("acp session: none", 1, true) ~= nil, true)
end

T["render"]["is deterministic for the same entries and state"] = function()
  local entries = {
    { kind = "user", text = "hi" },
    { kind = "tool_call", id = "tool-1", raw = { toolCallId = "tool-1", b = 2, a = 1 } },
  }

  MiniTest.expect.equality(Transcript.render(entries, state()), Transcript.render(entries, state()))
end

T["render"]["falls back to the tool call id and an unknown status when absent"] = function()
  local markdown = Transcript.render({
    { kind = "tool_call", id = "tool-9", raw = { toolCallId = "tool-9" } },
  }, state())

  MiniTest.expect.equality(markdown:find("<sub>**tool-9** — unknown</sub>", 1, true) ~= nil, true)
end

return T
