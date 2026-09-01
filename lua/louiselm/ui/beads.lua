---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local M = {}

---@class louiselm.ui.BeadsInspectorOptions
---@field cwd? string Working directory used to locate the Beads workspace.
---@field is_active? fun(): boolean Whether the requesting chat buffer still belongs to an active chat.
---@field on_error? fun(message: string) Receives expected lookup failures.
---@field sibling_roots? string[] Directories globbed one level for a sibling `.beads/beads.db`, tried in order when a cursor token looks like a foreign workspace's issue ID.

---@class louiselm.ui.BeadsIssue
---@field id string
---@field title string
---@field status string
---@field priority integer
---@field labels string[]
---@field description string

---@class louiselm.ui.BeadsModule
---@field inspect fun(buffer: integer, options?: louiselm.ui.BeadsInspectorOptions): boolean, string? Inspect the Beads issue under the cursor or prompt for one. Returns an error before the asynchronous lookup starts.

---@param value string
---@param prefix string
---@return boolean valid
local function valid_issue_id(value, prefix)
  local escaped_prefix = prefix:gsub("([^%w])", "%%%1")
  local pattern = "^" .. escaped_prefix .. "%-[a-z0-9]"
  return value:match(pattern .. "$") ~= nil or value:match(pattern .. "[a-z0-9%.%-]*[a-z0-9]$") ~= nil
end

---@param line string
---@param column integer Zero-based byte column.
---@param prefix string
---@return string? issue_id
local function issue_id_at_cursor(line, column, prefix)
  local found
  local count = 0
  local search_start = 1
  local escaped_prefix = prefix:gsub("([^%w])", "%%%1")
  while true do
    local start_index, end_index = line:find(escaped_prefix .. "%-[a-z0-9][a-z0-9%.%-]*", search_start)
    if start_index == nil then
      break
    end
    local issue_id = line:sub(start_index, end_index):gsub("[%.%-]+$", "")
    end_index = start_index + #issue_id - 1
    if valid_issue_id(issue_id, prefix) then
      count = count + 1
      if start_index <= column + 1 and column + 1 <= end_index then
        found = issue_id
      end
    end
    search_start = end_index + 1
  end
  return count == 1 and found or nil
end

---@param value unknown
---@param prefix? string When given, `issue.id` must match this workspace's ID shape. Omit for a foreign workspace whose prefix is unknown; callers must verify `issue.id` themselves in that case.
---@return louiselm.ui.BeadsIssue? issue
local function decode_issue(value, prefix)
  if type(value) ~= "table" or type(value[1]) ~= "table" then
    return nil
  end
  local issue = value[1]
  if
    type(issue.id) ~= "string"
    or (prefix ~= nil and not valid_issue_id(issue.id, prefix))
    or type(issue.title) ~= "string"
    or type(issue.status) ~= "string"
    or type(issue.priority) ~= "number"
    or issue.priority < 0
    or issue.priority > 4
    or issue.priority % 1 ~= 0
    or (issue.labels ~= nil and type(issue.labels) ~= "table")
    or (issue.description ~= nil and type(issue.description) ~= "string")
  then
    return nil
  end
  local labels = {}
  for index, label in ipairs(issue.labels or {}) do
    if type(label) ~= "string" then
      return nil
    end
    labels[index] = label
  end
  return {
    id = issue.id,
    title = issue.title,
    status = issue.status,
    priority = issue.priority,
    labels = labels,
    description = issue.description or "",
  }
end

