---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local M = {}

---@class louiselm.ui.BeadsInspectorOptions
---@field cwd? string Working directory used to locate the Beads workspace.
---@field is_active? fun(): boolean Whether the requesting chat buffer still belongs to an active chat.
---@field on_error? fun(message: string) Receives expected lookup failures.

---@class louiselm.ui.BeadsIssue
---@field id string
---@field title string
---@field status string
---@field priority integer
---@field labels string[]
---@field description string

---@class louiselm.ui.BeadsModule
---@field inspect fun(buffer: integer, options?: louiselm.ui.BeadsInspectorOptions): boolean, string? Inspect the Beads issue under the cursor or prompt for one. Returns an error before the asynchronous lookup starts.

local function valid_issue_id(value)
  return value:match("^louiselm%-[a-z0-9]$") ~= nil or value:match("^louiselm%-[a-z0-9][a-z0-9%.%-]*[a-z0-9]$") ~= nil
end

---@param line string
---@param column integer Zero-based byte column.
---@return string? issue_id
local function issue_id_at_cursor(line, column)
  local found
  local count = 0
  local search_start = 1
  while true do
    local start_index, end_index = line:find("louiselm%-[a-z0-9][a-z0-9%.%-]*", search_start)
    if start_index == nil then
      break
    end
    local issue_id = line:sub(start_index, end_index):gsub("[%.%-]+$", "")
    end_index = start_index + #issue_id - 1
    if valid_issue_id(issue_id) then
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
---@return louiselm.ui.BeadsIssue? issue
local function decode_issue(value)
  if type(value) ~= "table" or type(value[1]) ~= "table" then
    return nil
  end
  local issue = value[1]
  if
    type(issue.id) ~= "string"
    or not valid_issue_id(issue.id)
    or type(issue.title) ~= "string"
    or type(issue.status) ~= "string"
    or type(issue.priority) ~= "number"
    or issue.priority < 0
    or issue.priority > 4
    or issue.priority % 1 ~= 0
    or (issue.labels ~= nil and type(issue.labels) ~= "table")
    or type(issue.description) ~= "string"
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
    description = issue.description,
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
---@return string? issue_id
local function current_issue_id(buffer)
  local cursor = nvim.api.nvim_win_get_cursor(0)
  local line = nvim.api.nvim_buf_get_lines(buffer, cursor[1] - 1, cursor[1], false)[1]
  return type(line) == "string" and issue_id_at_cursor(line, cursor[2]) or nil
end

---@param issue_id string
---@param options louiselm.ui.BeadsInspectorOptions
---@return boolean started
---@return string? error_message
local function show_issue(issue_id, options)
  if nvim.fn.executable("br") ~= 1 then
    return false, "br is not available"
  end
  local started = pcall(nvim.system, { "br", "show", issue_id, "--json" }, {
    text = true,
    cwd = options.cwd or nvim.fn.getcwd(),
  }, function(result)
    nvim.schedule(function()
      if options.is_active ~= nil and not options.is_active() then
        return
      end
      if result.code ~= 0 then
        report_error(options, "could not read Beads issue " .. issue_id)
        return
      end
      local decoded, value_or_error = pcall(nvim.json.decode, result.stdout)
      local issue = decoded and decode_issue(value_or_error) or nil
      if issue == nil or issue.id ~= issue_id then
        report_error(options, "br returned malformed issue data")
        return
      end
      local opened, open_error = open_issue(issue)
      if not opened then
        report_error(options, "could not display Beads issue: " .. (open_error or "unknown error"))
      end
    end)
  end)
  if not started then
    return false, "could not start br"
  end
  return true
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
  local issue_id = current_issue_id(buffer)
  if issue_id ~= nil then
    return show_issue(issue_id, options)
  end
  nvim.ui.input({ prompt = "louiselm Beads issue id: " }, function(value)
    if value == nil or (options.is_active ~= nil and not options.is_active()) then
      return
    end
    local prompted_id = nvim.trim(value)
    if not valid_issue_id(prompted_id) then
      report_error(options, "Beads issue id must start with louiselm-")
      return
    end
    local _, lookup_error = show_issue(prompted_id, options)
    if lookup_error ~= nil then
      report_error(options, lookup_error)
    end
  end)
  return true
end

return M
