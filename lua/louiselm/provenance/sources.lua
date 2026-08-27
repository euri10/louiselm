---@class louiselm.provenance.SourceError: louiselm.provenance.Error
---@field exit_code? integer Git exit code, when available.
---@field detail? string Bounded stderr detail, when available.

---@class louiselm.provenance.HistoryMilestone
---@field timestamp string
---@field commit_sha string

---@class louiselm.provenance.HistoryCommit
---@field sha string Full commit object id.
---@field method "explicit_id"|"co_committed" How bvr correlated the commit.
---@field confidence number bvr's correlation confidence, from 0 to 1.
---@field message? string Commit message, when returned by bvr.

---@class louiselm.provenance.IssueHistory
---@field bead_id string Beads issue identifier.
---@field title? string Issue title, when the history is in range.
---@field status? string Issue status, when the history is in range.
---@field milestones table<string, louiselm.provenance.HistoryMilestone>
---@field commits louiselm.provenance.HistoryCommit[]

---@class louiselm.provenance.BeadsIssue
---@field id string Beads issue identifier.
---@field assignee? string Actor assigned to the issue, when present.
---@field created_by? string Actor that created the issue, when present.

local M = {}

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local RECORD_SEPARATOR = string.char(30)
local FIELD_SEPARATOR = string.char(0)
local LOG_FORMAT = "%H%x00%B%x00%x1e"
local HISTORY_SINCE = "2026-08-10"
local HISTORY_LIMIT = "500"

---@param code string
---@param message string
---@param detail? string
---@param exit_code? integer
---@return louiselm.provenance.SourceError
local function make_error(code, message, detail, exit_code)
  return { code = code, message = message, detail = detail, exit_code = exit_code }
end