---@param issue louiselm.ui.BeadsIssue
---@return string[] lines
local function issue_lines(issue)
  local lines = {
    "# " .. issue.title,
    "",
    "ID: " .. issue.id,
    "Status: " .. issue.status,
    "Priority: " .. issue.priority,
    "Labels: " .. (#issue.labels > 0 and table.concat(issue.labels, ", ") or "none"),
    "",
  }
  nvim.list_extend(lines, nvim.split(issue.description, "\n", { plain = true }))
  return lines
end

---@param issue louiselm.ui.BeadsIssue
---@return boolean opened
---@return string? error_message
local function open_issue(issue)
  local buffer = nvim.api.nvim_create_buf(false, true)
  local lines = issue_lines(issue)
  local opened, error_message = pcall(function()
    nvim.api.nvim_buf_set_name(buffer, "louiselm://beads/" .. issue.id)
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
      title = " Beads ",
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
  nvim.keymap.set("n", "q", close, { buffer = buffer, silent = true, nowait = true, desc = "Close Beads issue" })
  nvim.keymap.set("n", "<Esc>", close, { buffer = buffer, silent = true, nowait = true, desc = "Close Beads issue" })
  return true
end

---@param options louiselm.ui.BeadsInspectorOptions
---@param message string
local function report_error(options, message)
  if options.on_error ~= nil then
    options.on_error(message)
  end
end

---@param buffer integer
---@return string? line
---@return integer column Zero-based byte column.
local function cursor_line(buffer)
  local cursor = nvim.api.nvim_win_get_cursor(0)
  local line = nvim.api.nvim_buf_get_lines(buffer, cursor[1] - 1, cursor[1], false)[1]
  return type(line) == "string" and line or nil, cursor[2]
end

---@param buffer integer
---@return string? issue_id
local function current_issue_id(buffer, prefix)
  local line, column = cursor_line(buffer)
  return line ~= nil and issue_id_at_cursor(line, column, prefix) or nil
end

---Find the raw alnum/dot/hyphen token spanning the cursor, regardless of
---whether it already has a workspace prefix.
---@param line string
---@param column integer Zero-based byte column.
---@return string? token
local function bare_token_at_cursor(line, column)
  local search_start = 1
  while true do
    local start_index, end_index = line:find("[a-z0-9][a-z0-9%.%-]*", search_start)
    if start_index == nil then
      return nil
    end
    local token = line:sub(start_index, end_index):gsub("[%.%-]+$", "")
    end_index = start_index + #token - 1
    if token ~= "" and start_index <= column + 1 and column + 1 <= end_index then
      return token
    end
    search_start = start_index + math.max(#token, 1)
  end
end

---The raw token under the cursor, excluding one that already has the local
---workspace prefix -- that shape is either already handled by
---`current_issue_id` or ambiguous, and must not be re-guessed here.
---@param buffer integer
---@param prefix string
---@return string? token
local function candidate_token_at_cursor(buffer, prefix)
  local line, column = cursor_line(buffer)
  if line == nil then
    return nil
  end
  local token = bare_token_at_cursor(line, column)
  if token == nil or valid_issue_id(token, prefix) then
    return nil
  end
  return token
end

---Run `br` and decode a single-issue `show` response.
---@param cmd string[] Full `br` invocation, e.g. `{"br", "show", id, "--json"}`.
---@param prefix? string Omit when the workspace targeted by `cmd` (e.g. via `--db`) has an unknown prefix; the caller must verify the returned `issue.id` itself.
---@param options louiselm.ui.BeadsInspectorOptions
---@param callback fun(issue: louiselm.ui.BeadsIssue?, miss_reason: "not_found"|"malformed"|nil)
---@return boolean started
---@return string? error_message
local function run_show(cmd, prefix, options, callback)
  local started = pcall(nvim.system, cmd, {
    text = true,
    cwd = options.cwd or nvim.fn.getcwd(),
  }, function(result)
    nvim.schedule(function()
      if options.is_active ~= nil and not options.is_active() then
        return
      end
      if result.code ~= 0 then
        callback(nil, "not_found")
        return
      end
      local decoded, value_or_error = pcall(nvim.json.decode, result.stdout)
      local issue = decoded and decode_issue(value_or_error, prefix) or nil
      if issue == nil then
        callback(nil, "malformed")
        return
      end
      callback(issue, nil)
    end)
  end)
  if not started then
    return false, "could not start br"
  end
  return true
end

---@param issue_id string
---@param prefix string
---@param options louiselm.ui.BeadsInspectorOptions
---@return boolean started
---@return string? error_message
local function show_issue(issue_id, prefix, options)
  return run_show({ "br", "show", issue_id, "--json" }, prefix, options, function(issue, miss_reason)
    if miss_reason == "not_found" then
      report_error(options, "could not read Beads issue " .. issue_id)
      return
    end
    if issue == nil or issue.id ~= issue_id then
      report_error(options, "br returned malformed issue data")
      return
    end
    local opened, open_error = open_issue(issue)
    if not opened then
      report_error(options, "could not display Beads issue: " .. (open_error or "unknown error"))
    end
  end)
end

---One level under each root, looking for a sibling `.beads/beads.db`. No
---caching, no recursion: recomputed fresh on every call.
---@param roots string[]
---@return string[] db_paths
local function sibling_db_paths(roots)
  local db_paths = {}
  for _, root in ipairs(roots) do
    local expanded = nvim.fn.expand(root)
    for _, entry in ipairs(nvim.fn.glob(expanded .. "/*/.beads/beads.db", false, true)) do
      db_paths[#db_paths + 1] = entry
    end
  end
  return db_paths
end

---Try a full `<prefix>-<suffix>`-shaped token against each configured
---sibling workspace's own database, in order, stopping at the first hit.
---Cheap and speculative like the bare-suffix path: any miss (including all
---siblings exhausted) calls `on_miss` instead of reporting an error.
---@param token string
---@param options louiselm.ui.BeadsInspectorOptions
---@param on_miss fun()
local function try_sibling_ids(token, options, on_miss)
  local db_paths = sibling_db_paths(options.sibling_roots or {})
  local index = 0
  local function try_next()
    index = index + 1
    local db_path = db_paths[index]
    if db_path == nil then
      on_miss()
      return
    end
    local _, lookup_error = run_show({ "br", "--db", db_path, "show", token, "--json" }, nil, options, function(issue)
      if issue == nil or issue.id ~= token then
        try_next()
        return
      end
      local opened, open_error = open_issue(issue)
      if not opened then
        report_error(options, "could not display Beads issue: " .. (open_error or "unknown error"))
      end
    end)
    if lookup_error ~= nil then
      report_error(options, lookup_error)
    end
  end
  try_next()
end

---@param options louiselm.ui.BeadsInspectorOptions
---@param callback fun(prefix: string)
---@return boolean started
---@return string? error_message
local function discover_prefix(options, callback)
  if nvim.fn.executable("br") ~= 1 then
    return false, "br is not available"
  end
  local started = pcall(nvim.system, { "br", "where", "--json" }, {
    text = true,
    cwd = options.cwd or nvim.fn.getcwd(),
  }, function(result)
    nvim.schedule(function()
      if options.is_active ~= nil and not options.is_active() then
        return
      end
      if result.code ~= 0 then
        report_error(options, "could not locate Beads workspace")
        return
      end
      local decoded, value_or_error = pcall(nvim.json.decode, result.stdout)
      local prefix = decoded and type(value_or_error) == "table" and value_or_error.prefix or nil
      if
        type(prefix) ~= "string"
        or not prefix:match("^[a-z0-9][a-z0-9%-]*[a-z0-9]$") and not prefix:match("^[a-z0-9]$")
      then
        report_error(options, "br returned malformed workspace data")
        return
      end
      callback(prefix)
    end)
  end)
  if not started then
    return false, "could not start br"
  end
  return true
end

---@param prefix string
---@param options louiselm.ui.BeadsInspectorOptions
local function prompt_for_issue_id(prefix, options)
  nvim.ui.input({ prompt = prefix .. " Beads issue id: " }, function(value)
    if value == nil or (options.is_active ~= nil and not options.is_active()) then
      return
    end
    local prompted_id = nvim.trim(value)
    if not prompted_id:match("^" .. prefix:gsub("([^%w])", "%%%1") .. "%-") then
      prompted_id = prefix .. "-" .. prompted_id
    end
    if not valid_issue_id(prompted_id, prefix) then
      report_error(options, "Beads issue id must start with " .. prefix .. "-")
      return
    end
    local _, lookup_error = show_issue(prompted_id, prefix, options)
    if lookup_error ~= nil then
      report_error(options, lookup_error)
    end
  end)
end

---@param buffer integer
---@param prefix string
---@param options louiselm.ui.BeadsInspectorOptions
local function inspect_with_prefix(buffer, prefix, options)
  local issue_id = current_issue_id(buffer, prefix)
  if issue_id ~= nil then
    local _, lookup_error = show_issue(issue_id, prefix, options)
    if lookup_error ~= nil then
      report_error(options, lookup_error)
    end
    return
  end

  local token = candidate_token_at_cursor(buffer, prefix)
  if token ~= nil and token:find("-", 1, true) ~= nil then
    -- Already has some prefix shape, just not this workspace's -- try
    -- configured siblings before giving up.
    try_sibling_ids(token, options, function()
      prompt_for_issue_id(prefix, options)
    end)
    return
  end

  local bare_id = token ~= nil and prefix .. "-" .. token or nil
  if bare_id == nil or not valid_issue_id(bare_id, prefix) then
    prompt_for_issue_id(prefix, options)
    return
  end
  -- Speculative: any bare word under the cursor is tried, so a miss (not
  -- found or malformed) falls through to the manual prompt instead of
  -- reporting an error -- unlike show_issue's confident exact-prefix callers.
  local _, lookup_error = run_show({ "br", "show", bare_id, "--json" }, prefix, options, function(issue)
    if issue == nil or issue.id ~= bare_id then
      prompt_for_issue_id(prefix, options)
      return
    end
    local opened, open_error = open_issue(issue)
    if not opened then
      report_error(options, "could not display Beads issue: " .. (open_error or "unknown error"))
    end
  end)
  if lookup_error ~= nil then
    report_error(options, lookup_error)
  end
end

---Inspect the Beads issue under the cursor in a LouiseLM Session buffer.
---@param buffer integer Source chat buffer.
---@param options? louiselm.ui.BeadsInspectorOptions Lookup and lifecycle callbacks.
---@return boolean started Whether a lookup started or an ID prompt was opened.
---@return string? error_message Why inspection could not start.
function M.inspect(buffer, options)
  if type(buffer) ~= "number" or not nvim.api.nvim_buf_is_valid(buffer) then
    return false, "chat buffer is unavailable"
  end
  if nvim.api.nvim_get_current_buf() ~= buffer or nvim.bo[buffer].filetype ~= "louiselm-session" then
    return false, "current buffer is not a louiselm session"
  end
  if options ~= nil and type(options) ~= "table" then
    return false, "Beads inspector options must be a table"
  end
  options = options or {}
  if options.cwd ~= nil and (type(options.cwd) ~= "string" or options.cwd == "") then
    return false, "Beads inspector cwd must be a non-empty string"
  end
  if options.is_active ~= nil and type(options.is_active) ~= "function" then
    return false, "Beads inspector is_active must be a function"
  end
  if options.on_error ~= nil and type(options.on_error) ~= "function" then
    return false, "Beads inspector on_error must be a function"
  end
  if options.sibling_roots ~= nil and type(options.sibling_roots) ~= "table" then
    return false, "Beads inspector sibling_roots must be a table"
  end
  local started, lookup_error = discover_prefix(options, function(prefix)
    inspect_with_prefix(buffer, prefix, options)
  end)
  if not started then
    return false, lookup_error
  end
  return true
end

return M
