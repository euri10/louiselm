---Multi-session overview displaying side-by-side vertical windows for concurrent sessions,
---showing each session's modified files, line numbers of editions, diff stats, and conflict alerts.

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim
local Apply = require("louiselm.ui.diff.apply")
local DiffBuffer = require("louiselm.ui.diff.buffer")

local M = {}

---@class louiselm.ui.HunkEdition
---@field start_line integer Starting line number in modified file
---@field end_line integer Ending line number in modified file
---@field old_start integer Starting line number in original file
---@field old_count integer Line count in original file
---@field new_count integer Line count in modified file
---@field added integer Lines added in this hunk
---@field deleted integer Lines deleted in this hunk

---@class louiselm.ui.FileEdits
---@field path string Full or normalized file path
---@field display_path string Relative or shortened file path for display
---@field hunks louiselm.ui.HunkEdition[] List of edition line ranges
---@field total_added integer Total lines added
---@field total_deleted integer Total lines deleted
---@field is_new boolean Whether this file was newly created
---@field is_deleted? boolean Whether this file was deleted
---@field conflicts? string[] Session IDs of other sessions that modified this file
---@field diff? string Unified diff text if available

---@class louiselm.ui.SessionSummary
---@field session_id string Local session identifier
---@field agent string Configured agent name
---@field name string User-facing session name
---@field status string Lifecycle state (ready, prompting, waiting_permission, etc.)
---@field working_dir string ACP working directory
---@field files louiselm.ui.FileEdits[] Modified files in this session
---@field total_files integer Count of modified files
---@field total_added integer Cumulative lines added across all files
---@field total_deleted integer Cumulative lines deleted across all files

---@class louiselm.ui.LineTarget
---@field path string File path to open
---@field line integer Line number to jump to
---@field diff? string Unified diff for preview

---@class louiselm.ui.SessionOverviewOptions
---@field cwd? string Working directory for relative paths
---@field chat? louiselm.ui.Chat Associated chat controller

---@class louiselm.ui.OverviewState
---@field tabpage integer Tabpage handle containing the vertical windows
---@field windows integer[] Window handles for each session
---@field buffers integer[] Buffer handles for each session
---@field session_ids string[] Session IDs corresponding to each window
---@field line_targets table<integer, table<integer, louiselm.ui.LineTarget>> Buffer-specific line jump targets
---@field previous_tabpage? integer Tabpage focused prior to opening overview
---@field unsubscribes fun()[] Event listener cleanup callbacks
---@field chat? louiselm.ui.Chat Associated chat controller

---@type louiselm.ui.OverviewState?
local active_overview = nil

