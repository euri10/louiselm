---@alias louiselm.permission.Decision "allow"|"deny"|"ask"

---@class louiselm.permission.Request
---@field kind "file_edit"|"command"|"unknown" Operation being requested.
---@field path? string File being edited.
---@field command? string[] Command argv.
---@field diff? string Proposed file diff.

---@class louiselm.permission.Policy
---@field name string Policy name.
---@field evaluate fun(self: louiselm.permission.Policy, request: louiselm.permission.Request): louiselm.permission.Decision, string? Evaluate one operation.

local M = {}

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local function is_dense_string_array(value)
  if type(value) ~= "table" then
    return false
  end
  local length = 0
  while value[length + 1] ~= nil do
    if type(value[length + 1]) ~= "string" or value[length + 1] == "" then
      return false
    end
    length = length + 1
  end
  for key in pairs(value) do
    if type(key) ~= "number" or key < 1 or key % 1 ~= 0 or key > length then
      return false
    end
  end
  return true
end

local function copy_string_array(value, name)
  if value == nil then
    return {}
  end
  if not is_dense_string_array(value) then
    return nil, name .. " must be a dense array of non-empty strings"
  end
  local copy = {}
  for index, item in ipairs(value) do
    copy[index] = item
  end
  return copy
end

local function copy_command_scopes(value)
  if value == nil then
    return {}
  end
  if type(value) ~= "table" then
    return nil, "permission command scopes must be an array"
  end
  local scopes = {}
  local index = 0
  while value[index + 1] ~= nil do
    index = index + 1
    local command, err = copy_string_array(value[index], "permission command scope")
    if command == nil then
      return nil, err
    end
    scopes[index] = command
  end
  for key in pairs(value) do
    if type(key) ~= "number" or key < 1 or key % 1 ~= 0 or key > index then
      return nil, "permission command scopes must be a dense array"
    end
  end
  return scopes
end

local function path_is_scoped(path, scope)
  path = nvim.fs.normalize(path)
  scope = nvim.fs.normalize(scope)
  if path == scope then
    return true
  end
  return scope == "/" or path:sub(1, #scope + 1) == scope .. "/"
end

local function command_is_scoped(command, scope)
  if #command < #scope then
    return false
  end
  for index, value in ipairs(scope) do
    if command[index] ~= value then
      return false
    end
  end
  return true
end

local function ask_human()
  return {
    name = "ask-human",
    evaluate = function(_, _)
      return "ask"
    end,
  }
end

local function auto_approve_scoped(scopes)
  local paths, path_error = copy_string_array(scopes.paths, "permission paths")
  if paths == nil then
    return nil, path_error
  end
  local commands, command_error = copy_command_scopes(scopes.commands)
  if commands == nil then
    return nil, command_error
  end

  for key in pairs(scopes) do
    if key ~= "paths" and key ~= "commands" then
      return nil, "unknown auto-approve-scoped option " .. tostring(key)
    end
  end

  return {
    name = "auto-approve-scoped",
    evaluate = function(_, request)
      if request.kind == "file_edit" and type(request.path) == "string" then
        for _, path in ipairs(paths) do
          if path_is_scoped(request.path, path) then
            return "allow"
          end
        end
      elseif request.kind == "command" and type(request.command) == "table" then
        for _, command in ipairs(commands) do
          if command_is_scoped(request.command, command) then
            return "allow"
          end
        end
      end
      return "deny"
    end,
  }
end

---Create or normalize a permission policy.
---@param name? string|louiselm.permission.Policy Policy name or custom policy object. Nil asks a human.
---@param scopes? table Scope options for auto-approve-scoped.
---@return louiselm.permission.Policy? policy Normalized policy, or nil on invalid input.
---@return string? error_message Validation error.
function M.normalize(name, scopes)
  if name == nil then
    return ask_human()
  end
  if type(name) == "table" then
    if type(name.evaluate) ~= "function" then
      return nil, "permission policy must provide an evaluate function"
    end
    return name
  end
  if type(name) ~= "string" then
    return nil, "permission policy must be a string or policy object"
  end
  if name == "ask-human" then
    if scopes ~= nil then
      return nil, "ask-human policy does not accept scopes"
    end
    return ask_human()
  end
  if name == "auto-approve-scoped" then
    if type(scopes) ~= "table" then
      return nil, "auto-approve-scoped policy requires a scopes table"
    end
    return auto_approve_scoped(scopes)
  end
  return nil, "permission policy must be one of: ask-human, auto-approve-scoped"
end

---Create a policy that leaves every request for a human decision.
---@return louiselm.permission.Policy policy
function M.ask_human()
  return ask_human()
end

---Create a policy that allows only operations within explicit scopes.
---@param scopes table Scope options containing paths and command argv prefixes.
---@return louiselm.permission.Policy? policy Normalized policy, or nil on invalid input.
---@return string? error_message Validation error.
function M.auto_approve_scoped(scopes)
  if type(scopes) ~= "table" then
    return nil, "auto-approve-scoped policy requires a scopes table"
  end
  return auto_approve_scoped(scopes)
end

return M
