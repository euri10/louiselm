local Policy = require("louiselm.skills.policy")
local Provider = require("louiselm.agent.provider")

---@class louiselm.agent.SkillConfig
---@field policy? louiselm.skills.Policy Agent-specific policy override.

---@class louiselm.agent.CommandCheck
---@field command string Executable that prints a version.
---@field args string[] Arguments passed after the command, verbatim (nothing is auto-appended).
---@field env? table<string, string> Environment variables for the process.

---@class louiselm.agent.Definition
---@field command string Executable to start.
---@field args string[] Arguments passed after the command.
---@field provider louiselm.agent.Provider Explicit access/quota service or option routes; required before prompting.
---@field env? table<string, string> Environment variables for the process.
---@field options? table<string, unknown> Agent-specific options. `options._meta`, when present, is threaded
---verbatim into the ACP `session/new`/`session/load` request params (e.g. Claude's
---`{ claudeCode = { options = { thinking = { type = "adaptive" } } } }`).
---@field capabilities? string[] Capability tags this agent declares support for (e.g. "image-generation"). Matched against `needs-capability:*` beads labels by the agent selecting work; louiselm neither reads beads nor routes work itself.
---@field transcript_layout? string Optional Provenance integration for locating this Agent's historical transcripts on disk; live chat transcripts need no configuration.
---Setting this to `"claude"` also defaults the ACP session to request summarized thinking display
---(`_meta.claudeCode.options.thinking = { type = "adaptive", display = "summarized" }`) unless
---`options._meta.claudeCode.options.thinking` is already set: recent Claude models default to
---`display = "omitted"`, which streams signature-only reasoning with no visible `[thinking]` text.
---Summarized display trades a small amount of extra streaming latency for a visible reasoning trace.
---@field skills? louiselm.agent.SkillConfig Effective Agent Skills policy after normalization.
---@field version? louiselm.agent.CommandCheck Optional override for querying the installed version, when `command args... --version` is not the right invocation (e.g. a subcommand-based CLI).
---@field latest? louiselm.agent.CommandCheck Optional command that resolves the latest available version.
---@field upgrade? string[] Executable and arguments shown as a shell-escaped upgrade command; never executed by LouiseLM. Omission leaves upgrade guidance unavailable.

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
  capabilities = true,
  command = true,
  env = true,
  latest = true,
  options = true,
  provider = true,
  skills = true,
  transcript_layout = true,
  upgrade = true,
  version = true,
}

local command_check_allowed_keys = {
  args = true,
  command = true,
  env = true,
}

