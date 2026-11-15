---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local Correlate = require("louiselm.provenance.correlate")
local Sources = require("louiselm.provenance.sources")
local Vintage = require("louiselm.provenance.vintage")
local Locator = require("louiselm.session.locator")
local Render = require("louiselm.ui.provenance.render")

local M = {}

---@class louiselm.ui.ProvenanceOptions
---@field beads? boolean Explicit Beads integration opt-in; defaults to false. Git-only inspection stays available.
---@field cwd? string Working directory used for git lookup.
---@field definitions? table<string, louiselm.session.TranscriptDefinition> Agent definitions for transcript lookup.
---@field locator_options? louiselm.session.LocatorOptions Test or host-specific transcript roots.
---@field is_active? fun(): boolean Whether the requesting view still exists.
---@field on_error? fun(message: string) Receives expected lookup or display failures.

---Yield the single candidate the cursor sits inside. Ambiguity is not resolved by
---proximity: two candidates on one line means no answer, so an inspect keymap never
---guesses which id the user meant.
---@param matches { start_index: integer, value: string, length: integer }[]
---@param column integer Zero-based byte column.
---@return string?
local function only_match_at(matches, column)
  if #matches ~= 1 then
    return nil
  end
  local match = matches[1]
  local end_index = match.start_index + match.length - 1
  return match.start_index <= column + 1 and column + 1 <= end_index and match.value or nil
