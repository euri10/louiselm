---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local M = {}
local configured ---@type table?

local DEFAULT_ADAPTER_DEBUG_SCRIPT = "/home/lotso/code/acp-llm-adapter/acp-debug.sh"

local function session_module()
  return require("louiselm.session")
end

local function skills_module()
  return require("louiselm.skills")
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

---@param config table
---@return table<string, louiselm.agent.Definition>? definitions
local function configured_agents(config)
  if type(config.agents) == "table" then
    return config.agents
  end
  if type(config.agent) == "table" then
    return { default = config.agent }
  end
  return nil
end

---@param config table
---@return louiselm.skills.Skill[] skills
---@return louiselm.ui.ContextItem[] initial_contexts
---@return string? error_message
local function configured_skills(config)
  if type(config.skills) ~= "table" then
    return {}, {}
  end

  local skill_config = config.skills
  local skills = {}
  if skill_config.paths ~= nil then
    local discovery_errors
    skills, discovery_errors = skills_module().discover(skill_config.paths)
    for _, discovery_error in ipairs(discovery_errors) do
      nvim.notify("louiselm: " .. discovery_error.path .. ": " .. discovery_error.message, nvim.log.levels.WARN)
    end
  end

  local policy, policy_error = skills_module().policy(skill_config.policy)
  if policy == nil then
    return {}, {}, policy_error
  end
  if policy == "off" then
    return {}, {}
  end
  if policy == "native" or #skills == 0 then
    return skills, {}
  end

  local index, index_error = skills_module().inject(skills)
  if index == nil then
    return {}, {}, index_error
  end
  return skills, { { label = "skill-index", text = index } }
end

---@return louiselm.agent.Definition definition
local function default_agent_definition()
  local command = nvim.env.LOUISELM_AGENT_COMMAND
  if command ~= nil and command ~= "" then
    return { command = command, args = {} }
  end

  local environment
  local api_key = nvim.env.DEEPSEEK_API_KEY
  if api_key ~= nil and api_key ~= "" then
    environment = { LLM_API_KEY = api_key }
  end
  return {
    command = DEFAULT_ADAPTER_DEBUG_SCRIPT,
    args = { "acp-llm-adapter", "serve", "--backend", "deepseek" },
    env = environment,
  }
end

---Publish the validated setup configuration to the chat command.
---@param config? unknown Validated configuration, or nil to restore defaults.
---@return boolean configured True when the configuration is accepted.
---@return string? error_message Why the configuration was rejected.
function M.configure(config)
  if config == nil then
    configured = nil
    return true
  end
  if type(config) ~= "table" then
    return false, "chat command configuration must be a table"
  end
  configured = config
  return true
end

---Register the interactive chat command.
---@return boolean registered Always true after the command is registered.
function M.register()
  local chat

  nvim.api.nvim_create_user_command("LouiselmChat", function()
    if chat ~= nil and chat:buffer() ~= nil then
      nvim.api.nvim_set_current_buf(chat:buffer())
      return
    end

    local definitions = configured and configured_agents(configured)
    if definitions == nil then
      definitions = { default = default_agent_definition() }
    end
    local names = sorted_agent_names(definitions)
    local sessions, session_errors = session_module().new(definitions)
    if sessions == nil then
      nvim.notify("louiselm: invalid agent configuration (" .. #session_errors .. " errors)", nvim.log.levels.ERROR)
      return
    end
    local skills, initial_contexts, skills_error = {}, {}, nil
    if configured ~= nil then
      skills, initial_contexts, skills_error = configured_skills(configured)
    end
    if skills_error ~= nil then
      nvim.notify("louiselm: " .. skills_error, nvim.log.levels.ERROR)
      return
    end
    chat = assert(require("louiselm.ui.chat").new(sessions, {
      agents = names,
      skills = skills,
      initial_contexts = initial_contexts,
    }))
    local _, session_error = chat:new_session()
    if session_error ~= nil then
      nvim.notify("louiselm: " .. session_error, nvim.log.levels.ERROR)
    end
  end, { desc = "Open the louiselm chat buffer", force = true })
  return true
end

return M
