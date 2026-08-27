local MiniTest = require("mini.test")

local Correlate = require("louiselm.provenance.correlate")
local Sources = require("louiselm.provenance.sources")

local T = MiniTest.new_set()
local separator = string.char(30)
local nul = string.char(0)

-- Captured from `git log -n 4 --format='%H%x00%B%x00%x1e'` in this
-- repository during Session codex/01a03df9-6992-71e2-9050-1d4dd466011c.
-- The final record is a pre-trailer commit from the observed history.
local captured_log = table.concat({
  "32665fd3a1c99265496526a9a3116d0d8320182"
    .. nul
    .. "fix(session): guard unsafe editor exits\n\nRefs louiselm-ce6.34\n"
    .. nul,
  "5be054d50e2b48d231670933d896460e7d262b7a"
    .. nul
    .. "feat(session): resolve transcript paths\n\nRefs louiselm-dpik\nRefs codex/01a03df9-6992-71e2-9050-1d4dd466011c\n"
    .. nul,
  "0bcb2890022cf558a57c6f00257bd4f51e1b06ae" .. nul .. "chore(beads): plan diagnostics implementation\n" .. nul,
}, separator) .. separator

T["captured git log yields explicit issue and Session edges"] = function()
  local commits, parse_error = Sources.parse_git_log(captured_log)
  assert(parse_error == nil)
  assert(commits ~= nil)
  local edges, correlate_error = Correlate.commits(commits)
  assert(correlate_error == nil)
  assert(edges ~= nil)

  MiniTest.expect.equality(#edges, 3)
  MiniTest.expect.equality(edges[1], {
    source = { kind = "commit", id = "32665fd3a1c99265496526a9a3116d0d8320182" },
    target = { kind = "issue", id = "louiselm-ce6.34" },
    relation = "refs",
    method = "recorded",
    confidence = 1,
  })
  MiniTest.expect.equality(edges[2], {
    source = { kind = "commit", id = "5be054d50e2b48d231670933d896460e7d262b7a" },
    target = { kind = "issue", id = "louiselm-dpik" },
    relation = "refs",
    method = "recorded",
    confidence = 1,
  })
  MiniTest.expect.equality(edges[3], {
    source = { kind = "commit", id = "5be054d50e2b48d231670933d896460e7d262b7a" },
    target = { kind = "session", id = "codex/01a03df9-6992-71e2-9050-1d4dd466011c" },
    relation = "refs",
    method = "recorded",
    confidence = 1,
  })
end

T["bvr history yields issue-to-commit edges with method and confidence"] = function()
  local edges, error_value = Correlate.issue({
    bead_id = "louiselm-kpod",
    milestones = {},
    commits = {
      { sha = "explicit-sha", method = "explicit_id", confidence = 0.85 },
      { sha = "inferred-sha", method = "co_committed", confidence = 0.95 },
    },
  })
  assert(error_value == nil)
  assert(edges ~= nil)
  MiniTest.expect.equality(edges, {
    {
      source = { kind = "issue", id = "louiselm-kpod" },
      target = { kind = "commit", id = "explicit-sha" },
      relation = "implemented_by",
      method = "recorded",
      correlation_method = "explicit_id",
      confidence = 0.85,
    },
    {
      source = { kind = "issue", id = "louiselm-kpod" },
      target = { kind = "commit", id = "inferred-sha" },
      relation = "implemented_by",
      method = "inferred",
      correlation_method = "co_committed",
      confidence = 0.95,
    },
  })
end

T["issue actor edges classify and deduplicate Session identities"] = function()
  local edges, error_value = Correlate.issue_actors({
    id = "louiselm-kpod",
    assignee = "codex/session-1",
    created_by = "assistant",
  })
  assert(error_value == nil)
  assert(edges ~= nil)
  MiniTest.expect.equality(edges, {
    {
      source = { kind = "issue", id = "louiselm-kpod" },
      target = { kind = "session", id = "codex/session-1" },
      relation = "worked_on",
      method = "recorded",
      confidence = 1,
    },
    {
      source = { kind = "issue", id = "louiselm-kpod" },
      target = { kind = "actor", id = "assistant" },
      relation = "worked_on",
      method = "recorded",
      confidence = 1,
    },
  })

  local merged = Correlate.merge_issue_sessions({
    {
      source = { kind = "issue", id = "louiselm-kpod" },
      target = { kind = "session", id = "codex/session-1" },
      relation = "worked_on",
      method = "recorded",
      confidence = 1,
    },
  }, edges)
  MiniTest.expect.equality(#merged, 2)
  MiniTest.expect.equality(merged[1].target, { kind = "session", id = "codex/session-1" })
  MiniTest.expect.equality(merged[2].target, { kind = "actor", id = "assistant" })
end

T["issue history trailer edges are available for actor deduplication"] = function()
  local edges, error_value = Correlate.issue({
    bead_id = "louiselm-kpod",
    milestones = {},
    commits = {
      {
        sha = "commit-sha",
        method = "explicit_id",
        confidence = 1,
        message = "fix: work\n\nRefs codex/session-1\n",
      },
    },
  })
  assert(error_value == nil)
  assert(edges ~= nil)
  MiniTest.expect.equality(edges[2], {
    source = { kind = "issue", id = "louiselm-kpod" },
    target = { kind = "session", id = "codex/session-1" },
    relation = "worked_on",
    method = "recorded",
    confidence = 1,
  })
end

T["stale references remain data rather than becoming errors"] = function()
  local edges, error_value = Correlate.commits({
    { id = "deadbeef", message = "chore: old\n\nRefs louiselm-no-longer-exists\n" },
  })
  assert(error_value == nil)
  assert(edges ~= nil)
  MiniTest.expect.equality(edges[1].target, { kind = "issue", id = "louiselm-no-longer-exists" })
end

T["resolves reaper actors without disguising the daemon"] = function()
  local actor, error_value = Correlate.resolve_actor("reaper/codex/acp-session")
  assert(error_value == nil)
  MiniTest.expect.equality(actor, {
    raw = "reaper/codex/acp-session",
    kind = "reaper",
    session_id = "codex/acp-session",
  })
end

T["rejects actors without a Session identity"] = function()
  local actor, error_value = Correlate.resolve_actor("reaper/daemon")
  MiniTest.expect.equality(actor, nil)
  assert(error_value ~= nil)
  MiniTest.expect.equality(error_value.code, "invalid_actor")
end

T["invalid collected input returns a structured error"] = function()
  local malformed = {}
  rawset(malformed, "id", "commit")
  rawset(malformed, "message", false)
  local edges, error_value = Correlate.commits({ malformed })
  MiniTest.expect.equality(edges, nil)
  assert(error_value ~= nil)
  MiniTest.expect.equality(error_value.code, "invalid_commit")
  MiniTest.expect.equality(error_value.index, 1)
end

T["derives Decision anchors and recorded relations from question issues"] = function()
  local graph, error_value = Correlate.decisions({
    {
      id = "louiselm-old",
      issue_type = "question",
      title = "Old decision",
    },
    {
      id = "louiselm-new",
      issue_type = "question",
      title = "New decision",
      description = "Decision relation: supersedes louiselm-old",
    },
    { id = "louiselm-task", issue_type = "task", title = "Unrelated task" },
  })

  assert(error_value == nil)
  assert(graph ~= nil)
  MiniTest.expect.equality(graph.anchors, {
    { id = "louiselm-old", title = "Old decision" },
    { id = "louiselm-new", title = "New decision" },
  })
  MiniTest.expect.equality(graph.relations, {
    {
      source = { kind = "decision", id = "louiselm-new" },
      target = { kind = "decision", id = "louiselm-old" },
      relation = "supersedes",
      method = "recorded",
      confidence = 1,
    },
  })
end

T["malformed and unknown optional relations do not invent edges"] = function()
  local graph, error_value = Correlate.decisions({
    { id = "louiselm-known", issue_type = "question", title = "Known" },
    {
      id = "louiselm-current",
      issue_type = "question",
      description = table.concat({
        "Decision relation: supersedes",
        "Decision relation: reconsiders missing-id",
        "This mentions louiselm-known but is not a relation.",
      }, "\n"),
    },
  })

  assert(error_value == nil)
  assert(graph ~= nil)
  MiniTest.expect.equality(#graph.relations, 0)
  MiniTest.expect.equality(#graph.diagnostics, 2)
end

T["decision relations do not infer links from shared text"] = function()
  local graph, error_value = Correlate.decisions({
    { id = "louiselm-a", issue_type = "question", title = "Same topic" },
    { id = "louiselm-b", issue_type = "question", title = "Same topic" },
  })

  assert(error_value == nil)
  assert(graph ~= nil)
  MiniTest.expect.equality(#graph.relations, 0)
end

return T
