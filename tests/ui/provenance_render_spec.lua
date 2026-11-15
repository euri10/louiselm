local MiniTest = require("mini.test")
local Render = require("louiselm.ui.provenance.render")

local T = MiniTest.new_set()

T["Decision evidence renders labels, missing paths, relations, and historical errors from data"] = function()
  ---@type louiselm.provenance.Node
  local source = { id = "louiselm-decision", kind = "issue" }
  local lines = Render.decision({
    timeline = {
      anchor = { id = "louiselm-decision", title = "Boundary", state = "accepted" },
      implementation = {
        {
          source = source,
          target = { id = "commit-a", kind = "commit" },
          relation = "implemented_by",
          method = "recorded",
          correlation_method = "explicit_id",
          confidence = 1,
        },
        {
          source = source,
          target = { id = "commit-b", kind = "commit" },
          relation = "implemented_by",
          method = "inferred",
          correlation_method = "co_committed",
          confidence = 0.756,
        },
      },
      sessions = {
        {
          source = source,
          target = { id = "agent/one", kind = "session" },
          relation = "worked_on",
          method = "recorded",
          confidence = 1,
        },
        {
          source = source,
          target = { id = "agent/two", kind = "session" },
          relation = "worked_on",
          method = "recorded",
          confidence = 1,
        },
      },
      qa = {},
      forensics = {},
      gaps = { "old evidence missing" },
    },
    status = "closed",
    transcripts = {
      ["agent/one"] = { path = "/fixture/history.jsonl" },
      ["agent/two"] = { error = "ambiguous transcript" },
    },
    relations = { { relation = "reconsiders", target_id = "louiselm-old" } },
    historical = { ref = "close-sha", error = "snapshot missing" },
  })
  MiniTest.expect.equality(lines, {
    "# Decision Boundary",
    "",
    "ID: louiselm-decision",
    "State: accepted",
    "Status: closed",
    "",
    "Implementation:",
    "- commit-a · recorded · explicit_id · 100% confidence",
    "- commit-b · inferred · co_committed · 76% confidence",
    "",
    "Sessions:",
    "- agent/one → /fixture/history.jsonl · recorded · 100% confidence",
    "- agent/two → unresolved (ambiguous transcript) · recorded · 100% confidence",
    "",
    "QA acceptance:",
    "- unresolved (no attributable acceptance tied to a correlated commit)",
    "",
    "Forensics:",
    "- unresolved (no pointer recorded)",
    "",
    "Unresolved evidence:",
    "- old evidence missing",
    "",
    "Relations:",
    "- reconsiders louiselm-old · recorded",
    "",
    "Historical backlog:",
    "- ref: close-sha",
    "- unavailable: snapshot missing",
  })
end

T["empty commit and Session evidence retains its explicit fallbacks"] = function()
  MiniTest.expect.equality(Render.commit({ commit = { id = "sha", message = "" }, edges = {}, transcripts = {} }), {
    "# (no commit subject)",
    "",
    "Commit: sha",
    "",
    "Issues served:",
    "- none recorded",
    "",
    "Sessions:",
    "- none recorded",
  })
  MiniTest.expect.equality(Render.session({ id = "agent/id", edges = {}, transcript = {} }), {
    "# Session agent/id",
    "",
    "ID: agent/id",
    "",
    "Transcript:",
    "- unresolved (unknown error)",
    "",
    "Commits:",
    "- none recorded",
    "",
    "Issues:",
    "- none recorded",
  })
end

return T
