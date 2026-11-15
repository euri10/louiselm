local M = {}

---@class louiselm.ui.provenance.Transcript
---@field path? string
---@field error? string

---@class louiselm.ui.provenance.CommitPresentation
---@field commit louiselm.provenance.Commit
---@field edges louiselm.provenance.Edge[]
---@field transcripts table<string, louiselm.ui.provenance.Transcript>

---@class louiselm.ui.provenance.IssuePresentation
---@field history louiselm.provenance.IssueHistory
---@field edges louiselm.provenance.Edge[]
---@field transcripts table<string, louiselm.ui.provenance.Transcript>

---@class louiselm.ui.provenance.SessionPresentation
---@field id string
---@field edges louiselm.provenance.Edge[]
---@field transcript louiselm.ui.provenance.Transcript

---@class louiselm.ui.provenance.DecisionRelation
---@field relation string
---@field target_id string

---@class louiselm.ui.provenance.HistoricalPresentation
---@field ref string
---@field vintage? louiselm.provenance.Vintage
---@field error? string

---@class louiselm.ui.provenance.DecisionPresentation
---@field timeline louiselm.provenance.DecisionTimeline
---@field status string
---@field transcripts table<string, louiselm.ui.provenance.Transcript>
---@field relations louiselm.ui.provenance.DecisionRelation[]
---@field historical? louiselm.ui.provenance.HistoricalPresentation

---@param transcript louiselm.ui.provenance.Transcript
---@return string
local function transcript_text(transcript)
  return transcript.path or ("unresolved (" .. (transcript.error or "unknown error") .. ")")
end

