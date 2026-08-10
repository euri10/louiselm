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
  if skill_config.full_content ~= nil and type(skill_config.full_content) ~= "boolean" then
    return {}, {}, "skills full_content must be a boolean"
  end
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

  local index, index_error = skills_module().inject(skills, skill_config.full_content)
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
  local inline

  ---@param message string?
  local function report_error(message)
    if message ~= nil then
      nvim.notify("louiselm: " .. message, nvim.log.levels.ERROR)
    end
  end

  ---@return louiselm.ui.Chat? value
  local function ensure_chat()
    if chat ~= nil then
      return chat
    end
    local definitions = configured and configured_agents(configured)
    if definitions == nil then
      definitions = { default = default_agent_definition() }
    end
    local names = sorted_agent_names(definitions)
    local sessions, session_errors = session_module().new(definitions)
    if sessions == nil then
      nvim.notify("louiselm: invalid agent configuration (" .. #session_errors .. " errors)", nvim.log.levels.ERROR)
      return nil
    end
    local skills, initial_contexts, skills_error = {}, {}, nil
    if configured ~= nil then
      skills, initial_contexts, skills_error = configured_skills(configured)
    end
    if skills_error ~= nil then
      nvim.notify("louiselm: " .. skills_error, nvim.log.levels.ERROR)
      return nil
    end
    chat = assert(require("louiselm.ui.chat").new(sessions, {
      agents = names,
      skills = skills,
      initial_contexts = initial_contexts,
    }))
    return chat
  end

  ---@return louiselm.ui.Chat? value
  local function open_chat()
    local current = ensure_chat()
    if current == nil then
      return nil
    end
    local buffer = current:buffer()
    if buffer ~= nil then
      nvim.api.nvim_set_current_buf(buffer)
      return current
    end
    local _, session_error = current:new_session()
    report_error(session_error)
    return current
  end

  nvim.api.nvim_create_user_command("LouiselmChat", function()
    open_chat()
  end, { desc = "Open the louiselm chat buffer", force = true })

  nvim.api.nvim_create_user_command("LouiselmNewSession", function()
    if chat == nil then
      open_chat()
      return
    end
    local _, session_error = chat:new_session()
    report_error(session_error)
  end, { desc = "Create a separate louiselm session", force = true })

  nvim.api.nvim_create_user_command("LouiselmResume", function(arguments)
    local current = ensure_chat()
    if current == nil then
      return
    end
    local _, resume_error = current:resume_session(arguments.bang)
    report_error(resume_error)
  end, { bang = true, desc = "Resume a prior louiselm session; use ! for all workspaces", force = true })

  nvim.api.nvim_create_user_command("LouiselmSwitchSession", function()
    if chat == nil then
      report_error("no chat session is open")
      return
    end
    local _, switch_error = chat:switch_session()
    report_error(switch_error)
  end, { desc = "Switch between louiselm sessions", force = true })

  nvim.api.nvim_create_user_command("LouiselmRenameSession", function()
    if chat == nil then
      report_error("no chat session is open")
      return
    end
    nvim.ui.input({ prompt = "louiselm session name: " }, function(name)
      if name == nil then
        return
      end
      local _, rename_error = chat:rename_session(name)
      report_error(rename_error)
    end)
  end, { desc = "Rename the current louiselm session", force = true })

  nvim.api.nvim_create_user_command("LouiselmCloseSession", function()
    if chat == nil then
      report_error("no chat session is open")
      return
    end
    local _, close_error = chat:close_session()
    report_error(close_error)
  end, { desc = "Close the current louiselm session", force = true })

  nvim.api.nvim_create_user_command("LouiselmCancel", function()
    if chat == nil then
      report_error("no chat session is open")
      return
    end
    local _, cancel_error = chat:cancel()
    report_error(cancel_error)
  end, { desc = "Cancel the current louiselm turn", force = true })

  nvim.api.nvim_create_user_command("LouiselmSessionOptions", function()
    if chat == nil then
      report_error("no chat session is open")
      return
    end
    local _, options_error = chat:session_options()
    report_error(options_error)
  end, { desc = "Configure the current idle louiselm session", force = true })

  nvim.api.nvim_create_user_command("LouiselmInline", function()
    if inline == nil then
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
      inline = assert(require("louiselm.ui.inline").new(sessions, { agents = names, cwd = nvim.fn.getcwd() }))
    end
    nvim.ui.input({ prompt = "louiselm inline: " }, function(prompt)
      if prompt == nil then
        return
      end
      local _, inline_error = inline:run(prompt)
      report_error(inline_error)
    end)
  end, { desc = "Replace the current selection with louiselm output", force = true })
  return true
end

return M
