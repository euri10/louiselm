---@class louiselm.provenance.Node
---@field kind "commit"|"issue"|"session"|"actor"|"decision" Node kind.
---@field id string Stable identifier for the node.

---@class louiselm.provenance.Commit
---@field id string Full commit object id.
---@field message string Commit message, including trailers.

---@class louiselm.provenance.Edge
---@field source louiselm.provenance.Node Node that establishes the relationship.
---@field target louiselm.provenance.Node Node named by the relationship.
---@field relation string Relationship between the nodes.
---@field method "recorded"|"inferred" How the relationship was established.
---@field correlation_method? "explicit_id"|"co_committed" bvr's correlation method.
---@field confidence number Confidence in the relationship, from 0 to 1.

---@class louiselm.provenance.Issue
---@field id string Stable Beads issue identifier.
---@field issue_type string Beads issue type.
---@field title? string Human-readable issue title.
---@field description? string Human-authored issue description.
---@field notes? string Human-authored issue notes.
---@field design? string Human-authored design notes.
---@field acceptance_criteria? string Human-authored acceptance criteria.

---@class louiselm.provenance.DecisionAnchor
---@field id string Beads issue identifier that anchors the Decision.
---@field title? string Human-readable issue title.

---@class louiselm.provenance.DecisionGraph
---@field anchors louiselm.provenance.DecisionAnchor[] Question issues represented as Decisions.
---@field relations louiselm.provenance.Edge[] Explicit relationships between Decisions.
---@field diagnostics louiselm.provenance.Error[] Non-fatal malformed or unresolved relations.

---@class louiselm.provenance.Error
---@field code string Stable machine-readable error code.
---@field message string Human-readable error description.
---@field index? integer Input index associated with the error.

---@class louiselm.provenance.Actor
---@field raw string Actor recorded by Beads.
---@field kind "session"|"reaper"|"non_session" Actor origin.
---@field session_id? string Underlying Agent-scoped Session identity.

local M = {}

---@param code string
---@param message string
---@param index? integer
---@return louiselm.provenance.Error
local function make_error(code, message, index)
  return { code = code, message = message, index = index }
end