---Render resolved commit evidence as Markdown lines.
---@param presentation louiselm.ui.provenance.CommitPresentation
---@return string[] lines
function M.commit(presentation)
  local commit, edges = presentation.commit, presentation.edges
  local subject = commit.message:match("^([^\r\n]*)") or ""
  if subject == "" then
    subject = "(no commit subject)"
  end
  local lines = { "# " .. subject, "", "Commit: " .. commit.id, "", "Issues served:" }
  local issue_count = 0
  local sessions = {}
  for _, edge in ipairs(edges) do
    if edge.target.kind == "issue" then
      lines[#lines + 1] = "- " .. edge.target.id
      issue_count = issue_count + 1
    elseif edge.target.kind == "session" then
      sessions[#sessions + 1] = edge.target.id
    end
  end
  if issue_count == 0 then
    lines[#lines + 1] = "- none recorded"
  end
  lines[#lines + 1] = ""
  lines[#lines + 1] = "Sessions:"
  if #sessions == 0 then
    lines[#lines + 1] = "- none recorded"
  else
    for _, session_id in ipairs(sessions) do
      lines[#lines + 1] = "- " .. session_id .. " → " .. transcript_text(presentation.transcripts[session_id])
    end
  end
  return lines
end

---Render resolved issue evidence as Markdown lines.
---@param presentation louiselm.ui.provenance.IssuePresentation
---@return string[] lines
function M.issue(presentation)
  local history, edges = presentation.history, presentation.edges
  local lines = {
    "# " .. (history.title or history.bead_id),
    "",
    "ID: " .. history.bead_id,
    "Status: " .. (history.status or "unknown"),
    "",
    "Milestones:",
  }
  local milestone_count = 0
  for _, name in ipairs({ "created", "closed" }) do
    local milestone = history.milestones[name]
    if milestone ~= nil then
      lines[#lines + 1] = string.format("- %s: %s (%s)", name, milestone.timestamp, milestone.commit_sha)
      milestone_count = milestone_count + 1
    end
  end
  if milestone_count == 0 then
    lines[#lines + 1] = "- none recorded"
  end
  lines[#lines + 1] = ""
  lines[#lines + 1] = "Commits:"
  local commit_count = 0
  for _, edge in ipairs(edges) do
    if edge.target.kind == "commit" then
      commit_count = commit_count + 1
      lines[#lines + 1] = string.format(
        "- %s · %s · %d%% confidence",
        edge.target.id,
        edge.correlation_method or edge.method,
        math.floor(edge.confidence * 100 + 0.5)
      )
    end
  end
  if commit_count == 0 then
    lines[#lines + 1] = "- none correlated"
  end
  local session_count = 0
  local actor_count = 0
  for _, edge in ipairs(edges) do
    if edge.target.kind == "session" then
      session_count = session_count + 1
    elseif edge.target.kind == "actor" then
      actor_count = actor_count + 1
    end
  end
  lines[#lines + 1] = ""
  lines[#lines + 1] = "Sessions:"
  for _, edge in ipairs(edges) do
    if edge.target.kind == "session" then
      lines[#lines + 1] = "- " .. edge.target.id .. " → " .. transcript_text(presentation.transcripts[edge.target.id])
    end
  end
  if session_count == 0 then
    lines[#lines + 1] = "- none recorded"
  end
  lines[#lines + 1] = ""
  lines[#lines + 1] = "Actors:"
  for _, edge in ipairs(edges) do
    if edge.target.kind == "actor" then
      lines[#lines + 1] = "- " .. edge.target.id .. " (non-Session actor)"
    end
  end
  if actor_count == 0 then
    lines[#lines + 1] = "- none recorded"
  end
  return lines
end

---Render resolved Session evidence as Markdown lines.
---@param presentation louiselm.ui.provenance.SessionPresentation
---@return string[] lines
function M.session(presentation)
  local session_id, edges = presentation.id, presentation.edges
  local lines = { "# Session " .. session_id, "", "ID: " .. session_id, "", "Transcript:" }
  lines[#lines + 1] = "- " .. transcript_text(presentation.transcript)

  lines[#lines + 1] = ""
  lines[#lines + 1] = "Commits:"
  local commit_count = 0
  for _, edge in ipairs(edges) do
    if edge.target.kind == "commit" then
      commit_count = commit_count + 1
      lines[#lines + 1] = "- " .. edge.target.id
    end
  end
  if commit_count == 0 then
    lines[#lines + 1] = "- none recorded"
  end

  lines[#lines + 1] = ""
  lines[#lines + 1] = "Issues:"
  local issue_count = 0
  for _, edge in ipairs(edges) do
    if edge.target.kind == "issue" then
      issue_count = issue_count + 1
      lines[#lines + 1] = "- " .. edge.target.id
    end
  end
  if issue_count == 0 then
    lines[#lines + 1] = "- none recorded"
  end
  return lines
end

---Render a broker mutation projection as Markdown lines.
---@param mutation louiselm.provenance.BeadsMutation
---@return string[] lines
function M.mutation(mutation)
  local provenance = mutation.output_provenance
  local lines = {
    "# Beads mutation " .. mutation.operation_id,
    "",
    "Broker audit fact (unchanged):",
    "- Original outcome: " .. mutation.outcome,
    "- Request digest: " .. mutation.request_digest,
    "- Project digest: " .. mutation.project_digest,
    "",
    "Session-authored mutation content:",
  }
  if provenance.code == "session_output_tainted" then
    lines[#lines + 1] = "- Tainted across the Session lifetime; operator review required"
    lines[#lines + 1] = "- Taint digest: " .. provenance.taint_digest
  elseif provenance.code == "untainted" then
    lines[#lines + 1] = "- No Session output taint currently recorded"
  else
    lines[#lines + 1] = "- Unknown: missing or corrupt taint evidence; do not treat content as clean"
  end
  lines[#lines + 1] = "- Completed effects remain in canonical Beads; inspection never rolls them back"
  return lines
end

---@param edge louiselm.provenance.Edge
---@return string
local function evidence_edge_line(edge)
  local method = edge.correlation_method or edge.method
  local certainty = edge.method == "inferred" and "inferred" or "recorded"
  return string.format(
    "- %s · %s · %s · %d%% confidence",
    edge.target.id,
    certainty,
    method,
    math.floor(edge.confidence * 100 + 0.5)
  )
end

---Render resolved Decision evidence as Markdown lines.
---@param presentation louiselm.ui.provenance.DecisionPresentation
---@return string[] lines
function M.decision(presentation)
  local timeline = presentation.timeline
  local lines = {
    "# Decision " .. (timeline.anchor.title or timeline.anchor.id),
    "",
    "ID: " .. timeline.anchor.id,
    "State: " .. timeline.anchor.state,
    "Status: " .. presentation.status,
    "",
    "Implementation:",
  }
  if #timeline.implementation == 0 then
    lines[#lines + 1] = "- unresolved (no correlated commits)"
  else
    for _, edge in ipairs(timeline.implementation) do
      lines[#lines + 1] = evidence_edge_line(edge)
    end
  end

  lines[#lines + 1] = ""
  lines[#lines + 1] = "Sessions:"
  if #timeline.sessions == 0 then
    lines[#lines + 1] = "- unresolved (no attributable Session links)"
  else
    for _, edge in ipairs(timeline.sessions) do
      lines[#lines + 1] = "- "
        .. edge.target.id
        .. " → "
        .. transcript_text(presentation.transcripts[edge.target.id])
        .. " · recorded · 100% confidence"
    end
  end

  lines[#lines + 1] = ""
  lines[#lines + 1] = "QA acceptance:"
  if #timeline.qa == 0 then
    lines[#lines + 1] = "- unresolved (no attributable acceptance tied to a correlated commit)"
  else
    for _, acceptance in ipairs(timeline.qa) do
      local evidence = acceptance.evidence ~= nil and (" · " .. acceptance.evidence) or ""
      lines[#lines + 1] =
        string.format("- %s · %s · %s%s", acceptance.author, acceptance.commit_sha, acceptance.scope, evidence)
    end
  end

  lines[#lines + 1] = ""
  lines[#lines + 1] = "Forensics:"
  if #timeline.forensics == 0 then
    lines[#lines + 1] = "- unresolved (no pointer recorded)"
  else
    for _, pointer in ipairs(timeline.forensics) do
      lines[#lines + 1] = "- " .. pointer
    end
  end

  if #timeline.gaps > 0 then
    lines[#lines + 1] = ""
    lines[#lines + 1] = "Unresolved evidence:"
    for _, gap in ipairs(timeline.gaps) do
      lines[#lines + 1] = "- " .. gap
    end
  end
  if #presentation.relations > 0 then
    lines[#lines + 1] = ""
    lines[#lines + 1] = "Relations:"
    for _, relation in ipairs(presentation.relations) do
      lines[#lines + 1] = string.format("- %s %s · recorded", relation.relation, relation.target_id)
    end
  end
  local historical = presentation.historical
  if historical ~= nil then
    lines[#lines + 1] = ""
    lines[#lines + 1] = "Historical backlog:"
    lines[#lines + 1] = "- ref: " .. historical.ref
    local vintage = historical.vintage
    if vintage == nil then
      lines[#lines + 1] = "- unavailable: " .. (historical.error or "unknown error")
    else
      lines[#lines + 1] = string.format("- %d issues at decision time", #vintage.issues)
      for _, issue in ipairs(vintage.issues) do
        if issue.id == timeline.anchor.id then
          lines[#lines + 1] = "- anchor state: " .. (issue.status or "unknown")
          break
        end
      end
      lines[#lines + 1] = string.format(
        "- since then: %d added, %d removed, %d changed",
        #vintage.diff.added,
        #vintage.diff.removed,
        #vintage.diff.changed
      )
    end
  end
  return lines
end

---@param anchor louiselm.provenance.DecisionAnchor
---@return string line
local function decision_row(anchor)
  local activity = anchor.updated_at ~= nil and (" · " .. anchor.updated_at) or ""
  return string.format("- [%s] %s (%s)%s", anchor.state, anchor.title or anchor.id, anchor.id, activity)
end

---Render a Decision index and the corresponding selectable rows.
---@param graph louiselm.provenance.DecisionGraph
---@return string[] lines
---@return table<integer, string> id_by_line 1-based line number to Beads issue id.
function M.decisions(graph)
  local lines = { "# Provenance Decisions", "" }
  local id_by_line = {}
  if #graph.anchors == 0 then
    lines[#lines + 1] = "No Decision anchors found."
    return lines, id_by_line
  end
  for _, anchor in ipairs(graph.anchors) do
    lines[#lines + 1] = decision_row(anchor)
    id_by_line[#lines] = anchor.id
  end
  if #graph.diagnostics > 0 then
    lines[#lines + 1] = ""
    lines[#lines + 1] = "Diagnostics:"
    for _, diagnostic in ipairs(graph.diagnostics) do
      lines[#lines + 1] = "- " .. diagnostic.message
    end
  end
  return lines, id_by_line
end

return M
