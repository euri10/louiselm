local Gates = require("louiselm.permission.gates")
local HumanPrompt = require("louiselm.permission.human-prompt")
local Policy = require("louiselm.permission.policy")
local Store = require("louiselm.permission.store")

local M = {}

---@param name? string|louiselm.permission.Policy Policy name or custom policy object.
---@param scopes? table Scope options for auto-approve-scoped.
---@return louiselm.permission.Policy? policy Normalized policy, or nil on invalid input.
---@return string? error_message Validation error.
function M.policy(name, scopes)
  return Policy.normalize(name, scopes)
end

M.gates = Gates
M.human_prompt = HumanPrompt.new

---Create an explicit remembered-permission store.
---@param path? string JSON path. Defaults below stdpath("state").
---@return louiselm.permission.Store? store
---@return string? error_message
function M.store(path)
  return Store.new(path)
end
---Create a policy that leaves every request for a human decision.
---@return louiselm.permission.Policy policy
function M.ask_human()
  return Policy.ask_human()
end

---Create a policy that allows only operations within explicit scopes.
---@param scopes table Scope options containing paths and command argv prefixes.
---@return louiselm.permission.Policy? policy Normalized policy, or nil on invalid input.
---@return string? error_message Validation error.
function M.auto_approve_scoped(scopes)
  return Policy.auto_approve_scoped(scopes)
end

return M