---Resolve a Beads actor without erasing whether a daemon performed the action.
---@param actor unknown Recorded actor value.
---@return louiselm.provenance.Actor? resolved
---@return louiselm.provenance.Error? error_value
function M.resolve_actor(actor)
  if type(actor) ~= "string" or actor == "" then
    return nil, make_error("invalid_actor", "actor must be a non-empty string")
  end
  local session_id = actor:match("^reaper/(.+)$")
  if session_id ~= nil and session_id:find("/", 1, true) ~= nil then
    return { raw = actor, kind = "reaper", session_id = session_id }, nil
  end
  if actor:sub(1, #"reaper/") == "reaper/" then
    return nil, make_error("invalid_actor", "reaper actor must identify a Session")
  end
  if actor:find("/", 1, true) ~= nil then
    return { raw = actor, kind = "session", session_id = actor }, nil
  end
  return { raw = actor, kind = "non_session" }, nil
end

---@param value unknown
---@param index integer
---@return louiselm.provenance.Commit? commit
---@return louiselm.provenance.Error? error_value
local function validate_commit(value, index)
  if type(value) ~= "table" then
    return nil, make_error("invalid_commit", "commit must be a table", index)
  end
  if type(value.id) ~= "string" or value.id == "" then
    return nil, make_error("invalid_commit", "commit id must be a non-empty string", index)
  end
  if type(value.message) ~= "string" then
    return nil, make_error("invalid_commit", "commit message must be a string", index)
  end
  return { id = value.id, message = value.message }, nil
end

---@param reference string
---@return "issue"|"session"?
local function reference_kind(reference)
  if reference:find("/", 1, true) ~= nil then
    return "session"
  end
  if reference:match("^[%w][%w%._/-]*$") ~= nil then
    return "issue"
  end
  return nil
end

---@param message string
---@return string[]
local function references(message)
  local result = {}
  local seen = {}
  for line in message:gmatch("[^\r\n]+") do
    local values = line:match("^%s*Refs%s+(.+)%s*$")
    if values ~= nil then
      for reference in values:gmatch("%S+") do
        reference = reference:gsub("[,;]$", "")
        if reference_kind(reference) ~= nil and not seen[reference] then
          seen[reference] = true
          result[#result + 1] = reference
        end
      end
    end
  end
  return result
end

---@param value unknown
---@param index integer
---@return louiselm.provenance.Issue? issue
---@return louiselm.provenance.Error? error_value
local function validate_issue(value, index)
  if type(value) ~= "table" then
    return nil, make_error("invalid_issue", "issue must be a table", index)
  end
  if type(value.id) ~= "string" or value.id == "" then
    return nil, make_error("invalid_issue", "issue id must be a non-empty string", index)
  end
  if type(value.issue_type) ~= "string" or value.issue_type == "" then
    return nil, make_error("invalid_issue", "issue type must be a non-empty string", index)
  end
  local issue = { id = value.id, issue_type = value.issue_type }
  for _, field in ipairs({ "title", "description", "notes", "design", "acceptance_criteria" }) do
    if value[field] ~= nil then
      if type(value[field]) ~= "string" then
        return nil, make_error("invalid_issue", "issue " .. field .. " must be a string", index)
      end
      issue[field] = value[field]
    end
  end
  return issue, nil
end

---@param issue louiselm.provenance.Issue
---@return louiselm.provenance.DecisionAnchor anchor
local function decision_anchor(issue)
  local anchor = { id = issue.id }
  if issue.title ~= nil and issue.title ~= "" then
    anchor.title = issue.title
  end
  return anchor
end

---@param issue louiselm.provenance.Issue
---@return string[] text_fields
local function relation_text(issue)
  local fields = {}
  for _, field in ipairs({ "description", "notes", "design", "acceptance_criteria" }) do
    if issue[field] ~= nil then
      fields[#fields + 1] = issue[field]
    end
  end
  return fields
end

---@param issue louiselm.provenance.Issue
---@param decisions table<string, boolean>
---@param diagnostics louiselm.provenance.Error[]
---@return louiselm.provenance.Edge[] relations
local function decision_relations(issue, decisions, diagnostics)
  local relations = {}
  for _, text in ipairs(relation_text(issue)) do
    for line in text:gmatch("[^\r\n]+") do
      local relation, target_id = line:match("^%s*Decision relation:%s*(%w+)%s*(%S*)%s*$")
      if relation ~= nil then
        if (relation ~= "supersedes" and relation ~= "reconsiders") or target_id == "" then
          diagnostics[#diagnostics + 1] = make_error(
            "malformed_decision_relation",
            "Decision relation must name supersedes or reconsiders and a target issue",
            nil
          )
        elseif not decisions[target_id] then
          diagnostics[#diagnostics + 1] = make_error(
            "unresolved_decision_relation",
            "Decision relation target is not a question issue: " .. target_id,
            nil
          )
        else
          relations[#relations + 1] = {
            source = { kind = "decision", id = issue.id },
            target = { kind = "decision", id = target_id },
            relation = relation,
            method = "recorded",
            confidence = 1,
          }
        end
      end
    end
  end
  return relations
end

---Build explicit Provenance edges from already-collected git commits.
---This function performs no filesystem, process, or editor I/O.
---@param commits louiselm.provenance.Commit[] Collected commits in git order.
---@return louiselm.provenance.Edge[]? edges Edges in commit and trailer order.
---@return louiselm.provenance.Error? error_value Malformed input, if any.
function M.commits(commits)
  if type(commits) ~= "table" then
    return nil, make_error("invalid_commits", "commits must be an array")
  end

  local edges = {}
  for index, value in ipairs(commits) do
    local commit, validation_error = validate_commit(value, index)
    if commit == nil then
      return nil, validation_error
    end
    for _, reference in ipairs(references(commit.message)) do
      edges[#edges + 1] = {
        source = { kind = "commit", id = commit.id },
        target = { kind = reference_kind(reference), id = reference },
        relation = "refs",
        method = "recorded",
        confidence = 1,
      }
    end
  end
  return edges, nil
end

---Build issue-to-commit edges from one bvr history response.
---Explicit-id correlations remain recorded; co-committed correlations remain inferred.
---This function performs no filesystem, process, or editor I/O.
---@param history louiselm.provenance.IssueHistory Parsed bvr history.
---@return louiselm.provenance.Edge[]? edges Edges in bvr commit order.
---@return louiselm.provenance.Error? error_value Malformed input, if any.
function M.issue(history)
  if type(history) ~= "table" or type(history.bead_id) ~= "string" or history.bead_id == "" then
    return nil, make_error("invalid_issue_history", "issue history must identify a Beads issue")
  end
  if type(history.commits) ~= "table" then
    return nil, make_error("invalid_issue_history", "issue history commits must be an array")
  end

  local edges = {}
  local seen_sessions = {}
  for index, commit in ipairs(history.commits) do
    if
      type(commit) ~= "table"
      or type(commit.sha) ~= "string"
      or commit.sha == ""
      or (commit.method ~= "explicit_id" and commit.method ~= "co_committed")
      or type(commit.confidence) ~= "number"
      or commit.confidence < 0
      or commit.confidence > 1
    then
      return nil, make_error("invalid_issue_history", "issue history commit is malformed", index)
    end
    edges[#edges + 1] = {
      source = { kind = "issue", id = history.bead_id },
      target = { kind = "commit", id = commit.sha },
      relation = "implemented_by",
      method = commit.method == "explicit_id" and "recorded" or "inferred",
      correlation_method = commit.method,
      confidence = commit.confidence,
    }
    if commit.message ~= nil then
      if type(commit.message) ~= "string" then
        return nil, make_error("invalid_issue_history", "issue history commit message is malformed", index)
      end
      for _, reference in ipairs(references(commit.message)) do
        if reference_kind(reference) == "session" and not seen_sessions[reference] then
          seen_sessions[reference] = true
          edges[#edges + 1] = {
            source = { kind = "issue", id = history.bead_id },
            target = { kind = "session", id = reference },
            relation = "worked_on",
            method = "recorded",
            confidence = 1,
          }
        end
      end
    end
  end
  return edges, nil
end

---Build issue-to-actor edges from Beads actor fields.
---Session-shaped actors target Sessions; legacy bare names target actors.
---This function performs no filesystem, process, or editor I/O.
---@param issue louiselm.provenance.BeadsIssue Beads issue actor fields.
---@return louiselm.provenance.Edge[]? edges Edges in actor-field order.
---@return louiselm.provenance.Error? error_value Malformed input, if any.
function M.issue_actors(issue)
  if type(issue) ~= "table" or type(issue.id) ~= "string" or issue.id == "" then
    return nil, make_error("invalid_issue_actors", "Beads issue actors must identify an issue")
  end

  local edges = {}
  local seen = {}
  for index, field in ipairs({ "assignee", "created_by" }) do
    local raw_actor = issue[field]
    if raw_actor ~= nil then
      if type(raw_actor) ~= "string" then
        return nil, make_error("invalid_issue_actors", "issue " .. field .. " must be a string", index)
      end
      if raw_actor ~= "" and not seen[raw_actor] then
        local actor, actor_error = M.resolve_actor(raw_actor)
        if actor == nil then
          return nil,
            make_error(
              "invalid_issue_actors",
              actor_error and actor_error.message or "actor could not be resolved",
              index
            )
        end
        seen[raw_actor] = true
        edges[#edges + 1] = {
          source = { kind = "issue", id = issue.id },
          target = {
            kind = actor.kind == "non_session" and "actor" or "session",
            id = actor.session_id or actor.raw,
          },
          relation = "worked_on",
          method = "recorded",
          confidence = 1,
        }
      end
    end
  end
  return edges, nil
end

---Merge issue actor edges without duplicating Session identities already found in trailers.
---@param existing louiselm.provenance.Edge[] Existing issue edges.
---@param additional louiselm.provenance.Edge[] Actor-derived issue edges.
---@return louiselm.provenance.Edge[] edges Merged edges.
function M.merge_issue_sessions(existing, additional)
  local result = {}
  local seen_sessions = {}
  for _, edge in ipairs(existing) do
    result[#result + 1] = edge
    if edge.target.kind == "session" then
      seen_sessions[edge.target.id] = true
    end
  end
  for _, edge in ipairs(additional) do
    if edge.target.kind ~= "session" or not seen_sessions[edge.target.id] then
      result[#result + 1] = edge
      if edge.target.kind == "session" then
        seen_sessions[edge.target.id] = true
      end
    end
  end
  return result
end

---Derive Decision anchors and explicit relations from Beads issues.
---Question issues become anchors; no new persistent entity is created.
---Only explicit `Decision relation:` markers between known question issues
---produce relations. Malformed or unresolved optional markers are diagnostics.
---This function performs no filesystem, process, or editor I/O.
---@param issues louiselm.provenance.Issue[] Beads issues to inspect.
---@return louiselm.provenance.DecisionGraph? graph Derived anchors and relations.
---@return louiselm.provenance.Error? error_value Malformed required input.
function M.decisions(issues)
  if type(issues) ~= "table" then
    return nil, make_error("invalid_issues", "issues must be an array")
  end

  local valid_issues = {}
  local by_id = {}
  for index, value in ipairs(issues) do
    local issue, validation_error = validate_issue(value, index)
    if issue == nil then
      return nil, validation_error
    end
    if by_id[issue.id] then
      return nil, make_error("invalid_issue", "issue ids must be unique", index)
    end
    by_id[issue.id] = issue
    valid_issues[#valid_issues + 1] = issue
  end

  local anchors = {}
  local decisions = {}
  for _, issue in ipairs(valid_issues) do
    if issue.issue_type == "question" then
      anchors[#anchors + 1] = decision_anchor(issue)
      decisions[issue.id] = true
    end
  end

  local relations = {}
  local diagnostics = {}
  for _, issue in ipairs(valid_issues) do
    if decisions[issue.id] then
      local issue_relations = decision_relations(issue, decisions, diagnostics)
      for _, relation in ipairs(issue_relations) do
        relations[#relations + 1] = relation
      end
    end
  end

  return { anchors = anchors, relations = relations, diagnostics = diagnostics }, nil
end

return M
