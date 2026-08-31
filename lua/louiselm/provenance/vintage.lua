---@class louiselm.provenance.VintageIssue
---@field id string Beads issue identifier.
---@field [string] unknown Beads issue fields, preserved from the JSONL snapshot.

---@class louiselm.provenance.VintageDiff
---@field added string[] Issue IDs present only in the current snapshot.
---@field removed string[] Issue IDs present only in the historical snapshot.
---@field changed string[] Issue IDs whose records differ between snapshots.

---@class louiselm.provenance.Vintage
---@field issues louiselm.provenance.VintageIssue[] Historical issue records.
---@field current louiselm.provenance.VintageIssue[] Current issue records.
---@field diff louiselm.provenance.VintageDiff Difference from historical to current.

---@class louiselm.provenance.VintageError: louiselm.provenance.Error
---@field detail? string Bounded process or filesystem detail.
---@field exit_code? integer Git exit code, when available.

local M = {}

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

---@param code string
---@param message string
---@param detail? string
---@param exit_code? integer
---@return louiselm.provenance.VintageError
local function make_error(code, message, detail, exit_code)
  return { code = code, message = message, detail = detail, exit_code = exit_code }
end

---Parse a Beads JSONL snapshot without filtering issue statuses.
---@param output string JSON object per line.
---@return louiselm.provenance.VintageIssue[]? issues
---@return louiselm.provenance.VintageError? error_value
function M.parse_issues(output)
  if type(output) ~= "string" then
    return nil, make_error("invalid_snapshot", "historical Beads snapshot must be a string")
  end
  local issues = {}
  local by_id = {}
  for line in (output .. "\n"):gmatch("(.-)\n") do
    line = nvim.trim(line)
    if line ~= "" then
      local decoded_ok, value = pcall(nvim.json.decode, line)
      if not decoded_ok or type(value) ~= "table" or type(value.id) ~= "string" or value.id == "" then
        return nil, make_error("invalid_snapshot", "historical Beads snapshot contains a malformed issue")
      end
      if by_id[value.id] ~= nil then
        return nil, make_error("invalid_snapshot", "historical Beads snapshot contains duplicate issue " .. value.id)
      end
      by_id[value.id] = value
      issues[#issues + 1] = value
    end
  end
  return issues, nil
end

---@param issues louiselm.provenance.VintageIssue[]
---@return table<string, louiselm.provenance.VintageIssue>
local function index(issues)
  local result = {}
  for _, issue in ipairs(issues) do
    result[issue.id] = issue
  end
  return result
end

---@param values string[]
local function sort_ids(values)
  table.sort(values)
end

---Compare a historical snapshot with the current Beads snapshot.
---@param historical louiselm.provenance.VintageIssue[]
---@param current louiselm.provenance.VintageIssue[]
---@return louiselm.provenance.VintageDiff difference
function M.diff(historical, current)
  local old_by_id, current_by_id = index(historical), index(current)
  local difference = { added = {}, removed = {}, changed = {} }
  for id, issue in pairs(current_by_id) do
    if old_by_id[id] == nil then
      difference.added[#difference.added + 1] = id
    elseif not nvim.deep_equal(old_by_id[id], issue) then
      difference.changed[#difference.changed + 1] = id
    end
  end
  for id in pairs(old_by_id) do
    if current_by_id[id] == nil then
      difference.removed[#difference.removed + 1] = id
    end
  end
  sort_ids(difference.added)
  sort_ids(difference.removed)
  sort_ids(difference.changed)
  return difference
end

---@param cwd string
---@param ref string
---@param callback fun(vintage: louiselm.provenance.Vintage?, error_value: louiselm.provenance.VintageError?)
---@return boolean started
---@return louiselm.provenance.VintageError? error_value
function M.load(cwd, ref, callback)
  if type(cwd) ~= "string" or cwd == "" then
    return false, make_error("invalid_cwd", "historical backlog requires a non-empty cwd")
  end
  if type(ref) ~= "string" or ref == "" or ref:match("^%-") or ref:find("[%z%s]") ~= nil then
    return false, make_error("invalid_ref", "historical backlog requires a valid git ref")
  end
  if type(callback) ~= "function" then
    return false, make_error("invalid_callback", "historical backlog requires a callback")
  end

  local path = nvim.fs.joinpath(cwd, ".beads", "issues.jsonl")
  local stat = nvim.uv.fs_stat(path)
  if stat == nil then
    return false, make_error("current_snapshot_missing", "current Beads snapshot is missing: " .. path)
  end
  local current, current_error = M.parse_issues(table.concat(nvim.fn.readfile(path), "\n"))
  if current == nil then
    return false, current_error
  end

  local call_ok, handle_or_error = pcall(
    nvim.system,
    { "git", "show", ref .. ":.beads/issues.jsonl" },
    { cwd = cwd, text = true },
    function(result)
      nvim.schedule(function()
        if result.code ~= 0 then
          local detail = nvim.trim(result.stderr or "")
          callback(
            nil,
            make_error(
              "historical_snapshot_missing",
              "git ref has no Beads snapshot",
              detail ~= "" and detail or nil,
              result.code
            )
          )
          return
        end
        local issues, parse_error = M.parse_issues(result.stdout or "")
        if issues == nil then
          callback(nil, parse_error)
          return
        end
        callback({ issues = issues, current = current, diff = M.diff(issues, current) }, nil)
      end)
    end
  )
  if not call_ok then
    return false, make_error("git_launch_failed", tostring(handle_or_error))
  end
  if handle_or_error == nil then
    return false, make_error("git_launch_failed", "vim.system did not return a process handle")
  end
  return true, nil
end

return M
