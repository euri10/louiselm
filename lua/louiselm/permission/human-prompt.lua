local Policy = require("louiselm.permission.policy")

---@class louiselm.permission.HumanPrompt: louiselm.permission.Policy
---@field name "ask-human"
---@field evaluate fun(self: louiselm.permission.HumanPrompt, request: louiselm.permission.Request): louiselm.permission.Decision, string?
---@field prompt? fun(request: louiselm.permission.Request, respond: fun(decision: louiselm.permission.Decision)): boolean, string? Ask the presentation layer to decide.

local M = {}

---Create the default human-prompt policy.
---@param prompt? fun(request: louiselm.permission.Request, respond: fun(decision: louiselm.permission.Decision)): boolean, string? Optional presentation callback.
---@return louiselm.permission.HumanPrompt policy
function M.new(prompt)
  if prompt ~= nil and type(prompt) ~= "function" then
    error("human permission prompt must be a function")
  end
  local policy = Policy.ask_human()
  ---@cast policy louiselm.permission.HumanPrompt
  policy.prompt = prompt
  return policy
end

return M
