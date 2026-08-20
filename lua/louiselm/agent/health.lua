---@class louiselm.agent.HealthResult
---@field ok boolean Whether the executable responded with a version.
---@field command string Checked executable.
---@field available boolean Whether the executable is on PATH.
---@field version? string First line returned by `--version`.
---@field error? string Specific availability or version failure.
---@field code? integer Version process exit code.
---@field latest_version? string First line returned by the configured latest-version check.
---@field latest_error? string Why the latest-version check did not resolve a version.
---@field outdated? boolean True when both the installed and latest versions resolved and differ.

local Config = require("louiselm.agent.config")

local M = {}

---Extract the trailing semver-looking token from a version string, so a
---banner like "@scope/pkg 1.6.0" compares equal to a bare "1.6.0" from a
---separate latest-version check. Falls back to the whole string when no
---such token is found (e.g. an already-bare version).
---@param value string
---@return string
local function extract_version(value)
  return value:match("v?(%d[%d%.%-%+%w]*)%s*$") or value
end

---@param value string
---@return string?
local function first_line(value)
  local line = value:match("[^\r\n]+")
  if line == nil then
    return nil
  end
  line = line:gsub("^%s+", ""):gsub("%s+$", "")
  if line == "" then
    return nil
  end
  return line
end

---@param executable string
---@param args string[]
---@return string[]
local function build_argv(executable, args)
  local argv = { executable }
  for _, argument in ipairs(args) do
    argv[#argv + 1] = argument
  end
  return argv
end

---@param definition unknown
---@return louiselm.agent.Definition? normalized
---@return string? error_message
local function normalize_definition(definition)
  local normalized, errors = Config.normalize({ agent = definition })
  if normalized == nil then
    local first_error = errors[1]
    return nil, first_error.path .. ": " .. first_error.message
  end
  return normalized.agent
end

---@param result louiselm.agent.ProcessResult
---@param definition louiselm.agent.Definition
---@return louiselm.agent.HealthResult
local function build_version_result(result, definition)
  local version = first_line(result.stdout or "")
  local health_result = {
    ok = result.code == 0 and version ~= nil,
    command = definition.command,
    available = true,
    version = version,
    code = result.code,
  }
  if not health_result.ok then
    health_result.error = first_line(result.stderr or "")
      or (result.code == 0 and "version command returned no output" or "version command failed")
  end
  return health_result
end

---@param result louiselm.agent.ProcessResult
---@return string? latest_version
---@return string? latest_error
local function resolve_latest(result)
  local latest_version = first_line(result.stdout or "")
  if latest_version ~= nil and result.code == 0 then
    return latest_version, nil
  end
  local error_message = first_line(result.stderr or "")
    or (result.code == 0 and "latest-version command returned no output" or "latest-version command failed")
  return nil, error_message
end

---Spawn a normalized `latest` check, deferring completion to `on_done`.
---`on_done` is always called exactly once, synchronously when the
---executable cannot be found and asynchronously otherwise.
---@param latest louiselm.agent.CommandCheck
---@param on_done fun(latest_version: string?, latest_error: string?)
local function start_latest_check(latest, on_done)
  ---@diagnostic disable-next-line: undefined-global -- vim.fn.executable is Neovim's PATH lookup API.
  if vim.fn.executable(latest.command) == 0 then
    on_done(nil, "executable not found on PATH")
    return
  end

  local command = build_argv(latest.command, latest.args)
  local options = { text = true }
  if latest.env ~= nil then
    options.env = latest.env
  end

  ---@diagnostic disable-next-line: undefined-global -- vim.system is Neovim's stable process API.
  local call_ok, handle_or_error = pcall(vim.system, command, options, function(result)
    on_done(resolve_latest(result))
  end)
  if not call_ok then
    on_done(nil, tostring(handle_or_error))
  end
end

---Check that an agent executable is available and report its version asynchronously.
---@param definition louiselm.agent.Definition Agent definition to check.
---@param on_result? fun(result: louiselm.agent.HealthResult) Called once with the health result.
---@return table? handle The version-check process handle, or nil when no process starts.
---@return string? error_message Validation or immediate availability error.
function M.check(definition, on_result)
  local normalized, validation_error = normalize_definition(definition)
  if normalized == nil then
    return nil, validation_error
  end
  if on_result ~= nil and type(on_result) ~= "function" then
    return nil, "health callback must be a function"
  end

  ---@diagnostic disable-next-line: undefined-global -- vim.fn.executable is Neovim's PATH lookup API.
  if vim.fn.executable(normalized.command) == 0 then
    local error_message = "executable not found on PATH"
    if on_result ~= nil then
      on_result({
        ok = false,
        command = normalized.command,
        available = false,
        error = error_message,
      })
    end
    return nil, normalized.command .. ": " .. error_message
  end

  local version_check = normalized.version
  if version_check ~= nil then
    ---@diagnostic disable-next-line: undefined-global -- vim.fn.executable is Neovim's PATH lookup API.
    if vim.fn.executable(version_check.command) == 0 then
      local error_message = "executable not found on PATH"
      if on_result ~= nil then
        on_result({
          ok = false,
          command = normalized.command,
          available = true,
          error = error_message,
        })
      end
      return nil, version_check.command .. ": " .. error_message
    end
  end

  local command, options
  if version_check ~= nil then
    command = build_argv(version_check.command, version_check.args)
    options = { text = true }
    if version_check.env ~= nil then
      options.env = version_check.env
    end
  else
    command = build_argv(normalized.command, normalized.args)
    command[#command + 1] = "--version"
    options = { text = true }
    if normalized.env ~= nil then
      options.env = normalized.env
    end
  end

  local latest = normalized.latest
  if latest == nil then
    local callback = function(result)
      if on_result ~= nil then
        on_result(build_version_result(result, normalized))
      end
    end
    ---@diagnostic disable-next-line: undefined-global -- vim.system is Neovim's stable process API.
    local call_ok, handle_or_error = pcall(vim.system, command, options, callback)
    if not call_ok then
      return nil, tostring(handle_or_error)
    end
    return handle_or_error
  end

  -- The version check and the latest-version check are two independent
  -- vim.system calls; combine their results and fire on_result exactly
  -- once, regardless of which one completes first.
  local version_result, latest_version, latest_error, latest_done, fired

  local function finish()
    if fired or version_result == nil or not latest_done then
      return
    end
    fired = true
    if on_result == nil then
      return
    end
    local health_result = build_version_result(version_result, normalized)
    health_result.latest_version = latest_version
    health_result.latest_error = latest_error
    if health_result.version ~= nil and latest_version ~= nil then
      health_result.outdated = extract_version(health_result.version) ~= extract_version(latest_version)
    end
    on_result(health_result)
  end

  local callback = function(result)
    version_result = result
    finish()
  end

  ---@diagnostic disable-next-line: undefined-global -- vim.system is Neovim's stable process API.
  local call_ok, handle_or_error = pcall(vim.system, command, options, callback)
  if not call_ok then
    return nil, tostring(handle_or_error)
  end

  start_latest_check(latest, function(resolved_version, resolved_error)
    latest_version = resolved_version
    latest_error = resolved_error
    latest_done = true
    finish()
  end)

  return handle_or_error
end

return M
