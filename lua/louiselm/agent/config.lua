---@class louiselm.agent.Definition
---@field command string Executable to start.
---@field args string[] Arguments passed after the command.
---@field env? table<string, string> Environment variables for the process.
---@field options? table<string, unknown> Agent-specific options.

---@alias louiselm.agent.ConfigErrorType "unknown_key"|"wrong_type"|"missing_required"|"invalid_value"

---@class louiselm.agent.ConfigError
---@field path string Configuration path containing the error.
---@field type louiselm.agent.ConfigErrorType
---@field expected? string Expected type or shape.
---@field got? string Received Lua type.
---@field message string Human-readable failure.

---@alias louiselm.agent.Definitions table<string, louiselm.agent.Definition>

local M = {}

local allowed_keys = {
  args = true,
  command = true,
  env = true,
  options = true,
}

---@param value unknown
---@return string
local function value_type(value)
  return type(value)
end

---@param path string
---@param key string
---@return string
local function child_path(path, key)
  return path .. "." .. key
end

---@param path string
---@param index integer
---@return string
local function item_path(path, index)
  return path .. "[" .. index .. "]"
end

---@param value table
---@return unknown[] keys
local function sorted_keys(value)
  local keys = {}
  for key in pairs(value) do
    keys[#keys + 1] = key
  end
  table.sort(keys, function(left, right)
    return tostring(left) < tostring(right)
  end)
  return keys
end

---@param value table
---@param path string
---@return integer length
---@return boolean dense
local function sequence_length(value, path)
  local length = 0
  while value[length + 1] ~= nil do
    length = length + 1
  end

  for key in pairs(value) do
    if type(key) ~= "number" or key < 1 or key % 1 ~= 0 or key > length then
      return 0, false
    end
  end
  return length, true
end

---@param errors louiselm.agent.ConfigError[]
---@param path string
---@param error_type louiselm.agent.ConfigErrorType
---@param message string
---@param expected? string
---@param got? string
local function add_error(errors, path, error_type, message, expected, got)
  errors[#errors + 1] = {
    path = path,
    type = error_type,
    message = message,
    expected = expected,
    got = got,
  }
end

---@param value table
---@param path string
---@param errors louiselm.agent.ConfigError[]
---@return string[]? args
local function copy_args(value, path, errors)
  local length, dense = sequence_length(value, path)
  if not dense then
    add_error(errors, path, "invalid_value", "must be a dense array of strings", "string[]", "table")
    return nil
  end

  local args = {}
  for index = 1, length do
    local argument = value[index]
    if type(argument) ~= "string" then
      add_error(
        errors,
        item_path(path, index),
        "wrong_type",
        string.format("expected string, got %s", value_type(argument)),
        "string",
        value_type(argument)
      )
    else
      args[index] = argument
    end
  end
  return args
end

---@param value table
---@param path string
---@param errors louiselm.agent.ConfigError[]
---@return table<string, string>? env
local function copy_env(value, path, errors)
  local env = {}
  for _, key in ipairs(sorted_keys(value)) do
    local variable = value[key]
    if type(key) ~= "string" or key == "" then
      add_error(errors, path, "invalid_value", "environment keys must be non-empty strings")
    elseif type(variable) ~= "string" then
      add_error(
        errors,
        child_path(path, key),
        "wrong_type",
        string.format("expected string, got %s", value_type(variable)),
        "string",
        value_type(variable)
      )
    else
      env[key] = variable
    end
  end
  return env
end

---@param value table
---@return table<string, unknown>
local function copy_options(value)
  local options = {}
  for key, option in pairs(value) do
    options[key] = option
  end
  return options
end

---@param definitions unknown
---@return louiselm.agent.Definitions? normalized
---@return louiselm.agent.ConfigError[] errors
function M.normalize(definitions)
  local errors = {}
  if type(definitions) ~= "table" then
    add_error(
      errors,
      "agents",
      "wrong_type",
      "expected table, got " .. value_type(definitions),
      "table",
      value_type(definitions)
    )
    return nil, errors
  end

  local normalized = {}
  for _, name in ipairs(sorted_keys(definitions)) do
    local definition = definitions[name]
    local path = "agents." .. tostring(name)
    if type(name) ~= "string" or name == "" then
      add_error(errors, path, "invalid_value", "agent names must be non-empty strings")
    elseif type(definition) ~= "table" then
      add_error(
        errors,
        path,
        "wrong_type",
        "expected table, got " .. value_type(definition),
        "table",
        value_type(definition)
      )
    else
      for _, key in ipairs(sorted_keys(definition)) do
        if type(key) ~= "string" or not allowed_keys[key] then
          add_error(errors, child_path(path, tostring(key)), "unknown_key", "unknown agent configuration key")
        end
      end

      local command = definition.command
      if command == nil then
        add_error(
          errors,
          child_path(path, "command"),
          "missing_required",
          "required command is missing",
          "string",
          "nil"
        )
      elseif type(command) ~= "string" then
        add_error(
          errors,
          child_path(path, "command"),
          "wrong_type",
          "expected string, got " .. value_type(command),
          "string",
          value_type(command)
        )
      elseif command == "" then
        add_error(errors, child_path(path, "command"), "invalid_value", "command must be a non-empty string")
      end

      local args = {}
      if definition.args ~= nil then
        if type(definition.args) ~= "table" then
          add_error(
            errors,
            child_path(path, "args"),
            "wrong_type",
            "expected table, got " .. value_type(definition.args),
            "string[]",
            value_type(definition.args)
          )
        else
          args = copy_args(definition.args, child_path(path, "args"), errors) or {}
        end
      end

      local env
      if definition.env ~= nil then
        if type(definition.env) ~= "table" then
          add_error(
            errors,
            child_path(path, "env"),
            "wrong_type",
            "expected table, got " .. value_type(definition.env),
            "table<string, string>",
            value_type(definition.env)
          )
        else
          env = copy_env(definition.env, child_path(path, "env"), errors)
        end
      end

      local options
      if definition.options ~= nil then
        if type(definition.options) ~= "table" then
          add_error(
            errors,
            child_path(path, "options"),
            "wrong_type",
            "expected table, got " .. value_type(definition.options),
            "table",
            value_type(definition.options)
          )
        else
          options = copy_options(definition.options)
        end
      end

      normalized[name] = {
        command = command,
        args = args,
        env = env,
        options = options,
      }
    end
  end

  table.sort(errors, function(left, right)
    if left.path == right.path then
      return left.type < right.type
    end
    return left.path < right.path
  end)

  if #errors > 0 then
    return nil, errors
  end
  return normalized, errors
end

return M
