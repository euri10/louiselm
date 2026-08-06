---@class louiselm.agent.ProcessResult
---@field code integer Process exit code.
---@field signal integer Process signal, if terminated by a signal.
---@field stdout string Captured standard output.
---@field stderr string Captured standard error.

local Config = require("louiselm.agent.config")

local M = {}

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

---Start an external agent process without invoking a shell.
---@param definition louiselm.agent.Definition Normalized agent definition.
---@param on_exit? fun(result: louiselm.agent.ProcessResult) Called once when the process exits.
---@return userdata? handle The `vim.system()` process handle, or nil on failure.
---@return string? error_message A validation or launch error.
function M.start(definition, on_exit)
  local normalized, validation_error = normalize_definition(definition)
  if normalized == nil then
    return nil, validation_error
  end

  local command = { normalized.command }
  for _, argument in ipairs(normalized.args) do
    command[#command + 1] = argument
  end

  local options = { text = true }
  if normalized.env ~= nil then
    options.env = normalized.env
  end

  ---@diagnostic disable-next-line: undefined-global -- vim.system is Neovim's stable process API.
  local call_ok, handle_or_error = pcall(vim.system, command, options, on_exit)
  if not call_ok then
    return nil, tostring(handle_or_error)
  end
  return handle_or_error
end

return M
