---@class louiselm.health.Configuration
---@field config unknown Validated user configuration.
---@field schema louiselm.schema.Schema Schema used to validate the configuration.

local Agent = require("louiselm.agent")
local Schema = require("louiselm.schema")
local Skills = require("louiselm.skills")

local M = {}
local configuration ---@type louiselm.health.Configuration?

---@return table
local function nvim()
  ---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
  return vim
end

---@param message string
---@param ok boolean
local function report(message, ok)
  local health = nvim().health
  if ok then
    health.ok(message)
  else
    health.error(message)
  end
end

---@param result louiselm.agent.HealthResult
local function report_agent_result(result)
  local message = result.command
  if result.version ~= nil then
    message = message .. " — " .. result.version
  end
  if result.ok then
    report(message, true)
  else
    report(message .. ": " .. (result.error or "version check failed"), false)
  end
end

---@param result louiselm.agent.HealthResult
local function report_agent_result_safely(result)
  local editor = nvim()
  if not editor.in_fast_event() then
    report_agent_result(result)
    return
  end

  local emit = function()
    local message = result.command
    if result.version ~= nil then
      message = message .. " — " .. result.version
    end
    if result.error ~= nil and not result.ok then
      message = message .. ": " .. result.error
    end
    editor.notify("louiselm health: " .. message, result.ok and editor.log.levels.INFO or editor.log.levels.ERROR)
  end

  -- vim.system callbacks run in a fast event; health reporting is editor work.
  editor.schedule(emit)
end

---@param value unknown
---@return table<string, louiselm.agent.Definition>? definitions
local function configured_agents(value)
  if type(value) == "table" and value.agents ~= nil then
    return value.agents
  end
  return nil
end

---@param value table<string, louiselm.agent.Definition>
---@return string[] names
local function sorted_agent_names(value)
  local names = {}
  for name in pairs(value) do
    names[#names + 1] = name
  end
  table.sort(names)
  return names
end

---@param value unknown
---@return unknown paths
local function configured_skill_paths(value)
  if type(value) ~= "table" or type(value.skills) ~= "table" then
    return nil
  end
  return value.skills.paths
end

---@param value unknown
---@return unknown policy
local function configured_skill_policy(value)
  if type(value) ~= "table" or type(value.skills) ~= "table" then
    return nil
  end
  return value.skills.policy
end

---@param config louiselm.health.Configuration
local function check_configuration(config)
  local validation = Schema.report(Schema.validate(config.schema, config.config))
  if validation.ok then
    report("configuration is valid", true)
  else
    report(validation.text, false)
  end
end

---@param config louiselm.health.Configuration
local function check_agents(config)
  local definitions = configured_agents(config.config)
  if definitions == nil then
    nvim().health.info("no agent definitions configured")
    return
  end

  local normalized, errors = Agent.normalize(definitions)
  if normalized == nil then
    for _, error_item in ipairs(errors) do
      report(error_item.path .. ": " .. error_item.message, false)
    end
    return
  end

  for _, name in ipairs(sorted_agent_names(normalized)) do
    local definition = normalized[name]
    local callback_called = false
    local handle, error_message = Agent.check(definition, function(result)
      callback_called = true
      report_agent_result_safely(result)
    end)
    if handle == nil then
      if error_message ~= nil and not callback_called then
        report("agent " .. name .. ": " .. error_message, false)
      end
    else
      nvim().health.info("agent " .. name .. ": checking executable and version")
    end
  end
end

---@param config louiselm.health.Configuration
local function check_skills(config)
  local default_policy = Skills.policy(configured_skill_policy(config.config))
  if default_policy == nil then
    return
  end
  local definitions = configured_agents(config.config) or {}
  local normalized = Agent.normalize(definitions, default_policy)
  if normalized == nil then
    return
  end

  local local_enabled = false
  local inject_enabled = false
  local names = sorted_agent_names(normalized)
  if #names == 0 then
    nvim().health.info("default agent skills policy: " .. default_policy)
    local_enabled = default_policy ~= "off"
    inject_enabled = default_policy == "inject"
  else
    for _, name in ipairs(names) do
      local policy = normalized[name].skills.policy
      nvim().health.info("agent " .. name .. " skills policy: " .. policy)
      local_enabled = local_enabled or policy ~= "off"
      inject_enabled = inject_enabled or policy == "inject"
    end
  end
  if not local_enabled then
    nvim().health.info("LouiseLM-managed local skills are disabled")
    return
  end

  if not Skills.local_available() then
    if inject_enabled then
      report(
        'lyaml is missing; skills policy "inject" cannot start (install lyaml or use skills.policy = "native" or "off")',
        false
      )
    else
      nvim().health.warn("lyaml is missing; native sessions can start but the local skill picker is unavailable")
    end
    return
  end
  local paths = configured_skill_paths(config.config)
  if paths == nil then
    nvim().health.info("no skill paths configured")
    return
  end

  local skills, errors = Skills.discover(paths)
  for _, error_item in ipairs(errors) do
    report(error_item.path .. ": " .. error_item.message, false)
  end
  if #errors == 0 then
    report(string.format("discovered %d skill%s", #skills, #skills == 1 and "" or "s"), true)
  end
end

---@param config louiselm.health.Configuration
local function check_capture(config)
  local capture = type(config.config) == "table" and config.config.capture or nil
  capture = type(capture) == "table" and capture or {}
  local commands = {
    { label = "capture recorder", command = (capture.recorder or { "pw-record" })[1] },
    { label = "capture service", command = (capture.service or { "louiselm-capture" })[1] },
  }
  for _, item in ipairs(commands) do
    if nvim().fn.executable(item.command) == 1 then
      report(item.label .. " is executable: " .. item.command, true)
    else
      report(item.label .. " is not executable: " .. item.command, false)
    end
  end
end

---Register the configuration that `:checkhealth louiselm` should inspect.
---@param config unknown Validated user configuration. The table is only read.
---@param schema louiselm.schema.Schema Normalized schema used for validation.
---@return boolean registered True when the registration arguments are valid.
---@return string? error_message Why registration failed.
function M.configure(config, schema)
  if type(schema) ~= "table" or schema.type ~= "table" or type(schema.fields) ~= "table" then
    return false, "health configuration requires a normalized schema"
  end
  configuration = { config = config, schema = schema }
  return true
end

---Forget the configuration used by the healthcheck.
---@return boolean cleared Always true.
function M.reset()
  configuration = nil
  return true
end

---Run the LouiseLM healthcheck discovered by `:checkhealth`.
---@return boolean checked False when setup has not registered a configuration.
function M.check()
  local health = nvim().health
  health.start("louiselm")
  if configuration == nil then
    health.warn("LouiseLM has not been configured; run setup() before checking configuration")
    return false
  end

  check_configuration(configuration)
  check_agents(configuration)
  check_skills(configuration)
  check_capture(configuration)
  return true
end

return M
