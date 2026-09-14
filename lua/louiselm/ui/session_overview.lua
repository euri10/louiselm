---Current-Session sidebar showing modified files, edition lines, diff stats, and conflict alerts.

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

---@class louiselm.ui.OverviewState
---@field window integer Sidebar window.
---@field buffer integer Sidebar buffer.
---@field session_id string Subject Session, retained while the sidebar has focus.
---@field return_window integer Window from which the sidebar was opened.
---@field line_targets table<integer, louiselm.ui.LineTarget>
---@field unsubscribes fun()[]
---@field augroup integer Window/buffer cleanup observers.
---@field preview_buffer? integer Diff preview closed with the sidebar.

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

  add_line(string.format("SESSION: %s (%s)", summary.name, summary.agent))
  add_line(string.format("STATUS:  %s", summary.status))
  add_line(
    string.format("FILES:   %d modified (+%d -%d)", summary.total_files, summary.total_added, summary.total_deleted)
  )
  add_line("CWD:     " .. summary.working_dir)
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

  add_line("[<CR>] Open file  [d] Diff")
  add_line("[s] Chat  [r] Refresh  [q] Close")

  return lines, targets
end

---Check whether this chat's sidebar is visible.
---@param chat louiselm.ui.Chat
---@return boolean
function M.is_open(chat)
  local current = chat.overview
  return current ~= nil
    and nvim.api.nvim_win_is_valid(current.window)
    and nvim.api.nvim_buf_is_valid(current.buffer)
    and nvim.api.nvim_win_get_buf(current.window) == current.buffer
end

---Close this chat's sidebar and release its observers; false if already closed.
---@param chat louiselm.ui.Chat
---@return boolean closed
function M.close(chat)
  local current = chat.overview
  if current == nil then
    return false
  end
  chat.overview = nil
  nvim.api.nvim_del_augroup_by_id(current.augroup)
  for _, unsubscribe in ipairs(current.unsubscribes) do
    unsubscribe()
  end
  if current.preview_buffer ~= nil then
    DiffBuffer.close(current.preview_buffer)
  end
  local focused = nvim.api.nvim_get_current_win() == current.window
  if
    nvim.api.nvim_win_is_valid(current.window)
    and nvim.api.nvim_win_get_buf(current.window) == current.buffer
    and #nvim.api.nvim_tabpage_list_wins(nvim.api.nvim_win_get_tabpage(current.window)) > 1
  then
    nvim.api.nvim_win_close(current.window, true)
  end
  if nvim.api.nvim_buf_is_valid(current.buffer) then
    nvim.api.nvim_buf_delete(current.buffer, { force = true })
  end
  if focused and nvim.api.nvim_win_is_valid(current.return_window) then
    nvim.api.nvim_set_current_win(current.return_window)
  end
  return true
end

