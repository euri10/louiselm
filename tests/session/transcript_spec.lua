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

---Iterate `text` line by line, without depending on the `vim` global.
---@param text string
---@return fun(): string?
local function lines_of(text)
  return (text .. "\n"):gmatch("([^\n]*)\n")
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

T["Handoff uses the latest completed summary with boundary overlap and subsequent conversation"] = function()
  local transcript = Transcript.new()
  local function record(kind, data)
    transcript:record(event({ type = kind, session_id = "s", data = data }))
  end
  transcript:record_user("old user")
  record("chunk", { text = "obsolete history" })
  record("tool_call_started", { toolCallId = "spanning", title = "Running check", status = "in_progress" })
  transcript:record_user("current instruction")
  record(
    "compaction_updated",
    { id = "first", status = "completed", summary = { { type = "text", text = "first summary" } } }
  )
  record("tool_call_finished", { toolCallId = "spanning", status = "completed", rawOutput = "private tool payload" })
  record("chunk", { text = "result after compaction" })
  local first = Transcript.render_handoff(transcript:snapshot(), state())
  for _, text in ipairs({
    "first summary",
    "Running check",
    "completed",
    "current instruction",
    "result after compaction",
  }) do
    MiniTest.expect.equality(first:find(text, 1, true) ~= nil, true)
  end
  MiniTest.expect.equality(first:find("obsolete history", 1, true), nil)
  MiniTest.expect.equality(first:find("private tool payload", 1, true), nil)
  transcript:record_user("next instruction")
  record(
    "compaction_updated",
    { id = "second", status = "completed", summary = { { type = "text", text = "second summary" } } }
  )
  record(
    "compaction_updated",
    { id = "first", status = "completed", summary = { { type = "text", text = "late first replacement" } } }
  )
  record("chunk", { text = "latest answer" })
  local snapshot = transcript:snapshot()
  local latest = Transcript.render_handoff(snapshot, state())
  MiniTest.expect.equality(latest:find("second summary", 1, true) ~= nil, true)
  MiniTest.expect.equality(latest:find("late first replacement", 1, true), nil)
  MiniTest.expect.equality(latest:find("latest answer", 1, true) ~= nil, true)
  MiniTest.expect.equality(snapshot, transcript:snapshot())
  MiniTest.expect.equality(Transcript.render(snapshot, state()):find("obsolete history", 1, true) ~= nil, true)
end

T["Handoff falls back for unusable summaries and retains later failed compactions"] = function()
  for _, entity in ipairs({
    { id = "c", status = "in_progress", summary = { { type = "text", text = "unfinished" } } },
    { id = "c", status = "failed" },
    { id = "c", status = "cancelled" },
    { id = "c", status = "_unknown", summary = { { type = "text", text = "opaque" } } },
    { id = "c", status = "completed" },
    { id = "c", status = "completed", summary = {} },
    { id = "c", status = "completed", summary = { { type = "image", data = "blob" } } },
  }) do
    local transcript = Transcript.new()
    transcript:record_user("preserved history")
    transcript:record(event({ type = "compaction_updated", session_id = "s", data = entity }))
    local entries = transcript:snapshot()
    MiniTest.expect.equality(Transcript.render_handoff(entries, state()), Transcript.render_compact(entries, state()))
  end
  local ambiguous = {
    { kind = "user", text = "unknown boundary history" },
    {
      kind = "compaction",
      compaction = { id = "c", status = "completed", summary = { { type = "text", text = "retained" } } },
    },
  }
  MiniTest.expect.equality(Transcript.render_handoff(ambiguous, state()), Transcript.render_compact(ambiguous, state()))
  local transcript = Transcript.new()
  transcript:record_user("before")
  transcript:record(event({
    type = "compaction_updated",
    session_id = "s",
    data = { id = "c", status = "completed", summary = { { type = "text", text = "retained" } } },
  }))
  transcript:record_user("intervening work")
  transcript:record(
    event({ type = "compaction_updated", session_id = "s", data = { id = "failed", status = "failed" } })
  )
  local text = Transcript.render_handoff(transcript:snapshot(), state())
  for _, expected in ipairs({ "retained", "intervening work", "failed" }) do
    MiniTest.expect.equality(text:find(expected, 1, true) ~= nil, true)
  end
