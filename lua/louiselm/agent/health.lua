---@class louiselm.agent.HealthResult
---@field ok boolean Whether the executable responded with a version.
---@field command string Checked executable.
---@field available boolean Whether the executable is on PATH.
---@field version? string First line returned by `--version`.
---@field error? string Specific availability or version failure.
---@field code? integer Version process exit code.

local Config = require("louiselm.agent.config")

local M = {}

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
---@param on_result? fun(result: louiselm.agent.HealthResult)
local function report_version(result, definition, on_result)
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
  if on_result ~= nil then
    on_result(health_result)
  end
end

---Check that an agent executable is available and report its version asynchronously.
---@param definition louiselm.agent.Definition Agent definition to check.
---@param on_result? fun(result: louiselm.agent.HealthResult) Called once with the health result.
---@return userdata? handle The version-check process handle, or nil when no process starts.
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

  local command = { normalized.command, "--version" }
  local options = { text = true }
  if normalized.env ~= nil then
    options.env = normalized.env
  end
  local callback = function(result)
    report_version(result, normalized, on_result)
  end

  ---@diagnostic disable-next-line: undefined-global -- vim.system is Neovim's stable process API.
  local call_ok, handle_or_error = pcall(vim.system, command, options, callback)
  if not call_ok then
    return nil, tostring(handle_or_error)
  end
  return handle_or_error
end

return M
