local MiniTest = require("mini.test")
local Record = require("louiselm.forensics.record")
local View = require("louiselm.forensics.view")

local T = MiniTest.new_set()

---@return louiselm.forensics.Record
local function record()
  return {
    schema_version = 1,
    id = "record-1",
    observed_at = 1787715628,
    subject = { agent = "codex", acp_session_id = "acp-1" },
    diagnosing_session = "claude/acp-diagnoser",
    observations = {
      agent = "codex",
      cwd = "/tmp/project",
      git_branch = "main",
      dirty_files = { "a.lua", "b.lua" },
      capabilities = { load_session = true, embedded_context = false },
      options = { model = "gpt-5" },
    },
    evidence_sources = {
      { kind = "acp_log", state = "present", path = "/tmp/session.log", mutable = true },
      { kind = "git", state = "unsupported", mutable = true, reason = "Git status is unavailable" },
    },
  }
end

---@param lines string[]
---@param needle string
---@return boolean
local function has_line(lines, needle)
  for _, line in ipairs(lines) do
    if line:find(needle, 1, true) ~= nil then
      return true
    end
  end
  return false
end

T["names both Sessions without confusing them"] = function()
  local lines = View.lines(Record.with_availability(record(), { "available", "missing" }))

  MiniTest.expect.equality(has_line(lines, "Subject Session:    codex/acp-1"), true)
  MiniTest.expect.equality(has_line(lines, "Diagnosing Session: claude/acp-diagnoser"), true)
end

T["states the observation time in UTC"] = function()
  local lines = View.lines(Record.with_availability(record(), { "available", "missing" }))

  MiniTest.expect.equality(has_line(lines, "Observed at:        2026-08-26T03:40:28Z"), true)
end

T["says when no diagnosing Session collected the record"] = function()
  local without = record()
  without.diagnosing_session = nil

  local lines = View.lines(Record.with_availability(without, { "available", "missing" }))

  MiniTest.expect.equality(has_line(lines, "Diagnosing Session: none recorded"), true)
end

T["reports each source state, mutability, sensitivity and pointer"] = function()
  local lines = View.lines(Record.with_availability(record(), { "available", "missing" }))

  MiniTest.expect.equality(has_line(lines, "1. acp_log — present, changes after observation"), true)
  MiniTest.expect.equality(has_line(lines, "keeps conversation content, wire ordering"), true)
  MiniTest.expect.equality(has_line(lines, "read it at: /tmp/session.log"), true)
end

T["explains an omitted source instead of hiding it"] = function()
  local lines = View.lines(Record.with_availability(record(), { "available", "missing" }))

  MiniTest.expect.equality(has_line(lines, "2. git — unsupported, changes after observation"), true)
  MiniTest.expect.equality(has_line(lines, "why: Git status is unavailable"), true)
end

T["separates what the record captured from what is readable now"] = function()
  local lines = View.lines(Record.with_availability(record(), { "available", "missing" }))

  MiniTest.expect.equality(has_line(lines, "conversation_content: available"), true)
  MiniTest.expect.equality(has_line(lines, "repository_state: missing"), true)
end

T["shows bounded observations without raw source contents"] = function()
  local lines = View.lines(Record.with_availability(record(), { "available", "missing" }))

  MiniTest.expect.equality(has_line(lines, "cwd: /tmp/project"), true)
  MiniTest.expect.equality(has_line(lines, "git_branch: main"), true)
  MiniTest.expect.equality(has_line(lines, "dirty_files: a.lua, b.lua"), true)
  MiniTest.expect.equality(has_line(lines, "capabilities: embedded_context=false, load_session=true"), true)
end

T["renders a record that carries no evidence sources at all"] = function()
  local empty = record()
  empty.evidence_sources = {}
  empty.observations = {}

  local lines = View.lines(Record.with_availability(empty, {}))

  MiniTest.expect.equality(has_line(lines, "no evidence sources were recorded"), true)
  MiniTest.expect.equality(has_line(lines, "no observations were recorded"), true)
end

T["refuses a value that is not an inspection"] = function()
  MiniTest.expect.error(function()
    ---@diagnostic disable-next-line: param-type-mismatch -- Proves the guard rejects a value that is not an inspection.
    View.lines("not a record")
  end)
end

return T