end

T["record"]["compaction patches retain timeline position and full history"] = function()
  local transcript = Transcript.new()
  transcript:record_user("original instruction")
  transcript:record(event({ type = "chunk", session_id = "s", data = { text = "old answer" } }))
  transcript:record(
    event({ type = "compaction_updated", session_id = "s", data = { id = "c", status = "in_progress" } })
  )
  transcript:record_user("new instruction")
  transcript:record(event({
    type = "compaction_updated",
    session_id = "s",
    data = { id = "c", status = "completed", summary = { { type = "text", text = "retained context" } } },
  }))
  local entries = transcript:snapshot()
  MiniTest.expect.equality(#entries, 4)
  MiniTest.expect.equality(entries[3].kind, "compaction")
  MiniTest.expect.equality(entries[4].text, "new instruction")
  local full = Transcript.render(entries, state())
  for _, text in ipairs({ "original instruction", "old answer", "retained context", "new instruction" }) do
    MiniTest.expect.equality(full:find(text, 1, true) ~= nil, true)
  end
end

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

T["record"]["merges consecutive thought_chunk events into one reasoning block, separate from the answer"] = function()
  local transcript = Transcript.new()
  transcript:record(event({
    type = "thought_chunk",
    session_id = "session-1",
    data = { content = { type = "text", text = "**planned** " } },
  }))
  transcript:record(event({
    type = "thought_chunk",
    session_id = "session-1",
    data = { content = { type = "text", text = "the steps" } },
  }))
  transcript:record(
    event({ type = "chunk", session_id = "session-1", data = { content = { type = "text", text = "answer" } } })
  )

  MiniTest.expect.equality(transcript:snapshot(), {
    { kind = "reasoning", text = "**planned** the steps" },
    { kind = "assistant", text = "answer" },
  })
end

T["record"]["starts a new reasoning block when the answer interrupts it"] = function()
  local transcript = Transcript.new()
  transcript:record(event({
    type = "chunk",
    session_id = "session-1",
    data = { content = { type = "text", text = "before" } },
  }))
  transcript:record(event({
    type = "thought_chunk",
    session_id = "session-1",
    data = { content = { type = "text", text = "mid-thought" } },
  }))
  transcript:record(
    event({ type = "chunk", session_id = "session-1", data = { content = { type = "text", text = "after" } } })
  )

  MiniTest.expect.equality(transcript:snapshot(), {
    { kind = "assistant", text = "before" },
    { kind = "reasoning", text = "mid-thought" },
    { kind = "assistant", text = "after" },
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

T["render"]["renders a reasoning section between the user prompt and the answer"] = function()
  local markdown = Transcript.render({
    { kind = "user", text = "solve it" },
    { kind = "reasoning", text = "**deduced** the label order" },
    { kind = "assistant", text = "the answer" },
  }, state())

  local user_at = assert(markdown:find("## User", 1, true))
  local reasoning_at = assert(markdown:find("## Reasoning", 1, true))
  local assistant_at = assert(markdown:find("## Assistant", 1, true))
  MiniTest.expect.equality(user_at < reasoning_at, true)
  MiniTest.expect.equality(reasoning_at < assistant_at, true)
  MiniTest.expect.equality(markdown:find("**deduced** the label order", 1, true) ~= nil, true)
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

-- Shell tools carry the whole command as their title, so a real payload's title
-- is routinely multi-line. Leaking those newlines splits the single-line `<sub>`
-- caption across the document, leaves the element unclosed, and lets the command
-- body render as markdown -- including any `##` line in it, which fabricates
-- sections indistinguishable from the export's own (louiselm-cmr).
T["render"]["keeps the caption on one line when the tool title spans several"] = function()
  local markdown = Transcript.render({
    {
      kind = "tool_call",
      id = "tool-1",
      raw = {
        toolCallId = "tool-1",
        title = 'br create --description "$(cat <<EOF\n## Observed\nit broke\nEOF\n)"',
        status = "completed",
      },
    },
  }, state())

  local captions = 0
  local headings = 0
  for line in lines_of(markdown) do
    if line:find("<sub>", 1, true) ~= nil then
      captions = captions + 1
      MiniTest.expect.equality(line:sub(-6), "</sub>")
    end
    if line:sub(1, 2) == "##" then
      headings = headings + 1
    end
  end

  MiniTest.expect.equality(captions, 1)
  -- Exactly the one real `## Tool`: the `## Observed` inside the title must not
  -- have escaped the caption to become a section of its own.
  MiniTest.expect.equality(headings, 1)
end

-- A caption is a label. Shell tool titles run to several thousand characters in
-- practice (7319 was the longest in a real 156-call export), which a
-- fixed-width renderer cannot show without horizontal overflow -- the blog
-- pipeline's TOhtml excerpts especially. Nothing is lost by cutting it: the
-- untruncated payload sits a few lines below in the `<details>` block.
T["render"]["truncates an overlong title in the caption but keeps it whole in the payload"] = function()
  local long_title = string.rep("x", 400)
  local markdown = Transcript.render({
    {
      kind = "tool_call",
      id = "tool-1",
      raw = { toolCallId = "tool-1", title = long_title, status = "completed" },
    },
  }, state())

  MiniTest.expect.equality(
    markdown:find("<sub>**" .. string.rep("x", 120) .. "…** — completed</sub>", 1, true) ~= nil,
    true
  )
  MiniTest.expect.equality(markdown:find(long_title, 1, true) ~= nil, true)
end

T["render"]["truncates on character boundaries, not bytes"] = function()
  local markdown = Transcript.render({
    {
      kind = "tool_call",
      id = "tool-1",
      raw = { toolCallId = "tool-1", title = string.rep("é", 400), status = "completed" },
    },
  }, state())

  MiniTest.expect.equality(
    markdown:find("<sub>**" .. string.rep("é", 120) .. "…** — completed</sub>", 1, true) ~= nil,
    true
  )
end

T["render"]["leaves a title at the limit untouched"] = function()
  local markdown = Transcript.render({
    {
      kind = "tool_call",
      id = "tool-1",
      raw = { toolCallId = "tool-1", title = string.rep("x", 120), status = "completed" },
    },
  }, state())

  MiniTest.expect.equality(
    markdown:find("<sub>**" .. string.rep("x", 120) .. "** — completed</sub>", 1, true) ~= nil,
    true
  )
  MiniTest.expect.equality(markdown:find("…", 1, true), nil)
end

T["render"]["keeps the caption on one line when the tool status spans several"] = function()
  local markdown = Transcript.render({
    {
      kind = "tool_call",
      id = "tool-1",
      raw = { toolCallId = "tool-1", title = "Run tests", status = "failed\nhard" },
    },
  }, state())

  MiniTest.expect.equality(markdown:find("<sub>**Run tests** — failed hard</sub>", 1, true) ~= nil, true)
end

T["render_compact"] = MiniTest.new_set()

local compact_entries = {
  { kind = "user", text = "run the tests" },
  { kind = "reasoning", text = "**deduced** the label order" },
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

T["render_compact"]["keeps prose and tool captions, drops reasoning and payloads"] = function()
  local markdown = Transcript.render_compact(compact_entries, state())

  MiniTest.expect.equality(markdown:find("run the tests", 1, true) ~= nil, true)
  MiniTest.expect.equality(markdown:find("All green.", 1, true) ~= nil, true)
  MiniTest.expect.equality(markdown:find("<sub>**Run tests** — completed</sub>", 1, true) ~= nil, true)
  MiniTest.expect.equality(markdown:find("## Reasoning", 1, true), nil)
  MiniTest.expect.equality(markdown:find("**deduced** the label order", 1, true), nil)
  MiniTest.expect.equality(markdown:find("<details>", 1, true), nil)
  MiniTest.expect.equality(markdown:find("make", 1, true), nil)
  MiniTest.expect.equality(count_occurrences(markdown, "line"), 0)

  local user_pos = assert(markdown:find("## User", 1, true))
  local tool_pos = assert(markdown:find("## Tool", 1, true))
  local assistant_pos = assert(markdown:find("## Assistant", 1, true))
  MiniTest.expect.equality(user_pos < tool_pos, true)
  MiniTest.expect.equality(tool_pos < assistant_pos, true)
end

T["render_compact"]["leaves the full renderer untouched for the same snapshot"] = function()
  local markdown = Transcript.render(compact_entries, state())

  MiniTest.expect.equality(markdown:find("## Reasoning", 1, true) ~= nil, true)
  MiniTest.expect.equality(markdown:find("**deduced** the label order", 1, true) ~= nil, true)
  MiniTest.expect.equality(markdown:find("<details>", 1, true) ~= nil, true)
  MiniTest.expect.equality(markdown:find("make", 1, true) ~= nil, true)
  MiniTest.expect.equality(count_occurrences(markdown, "line"), 50)
end

-- A resumed-session user entry carries the adapter's flattened rendering of the
-- injected context: an embedded `[resource] <uri>` marker plus body, a link
-- marker glued directly to the typed text (the adapter concatenates blocks with
-- no separator), then the typed text itself.
T["render_compact"]["strips injected resource blocks from replayed user text"] = function()
  local markdown = Transcript.render_compact({
    {
      kind = "user",
      text = "[resource] <louiselm://skills/index>\n"
        .. "<skills_instructions>\nstep 1\n</skills_instructions>\n</available_skills>"
        .. "[resource] AGENTS.md <file:///home/lotso/code/louiselm/AGENTS.md>commit and sync",
    },
  }, state())

  MiniTest.expect.equality(markdown:find("commit and sync", 1, true) ~= nil, true)
  MiniTest.expect.equality(markdown:find("skills_instructions", 1, true), nil)
  MiniTest.expect.equality(markdown:find("louiselm://skills/index", 1, true), nil)
  MiniTest.expect.equality(markdown:find("AGENTS.md <file://", 1, true), nil)
end

-- A link marker with nothing after it on its own line keeps the text that
-- follows on the next line.
T["render_compact"]["strips a link marker that ends its own line"] = function()
  local markdown = Transcript.render_compact({
    {
      kind = "user",
      text = "[resource] AGENTS.md <file:///home/lotso/code/louiselm/AGENTS.md>\ncommit and sync",
    },
  }, state())

  MiniTest.expect.equality(markdown:find("commit and sync", 1, true) ~= nil, true)
  MiniTest.expect.equality(markdown:find("AGENTS.md <file://", 1, true), nil)
end

-- With no later marker, the boundary between an embedded resource's body and
-- the typed prompt is not representable in the flattened string: drop the
-- marker, keep the body.
T["render_compact"]["keeps the body of an unbounded trailing embedded marker"] = function()
  local markdown = Transcript.render_compact({
    {
      kind = "user",
      text = "[resource] <louiselm://skills/index>\n<skills_instructions>\nstep 1\n</skills_instructions>",
    },
  }, state())

  MiniTest.expect.equality(markdown:find("step 1", 1, true) ~= nil, true)
  MiniTest.expect.equality(markdown:find("louiselm://skills/index", 1, true), nil)
end

T["render_compact"]["preserves the Handoff provenance marker on user entries"] = function()
  local markdown = Transcript.render_compact({
    {
      kind = "user",
      text = "take over this",
      handoff_source_session_id = "claude/161e0a06-dea4-471b-99de-caa1ea641212",
    },
  }, state())

  MiniTest.expect.equality(
    markdown:find("<sub>Handoff from Session: `claude/161e0a06-dea4-471b-99de-caa1ea641212`</sub>", 1, true) ~= nil,
    true
  )
end

T["render_compact"]["is deterministic for the same entries and state"] = function()
  MiniTest.expect.equality(
    Transcript.render_compact(compact_entries, state()),
    Transcript.render_compact(compact_entries, state())
  )
end

return T