---Parse unified diff hunks into line ranges and edit counts.
---@param diff_text string Unified diff text
---@return louiselm.ui.HunkEdition[] hunks
---@return integer total_added
---@return integer total_deleted
function M.parse_diff_hunks(diff_text)
  local hunks = {}
  local total_added = 0
  local total_deleted = 0
  if type(diff_text) ~= "string" or diff_text == "" then
    return hunks, total_added, total_deleted
  end

  local current_hunk = nil
  for line in diff_text:gmatch("[^\r\n]+") do
    local old_start, old_count, new_start, new_count = line:match("^@@ %-(%d+),?(%d*) %+([0-9]+),?(%d*) @@")
    if old_start ~= nil then
      local ns = tonumber(new_start) or 1
      local nc = (new_count ~= "" and tonumber(new_count)) or 1
      local os = tonumber(old_start) or 1
      local oc = (old_count ~= "" and tonumber(old_count)) or 1
      local end_l = nc > 0 and (ns + nc - 1) or ns
      current_hunk = {
        start_line = ns,
        end_line = end_l,
        old_start = os,
        old_count = oc,
        new_count = nc,
        added = 0,
        deleted = 0,
      }
      hunks[#hunks + 1] = current_hunk
    elseif current_hunk ~= nil then
      local prefix = line:sub(1, 1)
      if prefix == "+" and line:sub(1, 3) ~= "+++" then
        current_hunk.added = current_hunk.added + 1
        total_added = total_added + 1
      elseif prefix == "-" and line:sub(1, 3) ~= "---" then
        current_hunk.deleted = current_hunk.deleted + 1
        total_deleted = total_deleted + 1
      end
    end
  end

  return hunks, total_added, total_deleted
end

---Count lines in a string.
---@param text string
---@return integer
local function count_lines(text)
  if type(text) ~= "string" or text == "" then
    return 0
  end
  local count = 1
  for _ in text:gmatch("\n") do
    count = count + 1
  end
  return count
end

---Find 1-based line number of a substring in file content.
---@param content string
---@param text string
---@return integer? line_nr
local function find_line_for_text(content, text)
  if type(content) ~= "string" or type(text) ~= "string" or text == "" then
    return nil
  end
  local pos = content:find(text, 1, true)
  if pos == nil then
    return nil
  end
  local line_nr = 1
  for _ in content:sub(1, pos - 1):gmatch("\n") do
    line_nr = line_nr + 1
  end
  return line_nr
end

---Normalize a path and compute its display path relative to cwd.
---@param path string
---@param cwd? string
---@return string normalized
---@return string display
local function normalize_and_display_path(path, cwd)
  local normalized = nvim.fs.normalize(nvim.fn.fnamemodify(path, ":p"))
  local base = cwd or nvim.fn.getcwd()
  local normalized_base = nvim.fs.normalize(nvim.fn.fnamemodify(base, ":p"))
  if normalized:sub(1, #normalized_base) == normalized_base then
    local rel = normalized:sub(#normalized_base + 1)
    if rel:sub(1, 1) == "/" then
      rel = rel:sub(2)
    end
    if rel ~= "" then
      return normalized, rel
    end
  end
  local home = nvim.fs.normalize(nvim.fn.expand("~"))
  if home ~= "" and normalized:sub(1, #home) == home then
    return normalized, "~" .. normalized:sub(#home + 1)
  end
  return normalized, normalized
end

---Extract candidate file path from raw input or tool call fields.
---@param raw table
---@return string?
local function extract_path_from_raw(raw)
  if type(raw) ~= "table" then
    return nil
  end
  local candidate = raw.path or raw.filePath or raw.file_path or raw.filename or raw.file
  if type(candidate) == "string" and candidate ~= "" then
    return candidate
  end
  if type(raw.rawInput) == "table" then
    local from_raw = extract_path_from_raw(raw.rawInput)
    if from_raw ~= nil then
      return from_raw
    end
  end
  if type(raw.raw_input) == "table" then
    local from_raw = extract_path_from_raw(raw.raw_input)
    if from_raw ~= nil then
      return from_raw
    end
  end
  if type(raw.input) == "table" then
    local from_raw = extract_path_from_raw(raw.input)
    if from_raw ~= nil then
      return from_raw
    end
  end
  if type(raw.operation) == "table" and type(raw.operation.path) == "string" and raw.operation.path ~= "" then
    return raw.operation.path
  end
  if type(raw.toolCall) == "table" then
    local from_nested = extract_path_from_raw(raw.toolCall.rawInput or raw.toolCall.raw_input or raw.toolCall)
    if from_nested ~= nil then
      return from_nested
    end
  end
  if type(raw.content) == "table" then
    for _, block in ipairs(raw.content) do
      if type(block) == "table" and type(block.path) == "string" and block.path ~= "" then
        return block.path
      end
    end
  end
  if type(raw.title) == "string" then
    local matched = raw.title:match("[eE]dit(?:ing)?%s+([%w_%-./\\]+)")
      or raw.title:match("[wW]rite(?:%s+file)?%s+([%w_%-./\\]+)")
      or raw.title:match("[cC]reat(?:e|ing)%s+([%w_%-./\\]+)")
      or raw.title:match("[uU]pdate%s+([%w_%-./\\]+)")
    if matched ~= nil and matched ~= "" then
      return matched
    end
  end
  return nil
end

---Extract file edit details from one ACP tool call payload or permission request.
---@param raw table Raw payload from tool call or permission request
---@param cwd? string Working directory
---@return louiselm.ui.FileEdits? edit
function M.extract_file_edit(raw, cwd)
  if type(raw) ~= "table" then
    return nil
  end
  local path_candidate = extract_path_from_raw(raw)
  if path_candidate == nil then
    return nil
  end

  local normalized_path, display_path = normalize_and_display_path(path_candidate, cwd)
  local tool_input = raw.rawInput or raw.raw_input or raw.input or raw

  -- 1. Check for diff or patch
  local diff_text = tool_input.diff or tool_input.patch or raw.diff or (raw.operation and raw.operation.diff)
  if type(diff_text) == "string" and diff_text ~= "" then
    local hunks, added, deleted = M.parse_diff_hunks(diff_text)
    if #hunks > 0 then
      return {
        path = normalized_path,
        display_path = display_path,
        hunks = hunks,
        total_added = added,
        total_deleted = deleted,
        is_new = false,
        diff = diff_text,
      }
    end
  end

  -- 2. Check for replacement (oldText/newText, old_str/new_str)
  local replacement = tool_input.replacement
  local old_text = tool_input.oldText or tool_input.old_text or tool_input.old_str or tool_input.old
  local new_text = tool_input.newText or tool_input.new_text or tool_input.new_str or tool_input.new
  if type(replacement) == "table" and type(replacement.old) == "string" then
    old_text = replacement.old
    new_text = replacement.new or ""
  end

  if type(old_text) == "string" and type(new_text) == "string" then
    local old_lines = count_lines(old_text)
    local new_lines = count_lines(new_text)
    local existing_content = Apply.read(normalized_path)
    local start_line = find_line_for_text(existing_content or "", old_text)
      or tonumber(tool_input.start_line)
      or tonumber(tool_input.line)
      or 1
    local end_line = start_line + math.max(0, new_lines - 1)
    local hunk = {
      start_line = start_line,
      end_line = end_line,
      old_start = start_line,
      old_count = old_lines,
      new_count = new_lines,
      added = new_lines,
      deleted = old_lines,
    }
    return {
      path = normalized_path,
      display_path = display_path,
      hunks = { hunk },
      total_added = new_lines,
      total_deleted = old_lines,
      is_new = false,
      diff = nvim.diff(old_text, new_text, { result_type = "unified", ctxlen = 3 }),
    }
  end

  -- 3. Check for full content
  local content = tool_input.content or tool_input.newText or tool_input.file_text
  if type(content) == "string" then
    local original = Apply.read(normalized_path)
    if original ~= nil and original ~= "" then
      local diff = nvim.diff(original, content, { result_type = "unified", ctxlen = 0 })
      local hunks, added, deleted = M.parse_diff_hunks(diff)
      if #hunks > 0 then
        return {
          path = normalized_path,
          display_path = display_path,
          hunks = hunks,
          total_added = added,
          total_deleted = deleted,
          is_new = false,
          diff = diff,
        }
      end
    else
      local line_count = count_lines(content)
      local hunk = {
        start_line = 1,
        end_line = math.max(1, line_count),
        old_start = 0,
        old_count = 0,
        new_count = line_count,
        added = line_count,
        deleted = 0,
      }
      return {
        path = normalized_path,
        display_path = display_path,
        hunks = { hunk },
        total_added = line_count,
        total_deleted = 0,
        is_new = true,
      }
    end
  end

  -- 4. Check for explicit line numbers
  local start_line = tonumber(tool_input.start_line or tool_input.line_number or tool_input.line)
  local end_line = tonumber(tool_input.end_line) or start_line
  if start_line ~= nil then
    local hunk = {
      start_line = start_line,
      end_line = end_line or start_line,
      old_start = start_line,
      old_count = 1,
      new_count = 1,
      added = 0,
      deleted = 0,
    }
    return {
      path = normalized_path,
      display_path = display_path,
      hunks = { hunk },
      total_added = 0,
      total_deleted = 0,
      is_new = false,
    }
  end

  return nil
end

---Merge two FileEdits records for the same path.
---@param existing louiselm.ui.FileEdits
---@param incoming louiselm.ui.FileEdits
local function merge_file_edits(existing, incoming)
  for _, hunk in ipairs(incoming.hunks) do
    existing.hunks[#existing.hunks + 1] = hunk
  end
  table.sort(existing.hunks, function(a, b)
    return a.start_line < b.start_line
  end)
  existing.total_added = existing.total_added + incoming.total_added
  existing.total_deleted = existing.total_deleted + incoming.total_deleted
  if incoming.is_new then
    existing.is_new = true
  end
  if incoming.diff ~= nil then
    existing.diff = incoming.diff
  end
end

---Collect all modified files and edition line numbers for a single session.
---@param session_or_view louiselm.session.Session|louiselm.ui.ChatView
---@param cwd? string
---@return louiselm.ui.SessionSummary
function M.collect_session_summary(session_or_view, cwd)
  local session = session_or_view.session or session_or_view
  local state = session:inspect()
  local working_dir = state.working_dir or cwd or nvim.fn.getcwd()

  local files_map = {} ---@type table<string, louiselm.ui.FileEdits>
  local files_order = {} ---@type string[]

  local function record_edit(edit)
    if edit == nil then
      return
    end
    local existing = files_map[edit.path]
    if existing == nil then
      files_map[edit.path] = edit
      files_order[#files_order + 1] = edit.path
    else
      merge_file_edits(existing, edit)
    end
  end

  -- Inspect view transcript if available
  local transcript = session_or_view.transcript
  if transcript ~= nil and type(transcript.snapshot) == "function" then
    for _, entry in ipairs(transcript:snapshot()) do
      local raw = entry.raw
      if entry.kind == "tool_call" and type(raw) == "table" then
        local has_diff = false
        if type(raw.content) == "table" then
          for _, block in ipairs(raw.content) do
            if type(block) == "table" and block.type == "diff" then
              has_diff = true
              if
                raw.status == "completed"
                and type(block.path) == "string"
                and block.path ~= ""
                and type(block.newText) == "string"
                and (type(block.oldText) == "string" or block.oldText == nil or block.oldText == nvim.NIL)
              then
                -- ACP carries the before/after text, even after the file has been written.
                -- One tool call can edit several files; do not collapse it to its first path.
                local original = type(block.oldText) == "string" and block.oldText or ""
                local diff = nvim.diff(original, block.newText, { result_type = "unified", ctxlen = 3 })
                local edit = M.extract_file_edit({ path = block.path, diff = diff }, working_dir)
                if edit ~= nil then
                  edit.is_new = block.oldText == nil or block.oldText == nvim.NIL
                  record_edit(edit)
                end
              end
            end
          end
        end
        if not has_diff then
          record_edit(M.extract_file_edit(raw, working_dir))
        end
      end
    end
  end

  -- Check any explicit session/view edits
  local extra_edits = rawget(session_or_view, "edits") or (type(session) == "table" and rawget(session, "edits") or nil)
  if type(extra_edits) == "table" then
    for _, item in ipairs(extra_edits) do
      record_edit(M.extract_file_edit(item, working_dir))
    end
  end

  local files = {}
  local total_added = 0
  local total_deleted = 0
  for _, path in ipairs(files_order) do
    local file_entry = files_map[path]
    files[#files + 1] = file_entry
    total_added = total_added + file_entry.total_added
    total_deleted = total_deleted + file_entry.total_deleted
  end

  return {
    session_id = state.id,
    agent = state.agent,
    name = state.name or state.id,
    status = state.status,
    working_dir = working_dir,
    files = files,
    total_files = #files,
    total_added = total_added,
    total_deleted = total_deleted,
  }
end

---Detect conflicts where multiple sessions modified the same file or overlapping lines.
---@param summaries louiselm.ui.SessionSummary[]
function M.detect_conflicts(summaries)
  local file_to_sessions = {} ---@type table<string, string[]>
  for _, summary in ipairs(summaries) do
    for _, file in ipairs(summary.files) do
      local list = file_to_sessions[file.path] or {}
      list[#list + 1] = summary.session_id
      file_to_sessions[file.path] = list
    end
  end

  for _, summary in ipairs(summaries) do
    for _, file in ipairs(summary.files) do
      local all_sessions = file_to_sessions[file.path] or {}
      local other_sessions = {}
      for _, sid in ipairs(all_sessions) do
        if sid ~= summary.session_id then
          other_sessions[#other_sessions + 1] = sid
        end
      end
      if #other_sessions > 0 then
        file.conflicts = other_sessions
      end
    end
  end
end

---Render buffer lines and line jump targets for one session summary.
---@param summary louiselm.ui.SessionSummary
---@return string[] lines
---@return table<integer, louiselm.ui.LineTarget> targets
function M.render_session_buffer(summary)
  local lines = {}
  local targets = {} ---@type table<integer, louiselm.ui.LineTarget>

  local function add_line(text, target)
    lines[#lines + 1] = text
    if target ~= nil then
      targets[#lines] = target
    end
  end

  add_line("========================================")
  add_line(string.format("SESSION: %s (%s)", summary.name, summary.agent))
  add_line(string.format("STATUS:  %s", summary.status))
  add_line(
    string.format("FILES:   %d modified (+%d -%d)", summary.total_files, summary.total_added, summary.total_deleted)
  )
  add_line("========================================")
  add_line("")

  if #summary.files == 0 then
    add_line("  (no files modified yet)")
    add_line("")
  else
    for _, file in ipairs(summary.files) do
      local file_header = string.format("▾ %s (+%d -%d)", file.display_path, file.total_added, file.total_deleted)
      if file.is_new then
        file_header = string.format("▾ %s [new] (+%d -%d)", file.display_path, file.total_added, file.total_deleted)
      end
      local default_line = file.hunks[1] and file.hunks[1].start_line or 1
      add_line(file_header, { path = file.path, line = default_line, diff = file.diff })

      if file.conflicts ~= nil and #file.conflicts > 0 then
        add_line(string.format("  ⚠️  CONFLICT: also modified in: %s", table.concat(file.conflicts, ", ")))
      end

      if #file.hunks == 0 then
        add_line("    • (entire file touched)", { path = file.path, line = default_line, diff = file.diff })
      else
        for _, hunk in ipairs(file.hunks) do
          local range_desc
          if hunk.start_line == hunk.end_line then
            range_desc = string.format("    • L%d (+%d -%d)", hunk.start_line, hunk.added, hunk.deleted)
          else
            range_desc =
              string.format("    • L%d-%d (+%d -%d)", hunk.start_line, hunk.end_line, hunk.added, hunk.deleted)
          end
          add_line(range_desc, { path = file.path, line = hunk.start_line, diff = file.diff })
        end
      end
      add_line("")
    end
  end

  add_line("----------------------------------------")
  add_line("[<CR>] Jump to line   [d] Diff preview")
  add_line("[s] Switch chat        [r] Refresh")
  add_line("[q] Close overview")

  return lines, targets
end

---Check whether the overview tabpage is currently open.
---@return boolean
function M.is_open()
  if active_overview == nil then
    return false
  end
  return nvim.api.nvim_tabpage_is_valid(active_overview.tabpage)
end

---Close the overview tabpage and release listeners.
---@return boolean closed
function M.close()
  local current = active_overview
  active_overview = nil
  if current == nil then
    return false
  end

  for _, unsub in ipairs(current.unsubscribes) do
    pcall(unsub)
  end

  if nvim.api.nvim_tabpage_is_valid(current.tabpage) then
    pcall(function()
      if #nvim.api.nvim_list_tabpages() > 1 then
        nvim.api.nvim_set_current_tabpage(current.tabpage)
        nvim.cmd("tabclose")
      else
        for _, win in ipairs(current.windows) do
          if nvim.api.nvim_win_is_valid(win) and #nvim.api.nvim_tabpage_list_wins(current.tabpage) > 1 then
            pcall(nvim.api.nvim_win_close, win, true)
          end
        end
        for _, buf in ipairs(current.buffers) do
          if nvim.api.nvim_buf_is_valid(buf) then
            pcall(nvim.api.nvim_buf_delete, buf, { force = true })
          end
        end
      end
    end)
  end

  if current.previous_tabpage ~= nil and nvim.api.nvim_tabpage_is_valid(current.previous_tabpage) then
    pcall(nvim.api.nvim_set_current_tabpage, current.previous_tabpage)
  end

  return true
end

---Refresh the overview buffers in-place.
---@return boolean refreshed
function M.refresh()
  if not M.is_open() or active_overview == nil then
    return false
  end

  local chat = active_overview.chat
  local summaries = {}
  local views_or_sessions = {}

  if chat ~= nil and chat.views ~= nil then
    for _, session_id in ipairs(chat.view_order or {}) do
      local view = chat.views[session_id]
      if view ~= nil then
        views_or_sessions[#views_or_sessions + 1] = view
        summaries[#summaries + 1] = M.collect_session_summary(view)
      end
    end
  end

  M.detect_conflicts(summaries)

  for index, summary in ipairs(summaries) do
    local buf = active_overview.buffers[index]
    local win = active_overview.windows[index]
    if buf ~= nil and nvim.api.nvim_buf_is_valid(buf) then
      local lines, targets = M.render_session_buffer(summary)
      active_overview.line_targets[buf] = targets
      nvim.api.nvim_set_option_value("modifiable", true, { buf = buf })
      nvim.api.nvim_buf_set_lines(buf, 0, -1, false, lines)
      nvim.api.nvim_set_option_value("modifiable", false, { buf = buf })

      if win ~= nil and nvim.api.nvim_win_is_valid(win) then
        local bar = string.format(" %s · %s [%s] ", summary.name, summary.agent, summary.status)
        pcall(nvim.api.nvim_set_option_value, "winbar", bar, { win = win })
      end
    end
  end

  return true
end

---Open the multi-session overview tabpage with vertical windows for each session.
---@param source? louiselm.ui.Chat|louiselm.session.Session[]|table Chat or list of sessions
---@param options? louiselm.ui.SessionOverviewOptions
---@return boolean opened
---@return string? error_message
function M.open(source, options)
  options = options or {}
  local chat = options.chat

  -- Check if source is a Chat instance
  if source ~= nil and type(source) == "table" and source.views ~= nil then
    chat = source
  end

  local views_or_sessions = {}
  local summaries = {}

  if chat ~= nil and chat.views ~= nil then
    for _, session_id in ipairs(chat.view_order or {}) do
      local view = chat.views[session_id]
      if view ~= nil then
        views_or_sessions[#views_or_sessions + 1] = view
        summaries[#summaries + 1] = M.collect_session_summary(view, options.cwd)
      end
    end
  elseif type(source) == "table" and #source > 0 then
    for _, item in ipairs(source) do
      views_or_sessions[#views_or_sessions + 1] = item
      summaries[#summaries + 1] = M.collect_session_summary(item, options.cwd)
    end
  end

  if #summaries == 0 then
    return false, "no active sessions to display in overview"
  end

  if M.is_open() then
    M.refresh()
    if active_overview ~= nil and nvim.api.nvim_tabpage_is_valid(active_overview.tabpage) then
      nvim.api.nvim_set_current_tabpage(active_overview.tabpage)
    end
    return true
  end

  M.detect_conflicts(summaries)

  local previous_tabpage = nvim.api.nvim_get_current_tabpage()
  nvim.cmd("tabnew")
  local tabpage = nvim.api.nvim_get_current_tabpage()

  local windows = {}
  local buffers = {}
  local session_ids = {}
  local line_targets = {}
  local unsubscribes = {}

  for index, summary in ipairs(summaries) do
    local win
    if index == 1 then
      win = nvim.api.nvim_get_current_win()
    else
      nvim.cmd("rightbelow vsplit")
      win = nvim.api.nvim_get_current_win()
    end

    local buf = nvim.api.nvim_create_buf(false, true)
    nvim.api.nvim_win_set_buf(win, buf)

    nvim.api.nvim_set_option_value("buftype", "nofile", { buf = buf })
    nvim.api.nvim_set_option_value("bufhidden", "wipe", { buf = buf })
    nvim.api.nvim_set_option_value("swapfile", false, { buf = buf })

    local lines, targets = M.render_session_buffer(summary)
    line_targets[buf] = targets

    nvim.api.nvim_set_option_value("modifiable", true, { buf = buf })
    nvim.api.nvim_buf_set_lines(buf, 0, -1, false, lines)
    nvim.api.nvim_set_option_value("modifiable", false, { buf = buf })
    nvim.api.nvim_set_option_value("filetype", "louiselm_overview", { buf = buf })

    nvim.wo[win].wrap = false
    nvim.wo[win].cursorline = true
    nvim.wo[win].number = false
    nvim.wo[win].relativenumber = false
    nvim.wo[win].signcolumn = "no"

    local bar = string.format(" %s · %s [%s] ", summary.name, summary.agent, summary.status)
    pcall(nvim.api.nvim_set_option_value, "winbar", bar, { win = win })

    windows[#windows + 1] = win
    buffers[#buffers + 1] = buf
    session_ids[#session_ids + 1] = summary.session_id

    -- Register interactive buffer keymaps
    local captured_buf = buf
    local captured_session_id = summary.session_id

    nvim.keymap.set("n", "<CR>", function()
      local cursor = nvim.api.nvim_win_get_cursor(0)
      local target = line_targets[captured_buf] and line_targets[captured_buf][cursor[1]]
      if target ~= nil and target.path ~= nil then
        if previous_tabpage ~= nil and nvim.api.nvim_tabpage_is_valid(previous_tabpage) then
          nvim.api.nvim_set_current_tabpage(previous_tabpage)
        else
          nvim.cmd("tabnew")
        end
        nvim.cmd("edit " .. nvim.fn.fnameescape(target.path))
        local total_lines = nvim.api.nvim_buf_line_count(0)
        local safe_line = math.min(math.max(1, target.line), total_lines)
        nvim.api.nvim_win_set_cursor(0, { safe_line, 0 })
      end
    end, { buffer = captured_buf, silent = true, nowait = true, desc = "Jump to file and edition line" })

    nvim.keymap.set("n", "d", function()
      local cursor = nvim.api.nvim_win_get_cursor(0)
      local target = line_targets[captured_buf] and line_targets[captured_buf][cursor[1]]
      if target ~= nil and target.diff ~= nil and target.diff ~= "" then
        local preview = Apply.preview({ path = target.path, diff = target.diff })
        if preview ~= nil then
          DiffBuffer.open(preview, { focus = true })
        else
          local original = Apply.read(target.path) or ""
          DiffBuffer.open({
            path = target.path,
            original = original,
            proposed = original,
            diff = target.diff,
          }, { focus = true })
        end
      end
    end, { buffer = captured_buf, silent = true, nowait = true, desc = "Preview diff for file" })

    nvim.keymap.set("n", "s", function()
      if chat ~= nil and type(chat.switch) == "function" then
        M.close()
        chat:switch(captured_session_id)
      end
    end, { buffer = captured_buf, silent = true, nowait = true, desc = "Switch to this session in chat" })

    nvim.keymap.set("n", "r", function()
      M.refresh()
    end, { buffer = captured_buf, silent = true, nowait = true, desc = "Refresh multi-session overview" })

    nvim.keymap.set("n", "q", function()
      M.close()
    end, { buffer = captured_buf, silent = true, nowait = true, desc = "Close multi-session overview" })

    nvim.keymap.set("n", "<Esc>", function()
      M.close()
    end, { buffer = captured_buf, silent = true, nowait = true, desc = "Close multi-session overview" })

    -- Listen to session events for live updates
    local session_obj = (views_or_sessions[index].session or views_or_sessions[index])
    if session_obj ~= nil and type(session_obj.on) == "function" then
      local unsub = session_obj:on(function(event)
        if
          event.type == "tool_call_finished"
          or event.type == "permission_requested"
          or event.type == "turn_done"
          or event.type == "state_changed"
        then
          nvim.schedule(function()
            M.refresh()
          end)
        end
      end)
      unsubscribes[#unsubscribes + 1] = unsub
    end
  end

  -- Equalize widths across all vertical splits
  nvim.cmd("wincmd =")
  if #windows > 0 and nvim.api.nvim_win_is_valid(windows[1]) then
    nvim.api.nvim_set_current_win(windows[1])
  end

  active_overview = {
    tabpage = tabpage,
    windows = windows,
    buffers = buffers,
    session_ids = session_ids,
    line_targets = line_targets,
    previous_tabpage = previous_tabpage,
    unsubscribes = unsubscribes,
    chat = chat,
  }

  return true
end

return M
