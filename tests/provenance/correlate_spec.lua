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

T["Session edges reverse commits and issue actors"] = function()
  local edges, error_value = Correlate.session("codex/session-1", {
    { id = "commit-sha", message = "feat: work\n\nRefs codex/session-1\n" },
    { id = "other-sha", message = "chore: other\n" },
  }, {
    { id = "louiselm-one", assignee = "codex/session-1", created_by = "assistant" },
    { id = "louiselm-two", assignee = "", created_by = "lotso" },
  })
  assert(error_value == nil)
  assert(edges ~= nil)
  MiniTest.expect.equality(edges, {
    {
      source = { kind = "session", id = "codex/session-1" },
      target = { kind = "commit", id = "commit-sha" },
      relation = "produced",
      method = "recorded",
      confidence = 1,
    },
    {
      source = { kind = "session", id = "codex/session-1" },
      target = { kind = "issue", id = "louiselm-one" },
      relation = "worked_on",
      method = "recorded",
      confidence = 1,
    },
  })
end

T["Session with no recorded work is empty"] = function()
  local edges, error_value = Correlate.session("codex/session-1", {}, {})
  assert(error_value == nil)
  MiniTest.expect.equality(edges, {})
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
      status = "closed",
      close_reason = "Design resolved",
      updated_at = "2026-08-20T00:00:00Z",
    },
    {
      id = "louiselm-new",
      issue_type = "question",
      title = "New decision",
      description = "Decision relation: supersedes louiselm-old",
      status = "closed",
      close_reason = "Superseding the old decision",
      updated_at = "2026-08-26T00:00:00Z",
    },
    { id = "louiselm-task", issue_type = "task", title = "Unrelated task" },
  })

  assert(error_value == nil)
  assert(graph ~= nil)
  MiniTest.expect.equality(graph.anchors, {
    { id = "louiselm-new", title = "New decision", state = "accepted", updated_at = "2026-08-26T00:00:00Z" },
    { id = "louiselm-old", title = "Old decision", state = "superseded", updated_at = "2026-08-20T00:00:00Z" },
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

T["Decision anchors are sorted by most recent activity first"] = function()
  local graph = assert(Correlate.decisions({
    { id = "louiselm-a", issue_type = "question", status = "open", updated_at = "2026-08-20T00:00:00Z" },
    { id = "louiselm-b", issue_type = "question", status = "open", updated_at = "2026-08-26T00:00:00Z" },
    { id = "louiselm-c", issue_type = "question", status = "open", updated_at = "2026-08-23T00:00:00Z" },
  }))

  MiniTest.expect.equality(
    { graph.anchors[1].id, graph.anchors[2].id, graph.anchors[3].id },
    { "louiselm-b", "louiselm-c", "louiselm-a" }
  )
end

T["Decision state distinguishes open, accepted, superseded, and unresolved"] = function()
  local graph = assert(Correlate.decisions({
    { id = "louiselm-open", issue_type = "question", status = "open" },
    { id = "louiselm-accepted", issue_type = "question", status = "closed", close_reason = "Resolved by grill" },
    {
      id = "louiselm-superseded",
      issue_type = "question",
      status = "closed",
      close_reason = "Resolved, later revisited",
    },
    {
      id = "louiselm-superseding",
      issue_type = "question",
      status = "closed",
      close_reason = "Supersedes the prior answer",
      description = "Decision relation: supersedes louiselm-superseded",
    },
    { id = "louiselm-unresolved", issue_type = "question", status = "closed" },
  }))

  local states = {}
  for _, anchor in ipairs(graph.anchors) do
    states[anchor.id] = anchor.state
  end
  MiniTest.expect.equality(states, {
    ["louiselm-open"] = "open",
    ["louiselm-accepted"] = "accepted",
    ["louiselm-superseded"] = "superseded",
    ["louiselm-superseding"] = "accepted",
    ["louiselm-unresolved"] = "unresolved",
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