---@param output string
---@return louiselm.provenance.Commit[]? commits
---@return louiselm.provenance.SourceError? error_value
function M.parse_git_log(output)
  if type(output) ~= "string" then
    return nil, make_error("invalid_git_log", "git log output must be a string")
  end

  local commits = {}
  local cursor = 1
  while cursor <= #output do
    local separator = output:find(RECORD_SEPARATOR, cursor, true)
    if separator == nil then
      return nil, make_error("invalid_git_log", "git log output has an incomplete record")
    end
    local record = output:sub(cursor, separator - 1)
    cursor = separator + 1
    if record ~= "" then
      local field_separator = record:find(FIELD_SEPARATOR, 1, true)
      if field_separator == nil then
        return nil, make_error("invalid_git_log", "git log record has no message field", nil, nil)
      end
      local id = record:sub(1, field_separator - 1)
      local message = record:sub(field_separator + 1)
      if message:sub(-1) == FIELD_SEPARATOR then
        message = message:sub(1, -2)
      end
      if id == "" or id:find(FIELD_SEPARATOR, 1, true) ~= nil then
        return nil, make_error("invalid_git_log", "git log record has an invalid commit id", nil, nil)
      end
      commits[#commits + 1] = { id = id, message = message }
    end
  end
  return commits, nil
end

---@param value unknown
---@return louiselm.provenance.HistoryMilestone? milestone
local function parse_milestone(value)
  if type(value) ~= "table" or type(value.timestamp) ~= "string" or type(value.commit_sha) ~= "string" then
    return nil
  end
  return { timestamp = value.timestamp, commit_sha = value.commit_sha }
end

---@param value unknown
---@return louiselm.provenance.HistoryCommit? commit
local function parse_history_commit(value)
  if
    type(value) ~= "table"
    or type(value.sha) ~= "string"
    or value.sha == ""
    or (value.method ~= "explicit_id" and value.method ~= "co_committed")
    or type(value.confidence) ~= "number"
    or value.confidence < 0
    or value.confidence > 1
  then
    return nil
  end
  local commit = { sha = value.sha, method = value.method, confidence = value.confidence }
  if value.message ~= nil then
    if type(value.message) ~= "string" then
      return nil
    end
    commit.message = value.message
  end
  return commit
end

---Parse one issue's observed bvr history response.
---@param output string JSON emitted by `bvr --robot-history`.
---@param bead_id string Requested Beads issue identifier.
---@return louiselm.provenance.IssueHistory? history
---@return louiselm.provenance.SourceError? error_value
function M.parse_bvr_history(output, bead_id)
  if type(output) ~= "string" then
    return nil, make_error("invalid_bvr_history", "bvr history output must be a string")
  end
  if type(bead_id) ~= "string" or bead_id == "" then
    return nil, make_error("invalid_bead_id", "bvr history requires a non-empty Beads issue id")
  end

  local decoded_ok, decoded = pcall(nvim.json.decode, output)
  if not decoded_ok or type(decoded) ~= "table" or type(decoded.histories) ~= "table" then
    return nil, make_error("invalid_bvr_history", "bvr returned malformed history data")
  end

  local raw_history = decoded.histories[bead_id]
  if raw_history == nil then
    return { bead_id = bead_id, commits = {}, milestones = {} }, nil
  end
  if type(raw_history) ~= "table" then
    return nil, make_error("invalid_bvr_history", "bvr returned malformed issue history")
  end

  local history = { bead_id = bead_id, commits = {}, milestones = {} }
  if raw_history.title ~= nil then
    if type(raw_history.title) ~= "string" then
      return nil, make_error("invalid_bvr_history", "bvr issue title must be a string")
    end
    history.title = raw_history.title
  end
  if raw_history.status ~= nil then
    if type(raw_history.status) ~= "string" then
      return nil, make_error("invalid_bvr_history", "bvr issue status must be a string")
    end
    history.status = raw_history.status
  end

  if raw_history.milestones ~= nil then
    if type(raw_history.milestones) ~= "table" then
      return nil, make_error("invalid_bvr_history", "bvr issue milestones must be an object")
    end
    for name, value in pairs(raw_history.milestones) do
      if name == "created" or name == "closed" then
        local milestone = parse_milestone(value)
        if milestone == nil then
          return nil, make_error("invalid_bvr_history", "bvr issue milestone is malformed")
        end
        history.milestones[name] = milestone
      end
    end
  end

  if raw_history.commits ~= nil then
    if type(raw_history.commits) ~= "table" then
      return nil, make_error("invalid_bvr_history", "bvr issue commits must be an array")
    end
    for index, value in ipairs(raw_history.commits) do
      local commit = parse_history_commit(value)
      if commit == nil then
        return nil, make_error("invalid_bvr_history", "bvr issue commit is malformed")
      end
      history.commits[#history.commits + 1] = commit
    end
  end
  return history, nil
end

---@param raw_issue unknown Raw issue value from Beads JSON.
---@return louiselm.provenance.BeadsIssue? issue
---@return louiselm.provenance.SourceError? error_value
local function parse_beads_issue_fields(raw_issue)
  if type(raw_issue) ~= "table" or type(raw_issue.id) ~= "string" or raw_issue.id == "" then
    return nil, make_error("invalid_beads_issue", "br returned an issue without a valid id")
  end
  local issue = { id = raw_issue.id }
  for _, field in ipairs({ "assignee", "created_by" }) do
    if raw_issue[field] ~= nil then
      if type(raw_issue[field]) ~= "string" then
        return nil, make_error("invalid_beads_issue", "br issue " .. field .. " must be a string")
      end
      issue[field] = raw_issue[field]
    end
  end
  return issue, nil
end

---Parse one Beads issue response from `br show --json`.
---@param output string JSON emitted by `br show <id> --json`.
---@param bead_id string Requested Beads issue identifier.
---@return louiselm.provenance.BeadsIssue? issue
---@return louiselm.provenance.SourceError? error_value
function M.parse_beads_issue(output, bead_id)
  if type(output) ~= "string" then
    return nil, make_error("invalid_beads_issue", "br issue output must be a string")
  end
  if type(bead_id) ~= "string" or bead_id == "" then
    return nil, make_error("invalid_bead_id", "br issue requires a non-empty Beads issue id")
  end

  local decoded_ok, decoded = pcall(nvim.json.decode, output)
  if not decoded_ok or type(decoded) ~= "table" or type(decoded[1]) ~= "table" then
    return nil, make_error("invalid_beads_issue", "br returned malformed issue data")
  end
  local raw_issue = decoded[1]
  if type(raw_issue.id) ~= "string" or raw_issue.id ~= bead_id then
    return nil, make_error("invalid_beads_issue", "br returned a different Beads issue")
  end
  return parse_beads_issue_fields(raw_issue)
end

---Parse all Beads issues returned by `br list --status all --json`.
---@param output string JSON emitted by `br list --status all --json`.
---@return louiselm.provenance.BeadsIssue[]? issues
---@return louiselm.provenance.SourceError? error_value
function M.parse_beads_issues(output)
  if type(output) ~= "string" then
    return nil, make_error("invalid_beads_issues", "br issue list output must be a string")
  end
  local decoded_ok, decoded = pcall(nvim.json.decode, output)
  if not decoded_ok or type(decoded) ~= "table" or type(decoded.issues) ~= "table" then
    return nil, make_error("invalid_beads_issues", "br returned malformed issue list data")
  end

  local issues = {}
  for index, raw_issue in ipairs(decoded.issues) do
    local issue, parse_error = parse_beads_issue_fields(raw_issue)
    if issue == nil then
      parse_error.index = index
      return nil, parse_error
    end
    issues[#issues + 1] = issue
  end
  return issues, nil
end

---@param raw_issue unknown Raw issue value from Beads JSON.
---@return louiselm.provenance.Issue? issue
---@return louiselm.provenance.SourceError? error_value
local function parse_beads_question_fields(raw_issue)
  if
    type(raw_issue) ~= "table"
    or type(raw_issue.id) ~= "string"
    or raw_issue.id == ""
    or type(raw_issue.issue_type) ~= "string"
    or raw_issue.issue_type == ""
  then
    return nil, make_error("invalid_beads_question", "br returned a question issue without a valid id or type")
  end
  local issue = { id = raw_issue.id, issue_type = raw_issue.issue_type }
  for _, field in ipairs({
    "title",
    "status",
    "close_reason",
    "updated_at",
    "description",
    "notes",
    "design",
    "acceptance_criteria",
  }) do
    if raw_issue[field] ~= nil then
      if type(raw_issue[field]) ~= "string" then
        return nil, make_error("invalid_beads_question", "br question issue " .. field .. " must be a string")
      end
      issue[field] = raw_issue[field]
    end
  end
  return issue, nil
end

---Parse all question issues returned by `br list --type question --status all --json`.
---@param output string JSON emitted by `br list --type question --status all --json`.
---@return louiselm.provenance.Issue[]? issues
---@return louiselm.provenance.SourceError? error_value
function M.parse_beads_questions(output)
  if type(output) ~= "string" then
    return nil, make_error("invalid_beads_questions", "br question list output must be a string")
  end
  local decoded_ok, decoded = pcall(nvim.json.decode, output)
  if not decoded_ok or type(decoded) ~= "table" or type(decoded.issues) ~= "table" then
    return nil, make_error("invalid_beads_questions", "br returned malformed question list data")
  end

  local issues = {}
  for index, raw_issue in ipairs(decoded.issues) do
    local issue, parse_error = parse_beads_question_fields(raw_issue)
    if issue == nil then
      parse_error.index = index
      return nil, parse_error
    end
    issues[#issues + 1] = issue
  end
  return issues, nil
end

---@param value string
---@return string
local function trim(value)
  local without_leading = value:gsub("^%s+", "")
  local without_trailing = without_leading:gsub("%s+$", "")
  return without_trailing
end

---Collect commits from git and parse their commit messages.
---The completion callback is always scheduled out of vim.system's fast event.
---@param cwd string Absolute repository working directory.
---@param revision_range string Git revision or range passed as one argument.
---@param callback fun(commits: louiselm.provenance.Commit[]?, error_value: louiselm.provenance.SourceError?)
---@return boolean started True when vim.system was started.
---@return louiselm.provenance.SourceError? error_value Launch or validation error.
function M.git_log(cwd, revision_range, callback)
  if type(cwd) ~= "string" or cwd == "" then
    return false, make_error("invalid_cwd", "git source requires a non-empty cwd")
  end
  if type(revision_range) ~= "string" or revision_range == "" then
    return false, make_error("invalid_revision_range", "git source requires a non-empty revision range")
  end
  if type(callback) ~= "function" then
    return false, make_error("invalid_callback", "git source requires a callback")
  end

  local call_ok, handle_or_error = pcall(nvim.system, {
    "git",
    "log",
    "--no-decorate",
    "--no-color",
    "--format=" .. LOG_FORMAT,
    revision_range,
  }, { cwd = cwd, text = true }, function(result)
    local function finish()
      if result.code ~= 0 then
        local detail = trim(result.stderr or "")
        callback(nil, make_error("git_failed", "git log failed", detail ~= "" and detail or nil, result.code))
        return
      end
      local commits, parse_error = M.parse_git_log(result.stdout or "")
      callback(commits, parse_error)
    end
    nvim.schedule(finish)
  end)
  if not call_ok then
    return false, make_error("git_launch_failed", tostring(handle_or_error))
  end
  if handle_or_error == nil then
    return false, make_error("git_launch_failed", "vim.system did not return a process handle")
  end
  return true, nil
end

---Collect one issue's commit correlations from bvr.
---The completion callback is always scheduled out of vim.system's fast event.
---@param cwd string Absolute repository working directory.
---@param bead_id string Beads issue identifier.
---@param callback fun(history: louiselm.provenance.IssueHistory?, error_value: louiselm.provenance.SourceError?)
---@return boolean started True when vim.system was started.
---@return louiselm.provenance.SourceError? error_value Launch or validation error.
function M.bvr_history(cwd, bead_id, callback)
  if type(cwd) ~= "string" or cwd == "" then
    return false, make_error("invalid_cwd", "bvr source requires a non-empty cwd")
  end
  if type(bead_id) ~= "string" or bead_id == "" then
    return false, make_error("invalid_bead_id", "bvr source requires a non-empty Beads issue id")
  end
  if type(callback) ~= "function" then
    return false, make_error("invalid_callback", "bvr source requires a callback")
  end

  local call_ok, handle_or_error = pcall(nvim.system, {
    "bvr",
    "--robot-history",
    "--bead-history",
    bead_id,
    "--history-since",
    HISTORY_SINCE,
    "--history-limit",
    HISTORY_LIMIT,
  }, { cwd = cwd, text = true }, function(result)
    local function finish()
      if result.code ~= 0 then
        local detail = trim(result.stderr or "")
        callback(nil, make_error("bvr_failed", "bvr history failed", detail ~= "" and detail or nil, result.code))
        return
      end
      local history, parse_error = M.parse_bvr_history(result.stdout or "", bead_id)
      callback(history, parse_error)
    end
    nvim.schedule(finish)
  end)
  if not call_ok then
    return false, make_error("bvr_launch_failed", tostring(handle_or_error))
  end
  if handle_or_error == nil then
    return false, make_error("bvr_launch_failed", "vim.system did not return a process handle")
  end
  return true, nil
end

---Collect one issue's actor fields from Beads.
---The completion callback is always scheduled out of vim.system's fast event.
---@param cwd string Absolute repository working directory.
---@param bead_id string Beads issue identifier.
---@param callback fun(issue: louiselm.provenance.BeadsIssue?, error_value: louiselm.provenance.SourceError?)
---@return boolean started True when vim.system was started.
---@return louiselm.provenance.SourceError? error_value Launch or validation error.
function M.beads_issue(cwd, bead_id, callback)
  if type(cwd) ~= "string" or cwd == "" then
    return false, make_error("invalid_cwd", "br source requires a non-empty cwd")
  end
  if type(bead_id) ~= "string" or bead_id == "" then
    return false, make_error("invalid_bead_id", "br issue requires a non-empty Beads issue id")
  end
  if type(callback) ~= "function" then
    return false, make_error("invalid_callback", "br source requires a callback")
  end

  local call_ok, handle_or_error = pcall(
    nvim.system,
    { "br", "show", bead_id, "--json" },
    { cwd = cwd, text = true },
    function(result)
      local function finish()
        if result.code ~= 0 then
          local detail = trim(result.stderr or "")
          callback(nil, make_error("br_failed", "br issue lookup failed", detail ~= "" and detail or nil, result.code))
          return
        end
        local issue, parse_error = M.parse_beads_issue(result.stdout or "", bead_id)
        callback(issue, parse_error)
      end
      nvim.schedule(finish)
    end
  )
  if not call_ok then
    return false, make_error("br_launch_failed", tostring(handle_or_error))
  end
  if handle_or_error == nil then
    return false, make_error("br_launch_failed", "vim.system did not return a process handle")
  end
  return true, nil
end

---Collect all Beads issues and their actor fields.
---The completion callback is always scheduled out of vim.system's fast event.
---@param cwd string Absolute repository working directory.
---@param callback fun(issues: louiselm.provenance.BeadsIssue[]?, error_value: louiselm.provenance.SourceError?)
---@return boolean started True when vim.system was started.
---@return louiselm.provenance.SourceError? error_value Launch or validation error.
function M.beads_issues(cwd, callback)
  if type(cwd) ~= "string" or cwd == "" then
    return false, make_error("invalid_cwd", "br source requires a non-empty cwd")
  end
  if type(callback) ~= "function" then
    return false, make_error("invalid_callback", "br source requires a callback")
  end

  local call_ok, handle_or_error = pcall(
    nvim.system,
    { "br", "list", "--status", "all", "--json" },
    { cwd = cwd, text = true },
    function(result)
      local function finish()
        if result.code ~= 0 then
          local detail = trim(result.stderr or "")
          callback(nil, make_error("br_failed", "br issue list failed", detail ~= "" and detail or nil, result.code))
          return
        end
        local issues, parse_error = M.parse_beads_issues(result.stdout or "")
        callback(issues, parse_error)
      end
      nvim.schedule(finish)
    end
  )
  if not call_ok then
    return false, make_error("br_launch_failed", tostring(handle_or_error))
  end
  if handle_or_error == nil then
    return false, make_error("br_launch_failed", "vim.system did not return a process handle")
  end
  return true, nil
end

---Collect all question issues with their Decision-relevant fields.
---The completion callback is always scheduled out of vim.system's fast event.
---@param cwd string Absolute repository working directory.
---@param callback fun(issues: louiselm.provenance.Issue[]?, error_value: louiselm.provenance.SourceError?)
---@return boolean started True when vim.system was started.
---@return louiselm.provenance.SourceError? error_value Launch or validation error.
function M.beads_questions(cwd, callback)
  if type(cwd) ~= "string" or cwd == "" then
    return false, make_error("invalid_cwd", "br source requires a non-empty cwd")
  end
  if type(callback) ~= "function" then
    return false, make_error("invalid_callback", "br source requires a callback")
  end

  local call_ok, handle_or_error = pcall(
    nvim.system,
    { "br", "list", "--type", "question", "--status", "all", "--json" },
    { cwd = cwd, text = true },
    function(result)
      local function finish()
        if result.code ~= 0 then
          local detail = trim(result.stderr or "")
          callback(nil, make_error("br_failed", "br question list failed", detail ~= "" and detail or nil, result.code))
          return
        end
        local issues, parse_error = M.parse_beads_questions(result.stdout or "")
        callback(issues, parse_error)
      end
      nvim.schedule(finish)
    end
  )
  if not call_ok then
    return false, make_error("br_launch_failed", tostring(handle_or_error))
  end
  if handle_or_error == nil then
    return false, make_error("br_launch_failed", "vim.system did not return a process handle")
  end
  return true, nil
end

return M