---@type table<"unknown_key"|"wrong_type"|"missing_required"|"validation_failed", louiselm.agent.ConfigErrorType>
local provider_error_types = {
  unknown_key = "unknown_key",
  wrong_type = "wrong_type",
  missing_required = "missing_required",
  validation_failed = "invalid_value",
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
---@return string[]? capabilities
local function copy_capabilities(value, path, errors)
  local length, dense = sequence_length(value, path)
  if not dense then
    add_error(errors, path, "invalid_value", "must be a dense array of non-empty strings", "string[]", "table")
    return nil
  end

  local capabilities = {}
  for index = 1, length do
    local capability = value[index]
    if type(capability) ~= "string" or capability == "" then
      add_error(
        errors,
        item_path(path, index),
        "invalid_value",
        string.format("expected non-empty string, got %s", value_type(capability)),
        "string",
        value_type(capability)
      )
    else
      capabilities[index] = capability
    end
  end
  return capabilities
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

---@param value table
---@param path string
---@param errors louiselm.agent.ConfigError[]
---@return louiselm.agent.CommandCheck fields
local function parse_process_fields(value, path, errors)
  local command = value.command
  if command == nil then
    add_error(errors, child_path(path, "command"), "missing_required", "required command is missing", "string", "nil")
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
  if value.args ~= nil then
    if type(value.args) ~= "table" then
      add_error(
        errors,
        child_path(path, "args"),
        "wrong_type",
        "expected table, got " .. value_type(value.args),
        "string[]",
        value_type(value.args)
      )
    else
      args = copy_args(value.args, child_path(path, "args"), errors) or {}
    end
  end

  local env
  if value.env ~= nil then
    if type(value.env) ~= "table" then
      add_error(
        errors,
        child_path(path, "env"),
        "wrong_type",
        "expected table, got " .. value_type(value.env),
        "table<string, string>",
        value_type(value.env)
      )
    else
      env = copy_env(value.env, child_path(path, "env"), errors)
    end
  end

  return { command = command, args = args, env = env }
end

---@param value unknown
---@param path string
---@param errors louiselm.agent.ConfigError[]
---@return louiselm.agent.CommandCheck? check
local function parse_command_check(value, path, errors)
  if type(value) ~= "table" then
    add_error(errors, path, "wrong_type", "expected table, got " .. value_type(value), "table", value_type(value))
    return nil
  end

  for _, key in ipairs(sorted_keys(value)) do
    if type(key) ~= "string" or not command_check_allowed_keys[key] then
      add_error(errors, child_path(path, tostring(key)), "unknown_key", "unknown version-check configuration key")
    end
  end
  return parse_process_fields(value, path, errors)
end

---@param value unknown
---@param path string
---@param errors louiselm.agent.ConfigError[]
---@param default_policy louiselm.skills.Policy
---@return louiselm.skills.Policy policy
local function skill_policy(value, path, errors, default_policy)
  if value == nil then
    return default_policy
  end
  if type(value) ~= "table" then
    add_error(errors, path, "wrong_type", "expected table, got " .. value_type(value), "table", value_type(value))
    return default_policy
  end

  for _, key in ipairs(sorted_keys(value)) do
    if key ~= "policy" then
      add_error(errors, child_path(path, tostring(key)), "unknown_key", "unknown agent skills configuration key")
    end
  end
  if value.policy == nil then
    add_error(errors, child_path(path, "policy"), "missing_required", "agent skills policy override is missing")
    return default_policy
  end
  local normalized, policy_error = Policy.normalize(value.policy)
  if normalized == nil then
    add_error(errors, child_path(path, "policy"), "invalid_value", policy_error or "invalid skills policy")
    return default_policy
  end
  return normalized
end

---@param definitions unknown
---@param default_skills_policy? unknown Global Agent Skills policy inherited by agents without an override.
---@return louiselm.agent.Definitions? normalized
---@return louiselm.agent.ConfigError[] errors
function M.normalize(definitions, default_skills_policy)
  local errors = {}
  local default_policy, policy_error = Policy.normalize(default_skills_policy)
  if default_policy == nil then
    add_error(errors, "skills.policy", "invalid_value", policy_error or "invalid skills policy")
    default_policy = "native"
  end
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

      local process = parse_process_fields(definition, path, errors)
      local provider, provider_errors = Provider.normalize(definition.provider)
      for _, provider_error in ipairs(provider_errors) do
        add_error(
          errors,
          child_path(path, provider_error.path),
          provider_error_types[provider_error.type],
          provider_error.message or "configure provider as a service name or exact option routes",
          provider_error.expected,
          provider_error.got
        )
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

      local capabilities
      if definition.capabilities ~= nil then
        if type(definition.capabilities) ~= "table" then
          add_error(
            errors,
            child_path(path, "capabilities"),
            "wrong_type",
            "expected table, got " .. value_type(definition.capabilities),
            "string[]",
            value_type(definition.capabilities)
          )
        else
          capabilities = copy_capabilities(definition.capabilities, child_path(path, "capabilities"), errors)
        end
      end

      local transcript_layout = definition.transcript_layout
      if transcript_layout ~= nil then
        if type(transcript_layout) ~= "string" then
          add_error(
            errors,
            child_path(path, "transcript_layout"),
            "wrong_type",
            "expected string, got " .. value_type(transcript_layout),
            "string",
            value_type(transcript_layout)
          )
        elseif transcript_layout == "" then
          transcript_layout = nil
        end
      end

      local effective_skill_policy = skill_policy(definition.skills, child_path(path, "skills"), errors, default_policy)
      local latest
      if definition.latest ~= nil then
        latest = parse_command_check(definition.latest, child_path(path, "latest"), errors)
      end
      local version
      if definition.version ~= nil then
        version = parse_command_check(definition.version, child_path(path, "version"), errors)
      end
      local upgrade
      if definition.upgrade ~= nil then
        local upgrade_path = child_path(path, "upgrade")
        if type(definition.upgrade) ~= "table" then
          add_error(
            errors,
            upgrade_path,
            "wrong_type",
            "must be an executable and arguments array",
            "string[]",
            value_type(definition.upgrade)
          )
        else
          upgrade = copy_args(definition.upgrade, upgrade_path, errors)
          if upgrade ~= nil and (#upgrade == 0 or upgrade[1] == "") then
            add_error(errors, upgrade_path, "invalid_value", "must start with an upgrade executable")
          end
        end
      end
      normalized[name] = {
        command = process.command,
        provider = provider,
        args = process.args,
        env = process.env,
        options = options,
        capabilities = capabilities,
        transcript_layout = transcript_layout,
        skills = { policy = effective_skill_policy },
        latest = latest,
        version = version,
        upgrade = upgrade,
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
