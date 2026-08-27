---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local Correlate = require("louiselm.provenance.correlate")
local Sources = require("louiselm.provenance.sources")
local Locator = require("louiselm.session.locator")

local M = {}

---@class louiselm.ui.ProvenanceOptions
---@field cwd? string Working directory used for git lookup.
---@field definitions? table<string, louiselm.session.TranscriptDefinition> Agent definitions for transcript lookup.
---@field locator_options? louiselm.session.LocatorOptions Test or host-specific transcript roots.
---@field is_active? fun(): boolean Whether the requesting view still exists.
---@field on_error? fun(message: string) Receives expected lookup or display failures.

---@param value string
---@return boolean
local function valid_sha(value)
  return value:match("^[0-9a-fA-F]+$") ~= nil and #value >= 7 and #value <= 40
end

---@param line string
---@param column integer Zero-based byte column.
---@return string? sha
local function sha_at_cursor(line, column)
  local matches = {}
  for start_index, value in line:gmatch("()([0-9a-fA-F]+)") do
    if #value >= 7 and #value <= 40 then
      matches[#matches + 1] = { start_index = start_index, value = value }
    end
  end
  if #matches ~= 1 then
    return nil
  end
  local match = matches[1]
  local end_index = match.start_index + #match.value - 1
  return match.start_index <= column + 1 and column + 1 <= end_index and match.value or nil
end

---@param value string
---@return boolean
local function valid_issue_id(value)
  return value:match("^[%a][%w%-]*%-%w[%w%.%-]*$") ~= nil
end

---@param line string
---@param column integer Zero-based byte column.
---@return string? issue_id
local function issue_id_at_cursor(line, column)
  local matches = {}
  for start_index, value in line:gmatch("()([%a][%w%-]*%-%w[%w%.%-]*)") do
    if valid_issue_id(value) then
      matches[#matches + 1] = { start_index = start_index, value = value }
    end
  end
  if #matches ~= 1 then
    return nil
  end
  local match = matches[1]
  local end_index = match.start_index + #match.value - 1
  return match.start_index <= column + 1 and column + 1 <= end_index and match.value or nil
end

---@param buffer integer
---@return string? sha
local function current_sha(buffer)
  local cursor = nvim.api.nvim_win_get_cursor(0)
  local line = nvim.api.nvim_buf_get_lines(buffer, cursor[1] - 1, cursor[1], false)[1]
  return type(line) == "string" and sha_at_cursor(line, cursor[2]) or nil
end

---@param buffer integer
---@return string? issue_id
local function current_issue(buffer)
  local cursor = nvim.api.nvim_win_get_cursor(0)
  local line = nvim.api.nvim_buf_get_lines(buffer, cursor[1] - 1, cursor[1], false)[1]
  return type(line) == "string" and issue_id_at_cursor(line, cursor[2]) or nil
end

---@param options louiselm.ui.ProvenanceOptions
---@param message string
local function report_error(options, message)
  if options.on_error ~= nil then
    options.on_error(message)
  end
end

---@param commit louiselm.provenance.Commit
---@param edges louiselm.provenance.Edge[]
---@param options louiselm.ui.ProvenanceOptions
---@return string[] lines
local function commit_lines(commit, edges, options)
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
      local path, error_message = Locator.resolve(session_id, options.definitions or {}, options.locator_options)
      if path ~= nil then
        lines[#lines + 1] = "- " .. session_id .. " → " .. path
      else
        lines[#lines + 1] = "- " .. session_id .. " → unresolved (" .. (error_message or "unknown error") .. ")"
      end
    end
  end
  return lines
end

---@param history louiselm.provenance.IssueHistory
---@param edges louiselm.provenance.Edge[]
---@return string[] lines
local function issue_lines(history, edges)
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
  for _, edge in ipairs(edges) do
    lines[#lines + 1] = string.format(
      "- %s · %s · %d%% confidence",
      edge.target.id,
      edge.correlation_method or edge.method,
      math.floor(edge.confidence * 100 + 0.5)
    )
  end
  if #edges == 0 then
    lines[#lines + 1] = "- none correlated"
  end
  return lines
end

---@param name string Buffer URI.
---@param lines string[]
---@return boolean opened
---@return string? error_message
local function open_provenance(name, lines)
  local buffer = nvim.api.nvim_create_buf(false, true)
  local opened, error_message = pcall(function()
    nvim.api.nvim_buf_set_name(buffer, name)
    nvim.api.nvim_set_option_value("buftype", "nofile", { buf = buffer })
    nvim.api.nvim_set_option_value("bufhidden", "wipe", { buf = buffer })
    nvim.api.nvim_set_option_value("swapfile", false, { buf = buffer })
    nvim.api.nvim_set_option_value("filetype", "markdown", { buf = buffer })
    nvim.api.nvim_buf_set_lines(buffer, 0, -1, false, lines)
    nvim.api.nvim_set_option_value("modifiable", false, { buf = buffer })
    local width = math.min(100, math.max(1, nvim.o.columns - 4))
    local height = math.min(#lines, math.max(1, nvim.o.lines - 4))
    nvim.api.nvim_open_win(buffer, true, {
      relative = "editor",
      row = 1,
      col = 2,
      width = width,
      height = height,
      style = "minimal",
      border = "rounded",
      title = " Provenance ",
      title_pos = "center",
    })
  end)
  if not opened then
    if nvim.api.nvim_buf_is_valid(buffer) then
      nvim.api.nvim_buf_delete(buffer, { force = true })
    end
    return false, tostring(error_message)
  end
  local function close()
    if nvim.api.nvim_buf_is_valid(buffer) then
      nvim.api.nvim_buf_delete(buffer, { force = true })
    end
  end
  nvim.keymap.set("n", "q", close, { buffer = buffer, silent = true, nowait = true, desc = "Close Provenance" })
  nvim.keymap.set("n", "<Esc>", close, { buffer = buffer, silent = true, nowait = true, desc = "Close Provenance" })
  return true
end

---@param sha string
---@param options louiselm.ui.ProvenanceOptions
---@return boolean started
---@return string? error_message
local function show_commit(sha, options)
  local started, start_error = Sources.git_log(
    options.cwd or nvim.fn.getcwd(),
    sha .. "^.." .. sha,
    function(commits, parse_error)
      if options.is_active ~= nil and not options.is_active() then
        return
      end
      if commits == nil or parse_error ~= nil or #commits ~= 1 then
        report_error(options, "git returned malformed commit data")
        return
      end
      local edges, correlate_error = Correlate.commits(commits)
      if edges == nil or correlate_error ~= nil then
        report_error(options, "could not correlate commit " .. sha)
        return
      end
      local lines = commit_lines(commits[1], edges, options)
      local opened, open_error = open_provenance("louiselm://provenance/commit/" .. commits[1].id, lines)
      if not opened then
        report_error(options, "could not display Provenance: " .. (open_error or "unknown error"))
      end
    end
  )
  if not started then
    return false, start_error and start_error.message or "could not start git"
  end
  return true
end

---@param issue_id string
---@param options louiselm.ui.ProvenanceOptions
---@return boolean started
---@return string? error_message
local function show_issue(issue_id, options)
  local started, start_error = Sources.bvr_history(
    options.cwd or nvim.fn.getcwd(),
    issue_id,
    function(history, source_error)
      if options.is_active ~= nil and not options.is_active() then
        return
      end
      if history == nil or source_error ~= nil then
        report_error(options, "could not read issue history")
        return
      end
      local edges, correlate_error = Correlate.issue(history)
      if edges == nil or correlate_error ~= nil then
        report_error(options, "could not correlate issue " .. issue_id)
        return
      end
      local opened, open_error =
        open_provenance("louiselm://provenance/issue/" .. issue_id, issue_lines(history, edges))
      if not opened then
        report_error(options, "could not display Provenance: " .. (open_error or "unknown error"))
      end
    end
  )
  if not started then
    return false, start_error and start_error.message or "could not start bvr"
  end
  return true
end

---Inspect the commit SHA or Beads issue under the cursor or prompt for one.
---@param buffer integer Source buffer.
---@param options? louiselm.ui.ProvenanceOptions Lookup and lifecycle callbacks.
---@return boolean started Whether a lookup started or an SHA prompt was opened.
---@return string? error_message Why inspection could not start.
function M.inspect(buffer, options)
  if type(buffer) ~= "number" or not nvim.api.nvim_buf_is_valid(buffer) then
    return false, "source buffer is unavailable"
  end
  if options ~= nil and type(options) ~= "table" then
    return false, "Provenance options must be a table"
  end
  options = options or {}
  if options.cwd ~= nil and (type(options.cwd) ~= "string" or options.cwd == "") then
    return false, "Provenance cwd must be a non-empty string"
  end
  if options.is_active ~= nil and type(options.is_active) ~= "function" then
    return false, "Provenance is_active must be a function"
  end
  if options.on_error ~= nil and type(options.on_error) ~= "function" then
    return false, "Provenance on_error must be a function"
  end
  local sha = current_sha(buffer)
  if sha ~= nil then
    return show_commit(sha, options)
  end
  local issue_id = current_issue(buffer)
  if issue_id ~= nil then
    return show_issue(issue_id, options)
  end
  nvim.ui.input({ prompt = "Commit SHA or Beads issue id: " }, function(value)
    if value == nil or (options.is_active ~= nil and not options.is_active()) then
      return
    end
    local prompted_sha = nvim.trim(value)
    if valid_sha(prompted_sha) then
      local _, lookup_error = show_commit(prompted_sha, options)
      if lookup_error ~= nil then
        report_error(options, lookup_error)
      end
      return
    end
    if valid_issue_id(prompted_sha) then
      local _, lookup_error = show_issue(prompted_sha, options)
      if lookup_error ~= nil then
        report_error(options, lookup_error)
      end
      return
    end
    report_error(options, "enter a valid commit SHA or Beads issue id")
  end)
  return true
end

return M