end

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
      matches[#matches + 1] = { start_index = start_index, value = value, length = #value }
    end
  end
  return only_match_at(matches, column)
end

---@param line string
---@param column integer
---@return string? operation_id
local function mutation_at_cursor(line, column)
  local matches = {}
  for start_index, value in line:gmatch("()([0-9a-f%-]+)") do
    if Sources.valid_operation_id(value) then
      matches[#matches + 1] = { start_index = start_index, value = value, length = #value }
    end
  end
  return only_match_at(matches, column)
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
      matches[#matches + 1] = { start_index = start_index, value = value, length = #value }
    end
  end
  return only_match_at(matches, column)
end

---@param value string
---@return boolean
local function valid_session_id(value)
  local agent_name, session_id = value:match("^([^/%z%s]+)/([^/%z%s]+)$")
  return agent_name ~= nil and agent_name ~= "." and agent_name ~= ".." and session_id ~= "." and session_id ~= ".."
end

---@param value string
---@return string normalized
local function normalize_session_id(value)
  local inner = value:match("^`(.+)`$")
  return inner or value
end

---@param line string
---@param column integer Zero-based byte column.
---@return string? session_id
local function session_id_at_cursor(line, column)
  local matches = {}
  for start_index, value in line:gmatch("()([^%s/]+/[^%s/]+)") do
    value = value:gsub("[,;%.%)]+$", "")
    local normalized = normalize_session_id(value)
    if valid_session_id(normalized) then
      matches[#matches + 1] = { start_index = start_index, value = normalized, length = #value }
    end
  end
  return only_match_at(matches, column)
end

---@param buffer integer
---@param extract fun(line: string, column: integer): string?
---@return string?
local function current_token(buffer, extract)
  local cursor = nvim.api.nvim_win_get_cursor(0)
  local line = nvim.api.nvim_buf_get_lines(buffer, cursor[1] - 1, cursor[1], false)[1]
  return type(line) == "string" and extract(line, cursor[2]) or nil
end

---@param buffer integer
---@return string? sha
local function current_sha(buffer)
  return current_token(buffer, sha_at_cursor)
end

---@param buffer integer
---@return string? operation_id
local function current_mutation(buffer)
  return current_token(buffer, mutation_at_cursor)
end

---@param buffer integer
---@return string? issue_id
local function current_issue(buffer)
  return current_token(buffer, issue_id_at_cursor)
end

---@param buffer integer
---@return string? session_id
local function current_session(buffer)
  return current_token(buffer, session_id_at_cursor)
end

---@param options louiselm.ui.ProvenanceOptions
---@param message string
local function report_error(options, message)
  if options.on_error ~= nil then
    options.on_error(message)
  end
end

---@param session_id string
---@param options louiselm.ui.ProvenanceOptions
---@return louiselm.ui.provenance.Transcript
local function resolve_transcript(session_id, options)
  local path, error_message = Locator.resolve(session_id, options.definitions or {}, options.locator_options)
  return { path = path, error = error_message }
end

---@param edges louiselm.provenance.Edge[]
---@param options louiselm.ui.ProvenanceOptions
---@return table<string, louiselm.ui.provenance.Transcript>
local function resolve_transcripts(edges, options)
  local transcripts = {}
  for _, edge in ipairs(edges) do
    if edge.target.kind == "session" then
      transcripts[edge.target.id] = resolve_transcript(edge.target.id, options)
    end
  end
  return transcripts
end

---@param issue louiselm.provenance.BeadsIssue
---@return louiselm.ui.provenance.DecisionRelation[]
local function decision_relations(issue)
  local relations = {}
  for _, field in ipairs({ "description", "notes", "design", "acceptance_criteria" }) do
    local text = issue[field]
    if text ~= nil then
      for relation, target_id in text:gmatch("Decision relation:%s*(%w+)%s+(%S+)") do
        if relation == "supersedes" or relation == "reconsiders" then
          relations[#relations + 1] = { relation = relation, target_id = target_id }
        end
      end
    end
  end
  return relations
end

---@param name string Buffer URI.
---@param lines string[]
---@param on_select? fun(line: integer) Called with the 1-based cursor line on <CR>.
---@return boolean opened
---@return string? error_message
local function open_provenance(name, lines, on_select)
  local buffer = nvim.fn.bufnr(name)
  local new_buffer = buffer <= 0
  if new_buffer then
    buffer = nvim.api.nvim_create_buf(false, true)
  end
  local opened, error_message = pcall(function()
    if new_buffer then
      nvim.api.nvim_buf_set_name(buffer, name)
    end
    nvim.api.nvim_set_option_value("buftype", "nofile", { buf = buffer })
    nvim.api.nvim_set_option_value("bufhidden", "wipe", { buf = buffer })
    nvim.api.nvim_set_option_value("swapfile", false, { buf = buffer })
    nvim.api.nvim_set_option_value("filetype", "markdown", { buf = buffer })
    nvim.api.nvim_set_option_value("modifiable", true, { buf = buffer })
    nvim.api.nvim_buf_set_lines(buffer, 0, -1, false, lines)
    nvim.api.nvim_set_option_value("modifiable", false, { buf = buffer })
    local width = math.min(100, math.max(1, nvim.o.columns - 4))
    local height = math.min(#lines, math.max(1, nvim.o.lines - 4))
    local window = nvim.fn.bufwinid(buffer)
    if window > 0 and nvim.api.nvim_win_is_valid(window) then
      nvim.api.nvim_win_set_height(window, height)
      nvim.api.nvim_set_current_win(window)
    else
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
    end
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
  if on_select ~= nil then
    nvim.keymap.set("n", "<CR>", function()
      on_select(nvim.api.nvim_win_get_cursor(0)[1])
    end, { buffer = buffer, silent = true, nowait = true, desc = "Select Provenance row" })
  end
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
      local lines =
        Render.commit({ commit = commits[1], edges = edges, transcripts = resolve_transcripts(edges, options) })
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

---@param operation_id string
---@param options louiselm.ui.ProvenanceOptions
---@return boolean started
---@return string? error_message
local function show_mutation(operation_id, options)
  if options.beads ~= true then
    return false, "Beads is disabled; set beads.enabled = true and run :checkhealth louiselm"
  end
  local started, start_error = Sources.beads_mutation(operation_id, function(mutation, source_error)
    if options.is_active ~= nil and not options.is_active() then
      return
    end
    if mutation == nil or source_error ~= nil then
      report_error(options, "could not read broker Beads mutation inspection")
      return
    end
    local opened, open_error =
      open_provenance("louiselm://provenance/mutation/" .. operation_id, Render.mutation(mutation))
    if not opened then
      report_error(options, "could not display Provenance: " .. (open_error or "unknown error"))
    end
  end)
  if not started then
    return false, start_error and start_error.message or "could not start broker inspection"
  end
  return true
end

---@param issue_id string
---@param options louiselm.ui.ProvenanceOptions
---@return boolean started
---@return string? error_message
local function show_issue(issue_id, options)
  if options.beads ~= true then
    return false, "Beads is disabled; set beads.enabled = true and run :checkhealth louiselm"
  end
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
      local actors_started, actors_error = Sources.beads_issue(
        options.cwd or nvim.fn.getcwd(),
        issue_id,
        function(issue, actor_source_error)
          if options.is_active ~= nil and not options.is_active() then
            return
          end
          if issue == nil or actor_source_error ~= nil then
            report_error(options, "could not read issue actors")
            return
          end
          local actor_edges, actor_error = Correlate.issue_actors(issue)
          if actor_edges == nil or actor_error ~= nil then
            report_error(options, "could not correlate issue actors " .. issue_id)
            return
          end
          local all_edges = Correlate.merge_issue_sessions(edges, actor_edges)
          local presentation =
            { history = history, edges = all_edges, transcripts = resolve_transcripts(all_edges, options) }
          local opened, open_error =
            open_provenance("louiselm://provenance/issue/" .. issue_id, Render.issue(presentation))
          if not opened then
            report_error(options, "could not display Provenance: " .. (open_error or "unknown error"))
          end
        end
      )
      if not actors_started then
        report_error(options, actors_error and actors_error.message or "could not start br")
      end
    end
  )
  if not started then
    return false, start_error and start_error.message or "could not start bvr"
  end
  return true
end

---@param issue_id string
---@param options louiselm.ui.ProvenanceOptions
---@return boolean started
---@return string? error_message
local function show_decision(issue_id, options)
  local name = "louiselm://provenance/decision/" .. issue_id
  local opened, open_error = open_provenance(name, {
    "# Decision " .. issue_id,
    "",
    "Loading Provenance evidence...",
  })
  if not opened then
    return false, open_error
  end

  local started, start_error = Sources.bvr_history(
    options.cwd or nvim.fn.getcwd(),
    issue_id,
    function(history, source_error)
      if options.is_active ~= nil and not options.is_active() then
        return
      end
      if history == nil or source_error ~= nil then
        report_error(options, "could not read Decision history")
        return
      end
      local edges, correlate_error = Correlate.issue(history)
      if edges == nil or correlate_error ~= nil then
        report_error(options, "could not correlate Decision " .. issue_id)
        return
      end
      local actors_started, actors_error = Sources.beads_issue(
        options.cwd or nvim.fn.getcwd(),
        issue_id,
        function(issue, actor_source_error)
          if options.is_active ~= nil and not options.is_active() then
            return
          end
          if issue == nil or actor_source_error ~= nil or issue.issue_type == nil then
            report_error(options, "could not read Decision anchor")
            return
          end
          local actor_edges, actor_error = Correlate.issue_actors(issue)
          if actor_edges == nil or actor_error ~= nil then
            report_error(options, "could not correlate Decision actors " .. issue_id)
            return
          end
          local timeline, timeline_error =
            Correlate.decision_timeline(issue, history, Correlate.merge_issue_sessions(edges, actor_edges))
          if timeline == nil or timeline_error ~= nil then
            report_error(options, "could not derive Decision evidence " .. issue_id)
            return
          end
          ---@type louiselm.ui.provenance.DecisionPresentation
          local presentation = {
            timeline = timeline,
            status = history.status or issue.status or "unknown",
            transcripts = resolve_transcripts(timeline.sessions, options),
            relations = decision_relations(issue),
          }
          local function render()
            local rendered, render_error = open_provenance(name, Render.decision(presentation))
            if not rendered then
              report_error(options, "could not display Provenance: " .. (render_error or "unknown error"))
            end
          end
          local closed = history.milestones.closed
          if issue.status == "closed" and closed ~= nil then
            local vintage_started, vintage_error = Vintage.load(
              options.cwd or nvim.fn.getcwd(),
              closed.commit_sha,
              function(vintage, historical_error)
                if options.is_active ~= nil and not options.is_active() then
                  return
                end
                presentation.historical = {
                  ref = closed.commit_sha,
                  vintage = historical_error == nil and vintage or nil,
                  error = historical_error and historical_error.message or nil,
                }
                render()
              end
            )
            if not vintage_started then
              presentation.historical = { ref = closed.commit_sha, error = vintage_error and vintage_error.message }
              render()
            end
            return
          end
          render()
        end
      )
      if not actors_started then
        report_error(options, actors_error and actors_error.message or "could not start br")
      end
    end
  )
  if not started then
    return false, start_error and start_error.message or "could not start bvr"
  end
  return true
end

---@param session_id string
---@param options louiselm.ui.ProvenanceOptions
---@return boolean started
---@return string? error_message
local function show_session(session_id, options)
  local started, start_error = Sources.git_log(options.cwd or nvim.fn.getcwd(), "--all", function(commits, source_error)
    if options.is_active ~= nil and not options.is_active() then
      return
    end
    if commits == nil or source_error ~= nil then
      report_error(options, "could not read commit history")
      return
    end
    local function complete(issues, issue_source_error)
      if options.is_active ~= nil and not options.is_active() then
        return
      end
      if issues == nil or issue_source_error ~= nil then
        report_error(options, "could not read Beads issues")
        return
      end
      local edges, correlate_error = Correlate.session(session_id, commits, issues)
      if edges == nil or correlate_error ~= nil then
        report_error(options, "could not correlate Session " .. session_id)
        return
      end
      local presentation = { id = session_id, edges = edges, transcript = resolve_transcript(session_id, options) }
      local opened, open_error =
        open_provenance("louiselm://provenance/session/" .. session_id, Render.session(presentation))
      if not opened then
        report_error(options, "could not display Provenance: " .. (open_error or "unknown error"))
      end
    end
    if options.beads ~= true then
      complete({})
      return
    end
    local issues_started, issues_error = Sources.beads_issues(options.cwd or nvim.fn.getcwd(), complete)
    if not issues_started then
      report_error(options, issues_error and issues_error.message or "could not start br")
    end
  end)
  if not started then
    return false, start_error and start_error.message or "could not start git"
  end
  return true
end

---Show a read-only index of browsable Decisions derived from question issues.
---@param options? louiselm.ui.ProvenanceOptions Lookup and lifecycle callbacks.
---@return boolean started
---@return string? error_message
function M.show_decisions(options)
  if options ~= nil and type(options) ~= "table" then
    return false, "Provenance options must be a table"
  end
  options = options or {}
  if options.beads ~= true then
    return false, "Beads is disabled; set beads.enabled = true and run :checkhealth louiselm"
  end
  if options.cwd ~= nil and (type(options.cwd) ~= "string" or options.cwd == "") then
    return false, "Provenance cwd must be a non-empty string"
  end
  if options.is_active ~= nil and type(options.is_active) ~= "function" then
    return false, "Provenance is_active must be a function"
  end
  if options.on_error ~= nil and type(options.on_error) ~= "function" then
    return false, "Provenance on_error must be a function"
  end
  local started, start_error = Sources.beads_questions(options.cwd or nvim.fn.getcwd(), function(issues, source_error)
    if options.is_active ~= nil and not options.is_active() then
      return
    end
    if issues == nil or source_error ~= nil then
      report_error(options, "could not read Beads question issues")
      return
    end
    local graph, correlate_error = Correlate.decisions(issues)
    if graph == nil or correlate_error ~= nil then
      report_error(options, "could not derive Decisions")
      return
    end
    local lines, id_by_line = Render.decisions(graph)
    local opened, open_error = open_provenance("louiselm://provenance/decisions", lines, function(line)
      local issue_id = id_by_line[line]
      if issue_id == nil then
        return
      end
      local _, lookup_error = show_decision(issue_id, options)
      if lookup_error ~= nil then
        report_error(options, lookup_error)
      end
    end)
    if not opened then
      report_error(options, "could not display Provenance: " .. (open_error or "unknown error"))
    end
  end)
  if not started then
    return false, start_error and start_error.message or "could not start br"
  end
  return true
end

---Inspect a commit, Beads issue or mutation, or Session under the cursor or prompt for one.
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
  local operation_id = current_mutation(buffer)
  if operation_id ~= nil then
    return show_mutation(operation_id, options)
  end
  local sha = current_sha(buffer)
  if sha ~= nil then
    return show_commit(sha, options)
  end
  local session_id = current_session(buffer)
  if session_id ~= nil then
    return show_session(session_id, options)
  end
  local issue_id = current_issue(buffer)
  if issue_id ~= nil then
    return show_issue(issue_id, options)
  end
  nvim.ui.input({ prompt = "Commit SHA, Beads issue/operation id, or Session id: " }, function(value)
    if value == nil or (options.is_active ~= nil and not options.is_active()) then
      return
    end
    local prompted_sha = normalize_session_id(nvim.trim(value))
    if Sources.valid_operation_id(prompted_sha) then
      local _, lookup_error = show_mutation(prompted_sha, options)
      if lookup_error ~= nil then
        report_error(options, lookup_error)
      end
      return
    end
    if valid_sha(prompted_sha) then
      local _, lookup_error = show_commit(prompted_sha, options)
      if lookup_error ~= nil then
        report_error(options, lookup_error)
      end
      return
    end
    if valid_session_id(prompted_sha) then
      local _, lookup_error = show_session(prompted_sha, options)
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
    report_error(options, "enter a valid commit SHA, Beads issue/operation id, or Session id")
  end)
  return true
end

return M