---Refresh the subject Session in place; close if its view was removed or disposed.
---@param chat louiselm.ui.Chat
---@return boolean refreshed False when no live subject/sidebar remains.
function M.refresh(chat)
  local current = chat.overview
  if current == nil then
    return false
  end
  local view = chat.views[current.session_id]
  if chat.disposed or not M.is_open(chat) or view == nil or view.session:inspect().status == "disposed" then
    M.close(chat)
    return false
  end

  local summaries = {}
  local subject
  for _, id in ipairs(chat.view_order) do
    local candidate = chat.views[id]
    if candidate ~= nil then
      local summary = M.collect_session_summary(candidate)
      summaries[#summaries + 1] = summary
      if id == current.session_id then
        subject = summary
      end
    end
  end
  if subject == nil then
    M.close(chat)
    return false
  end
  M.detect_conflicts(summaries)
  local lines, targets = M.render_session_buffer(subject)
  current.line_targets = targets
  nvim.api.nvim_set_option_value("modifiable", true, { buf = current.buffer })
  nvim.api.nvim_buf_set_lines(current.buffer, 0, -1, false, lines)
  nvim.api.nvim_set_option_value("modifiable", false, { buf = current.buffer })
  return true
end

---@param current louiselm.ui.OverviewState
---@return louiselm.ui.LineTarget?
local function cursor_target(current)
  return current.line_targets[nvim.api.nvim_win_get_cursor(current.window)[1]]
end

---@param current louiselm.ui.OverviewState
local function jump_to_file(current)
  local target = cursor_target(current)
  if target == nil then
    return
  end
  local destination
  for _, window in ipairs(nvim.api.nvim_tabpage_list_wins(0)) do
    local buffer = nvim.api.nvim_win_get_buf(window)
    if nvim.bo[buffer].buftype == "" and nvim.api.nvim_win_get_config(window).relative == "" then
      destination = window
      break
    end
  end
  local ok, err = pcall(function()
    if destination ~= nil then
      nvim.api.nvim_set_current_win(destination)
      nvim.api.nvim_cmd({ cmd = "edit", args = { target.path } }, {})
    else
      nvim.api.nvim_cmd({ cmd = "vsplit", args = { target.path }, mods = { split = "botright" } }, {})
    end
    local line = math.min(math.max(1, target.line), nvim.api.nvim_buf_line_count(0))
    nvim.api.nvim_win_set_cursor(0, { line, 0 })
  end)
  if not ok then
    nvim.notify("louiselm: could not open overview file: " .. tostring(err), nvim.log.levels.ERROR)
  end
end

---Open or reuse a left sidebar for the invoking Session, without replacing chat windows.
---@param chat louiselm.ui.Chat Owner and source of attached Sessions.
---@return boolean opened
---@return string? error_message Missing/disposed chat or Session.
function M.open(chat)
  if chat.disposed then
    return false, "chat UI is disposed"
  end
  local window = nvim.api.nvim_get_current_win()
  local buffer = nvim.api.nvim_get_current_buf()
  local current = chat.overview
  local session_id = current ~= nil and window == current.window and current.session_id or chat.current_id
  -- Window focus can change without Chat:switch() when several chats are visible.
  for id, view in pairs(chat.views) do
    if view.renderer.buffer == buffer then
      session_id = id
      break
    end
  end
  local view = session_id and chat.views[session_id]
  if session_id == nil or view == nil then
    return false, "no chat session is attached"
  end
  if view.session:inspect().status == "disposed" then
    return false, "session is disposed"
  end

  nvim.cmd.stopinsert()
  if
    current ~= nil
    and (not M.is_open(chat) or nvim.api.nvim_win_get_tabpage(current.window) ~= nvim.api.nvim_get_current_tabpage())
  then
    M.close(chat)
    current = nil
  end
  if current == nil then
    local sidebar_buffer = nvim.api.nvim_create_buf(false, true)
    local opened, sidebar_window =
      pcall(nvim.api.nvim_open_win, sidebar_buffer, true, { split = "left", win = -1, width = 44 })
    if not opened then
      nvim.api.nvim_buf_delete(sidebar_buffer, { force = true })
      return false, "could not open Session overview: " .. tostring(sidebar_window)
    end
    nvim.bo[sidebar_buffer].bufhidden = "wipe"
    nvim.bo[sidebar_buffer].filetype = "louiselm_overview"
    nvim.wo[sidebar_window].wrap = true
    nvim.wo[sidebar_window].linebreak = true
    nvim.wo[sidebar_window].cursorline = true
    nvim.wo[sidebar_window].number = false
    nvim.wo[sidebar_window].relativenumber = false
    nvim.wo[sidebar_window].signcolumn = "no"
    nvim.wo[sidebar_window].foldcolumn = "0"
    nvim.wo[sidebar_window].winfixwidth = true
    nvim.wo[sidebar_window].winbar = " Session Overview "
    current = {
      window = sidebar_window,
      buffer = sidebar_buffer,
      session_id = session_id,
      return_window = window,
      line_targets = {},
      unsubscribes = {},
      augroup = nvim.api.nvim_create_augroup("LouiselmOverview" .. sidebar_buffer, { clear = true }),
    }
    chat.overview = current
    local sidebar = current
    local function cleanup()
      nvim.schedule(function()
        if chat.overview == sidebar and not M.is_open(chat) then
          M.close(chat)
        end
      end)
    end
    nvim.api.nvim_create_autocmd("WinClosed", {
      group = sidebar.augroup,
      pattern = tostring(sidebar.window),
      callback = cleanup,
    })
    nvim.api.nvim_create_autocmd({ "BufWipeout", "BufWinLeave" }, {
      group = sidebar.augroup,
      buffer = sidebar.buffer,
      callback = cleanup,
    })
    local function map(lhs, callback, desc)
      nvim.keymap.set("n", lhs, callback, { buffer = sidebar.buffer, silent = true, nowait = true, desc = desc })
    end
    map("<CR>", function()
      jump_to_file(sidebar)
    end, "Open file at edition line")
    map("d", function()
      local target = cursor_target(sidebar)
      if target ~= nil and target.diff ~= nil and target.diff ~= "" then
        local preview = Apply.preview({ path = target.path, diff = target.diff })
        if preview == nil then
          local original = Apply.read(target.path) or ""
          preview = { path = target.path, original = original, proposed = original, diff = target.diff }
        end
        if sidebar.preview_buffer ~= nil then
          DiffBuffer.close(sidebar.preview_buffer)
        end
        local preview_buffer, err = DiffBuffer.open(preview, { focus = false })
        if preview_buffer == nil then
          nvim.notify("louiselm: " .. tostring(err), nvim.log.levels.ERROR)
          return
        end
        sidebar.preview_buffer = preview_buffer
        local width = math.max(1, math.min(100, nvim.o.columns - 4))
        local height = math.max(1, math.min(30, nvim.o.lines - 4))
        nvim.api.nvim_open_win(preview_buffer, true, {
          relative = "editor",
          row = math.floor((nvim.o.lines - height) / 2),
          col = math.floor((nvim.o.columns - width) / 2),
          width = width,
          height = height,
          style = "minimal",
          border = "rounded",
          title = " Session diff ",
        })
      end
    end, "Preview file diff")
    map("s", function()
      local subject = chat.views[sidebar.session_id]
      if subject ~= nil then
        local host = subject.renderer.window
        if host ~= nil and nvim.api.nvim_win_is_valid(host) and host ~= sidebar.window then
          nvim.api.nvim_set_current_win(host)
        else
          M.close(chat)
        end
        local _, err = chat:switch(sidebar.session_id)
        if err ~= nil then
          nvim.notify("louiselm: " .. err, nvim.log.levels.ERROR)
        end
      end
    end, "Focus Session chat")
    map("r", function()
      M.refresh(chat)
    end, "Refresh Session overview")
    map("q", function()
      M.close(chat)
    end, "Close Session overview")
    map("<Esc>", function()
      M.close(chat)
    end, "Close Session overview")
  end

  current.session_id = session_id
  if window ~= current.window then
    current.return_window = window
  end
  for _, unsubscribe in ipairs(current.unsubscribes) do
    unsubscribe()
  end
  current.unsubscribes = {}
  local sidebar = current
  for _, candidate in pairs(chat.views) do
    current.unsubscribes[#current.unsubscribes + 1] = candidate.session:on(function(event)
      if
        event.type == "tool_call_finished"
        or event.type == "permission_requested"
        or event.type == "turn_done"
        or event.type == "state_changed"
      then
        nvim.schedule(function()
          if chat.overview == sidebar then
            M.refresh(chat)
          end
        end)
      end
    end)
  end
  M.refresh(chat)
  nvim.api.nvim_set_current_win(current.window)
  return true
end

return M
