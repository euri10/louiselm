---@class louiselm.provenance.SourceError: louiselm.provenance.Error
---@field exit_code? integer Git exit code, when available.
---@field detail? string Bounded stderr detail, when available.

local M = {}

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local RECORD_SEPARATOR = string.char(30)
local FIELD_SEPARATOR = string.char(0)
local LOG_FORMAT = "%H%x00%B%x00%x1e"

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

return M
