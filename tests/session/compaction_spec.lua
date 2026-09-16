local MiniTest = require("mini.test")
local Compaction = require("louiselm.session.compaction")
local T = MiniTest.new_set()
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim

-- Synthetic contract sequences, not captured runtime ordering. Source:
-- https://agentclientprotocol.com/rfds/session-compaction (2026-09-16).
local function update(status, fields)
  return nvim.tbl_extend("force", {
    sessionUpdate = "compaction_update",
    compactionId = "c1",
    status = status,
  }, fields or {})
end

T["applies streamed content, replacement, omission and clearing without mutating inputs"] = function()
  local started = assert(Compaction.apply(nil, update("in_progress", { _meta = { count = 1 } })))
  local streamed = assert(Compaction.apply(started, {
    sessionUpdate = "compaction_summary_chunk",
    compactionId = "c1",
    content = { type = "text", text = "draft" },
  }))
  MiniTest.expect.equality(started.summary, nil)
  MiniTest.expect.equality(Compaction.text(streamed), "draft")
  local completed =
    assert(Compaction.apply(streamed, update("completed", { summary = { { type = "text", text = "final" } } })))
  MiniTest.expect.equality(Compaction.text(completed), "final")
  local patched = assert(Compaction.apply(completed, update("completed", { _meta = { count = 2 } })))
  MiniTest.expect.equality(Compaction.text(patched), "final")
  MiniTest.expect.equality(patched.meta, { count = 2 })
  for _, clear in ipairs({ nvim.NIL, {} }) do
    local cleared = assert(Compaction.apply(patched, update("completed", { summary = clear, _meta = nvim.NIL })))
    MiniTest.expect.equality(Compaction.text(cleared), nil)
    MiniTest.expect.equality(cleared.meta, nil)
  end
end

T["accepts terminal first and opaque statuses but rejects contradictory or malformed updates"] = function()
  local completed = assert(Compaction.apply(nil, update("completed")))
  local unknown = assert(Compaction.apply(nil, update("_paused")))
  MiniTest.expect.equality(unknown.status, "_paused")
  for _, fields in ipairs({
    { status = "" },
    { compactionId = false },
    { summary = false },
    { summary = nvim.empty_dict() },
    { summary = { { type = "text", text = 12 } } },
    { summary = { [2] = { type = "text", text = "gap" } } },
    { error = "bad" },
    { _meta = false },
    { _meta = { "array" } },
  }) do
    local result, err = Compaction.apply(nil, update("completed", fields))
    MiniTest.expect.equality(result, nil)
    MiniTest.expect.equality(type(err), "string")
  end
  MiniTest.expect.equality(Compaction.apply(completed, update("in_progress")), nil)
  MiniTest.expect.equality(Compaction.apply(completed, update("failed")), nil)
  local chunk =
    { sessionUpdate = "compaction_summary_chunk", compactionId = "c1", content = { type = "text", text = "late" } }
  MiniTest.expect.equality(Compaction.apply(nil, chunk), nil)
  MiniTest.expect.equality(Compaction.apply(completed, chunk), nil)
  MiniTest.expect.equality(Compaction.apply(unknown, chunk), nil)
end

T["keeps opaque content and preserves draft text when terminal updates omit summary"] = function()
  local image = assert(
    Compaction.apply(
      nil,
      update("completed", { summary = { { type = "image", data = "opaque", mimeType = "image/png" } } })
    )
  )
  MiniTest.expect.equality(Compaction.text(image), nil)
  MiniTest.expect.equality(image.summary[1].data, "opaque")
  local state = assert(Compaction.apply(nil, update("in_progress")))
  state = assert(
    Compaction.apply(
      state,
      { sessionUpdate = "compaction_summary_chunk", compactionId = "c1", content = { type = "text", text = "draft" } }
    )
  )
  for _, status in ipairs({ "failed", "cancelled" }) do
    local result = assert(Compaction.apply(state, update(status)))
    MiniTest.expect.equality(Compaction.text(result), "draft")
  end
end

return T
