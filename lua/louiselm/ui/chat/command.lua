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

local function instructions_module()
  return require("louiselm.instructions")
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
  return nil
end

---@param config table
---@return louiselm.skills.Policy? policy
---@return string? error_message
local function configured_skill_policy(config)
  local value = type(config.skills) == "table" and config.skills.policy or nil
  return skills_module().policy(value)
end

---@param definitions table<string, louiselm.agent.Definition>
---@param default_policy louiselm.skills.Policy
---@return boolean enabled
local function inject_policy_enabled(definitions, default_policy)
  for _, definition in pairs(definitions) do
    local override = type(definition.skills) == "table" and definition.skills.policy or nil
    local policy = override or default_policy
    if policy == "inject" then
      return true
    end
  end
  return false
end

---@param config table
---@param definitions table<string, louiselm.agent.Definition>
---@param default_policy louiselm.skills.Policy
---@return louiselm.skills.Skill[] skills
---@return louiselm.ui.ContextItem? skill_context
---@return string? error_message
local function configured_skills(config, definitions, default_policy)
  if type(config.skills) ~= "table" then
    return {}, nil
  end

  local skill_config = config.skills
  if not inject_policy_enabled(definitions, default_policy) then
    return {}, nil
  end
  local skills = {}
  if skill_config.paths ~= nil then
    local discovery_errors
    skills, discovery_errors = skills_module().discover(skill_config.paths)
    for _, discovery_error in ipairs(discovery_errors) do
      if discovery_error.code == "missing_dependency" then
        return {}, nil, discovery_error.message
      end
    end
    if #discovery_errors > 0 then
      nvim.notify(
        string.format(
          "louiselm: skill discovery found %d issue(s); run :checkhealth louiselm for details",
          #discovery_errors
        ),
        nvim.log.levels.WARN
      )
    end
  end

  if #skills == 0 then
    return skills, nil
  end

  local index, index_error = skills_module().inject(skills)
  if index == nil then
    return {}, nil, index_error
  end
  return skills, { label = "skill-index", text = index }
end

---@param config table
---@return louiselm.ui.ContextItem? instructions_context
local function configured_instructions(config)
  local filename = type(config.context) == "table" and config.context.instructions_file or nil
  return instructions_module().link(filename)
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
    local default_policy, policy_error = configured_skill_policy(configured or {})
    if default_policy == nil then
      nvim.notify("louiselm: " .. (policy_error or "invalid skills policy"), nvim.log.levels.ERROR)
      return nil
    end
    local sessions, session_errors = session_module().new(definitions, default_policy)
    if sessions == nil then
      nvim.notify("louiselm: invalid agent configuration (" .. #session_errors .. " errors)", nvim.log.levels.ERROR)
      return nil
    end
    local skills, skill_context, skills_error = {}, nil, nil
    local instructions_context
    if configured ~= nil then
      skills, skill_context, skills_error = configured_skills(configured, definitions, default_policy)
      instructions_context = configured_instructions(configured)
    end
    if skills_error ~= nil then
      nvim.notify("louiselm: " .. skills_error, nvim.log.levels.ERROR)
      return nil
    end
    chat = assert(require("louiselm.ui.chat").new(sessions, {
      agents = names,
      skills = skills,
      skill_paths = configured and configured.skills and configured.skills.paths or nil,
      skill_context = skill_context,
      instructions_context = instructions_context,
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

  nvim.api.nvim_create_user_command("LouiselmSessionId", function()
    if chat == nil then
      report_error("no chat session is open")
      return
    end
    local session_id, session_error = chat:session_id()
    if session_id == nil then
      report_error(session_error)
      return
    end
    local copied = pcall(nvim.fn.setreg, "+", session_id)
    local message = copied and "copied session id " or "session id "
    nvim.notify("louiselm: " .. message .. session_id, nvim.log.levels.INFO)
  end, { desc = "Copy the current agent-scoped ACP session id", force = true })

  nvim.api.nvim_create_user_command("LouiselmToMarkdown", function(arguments)
    if chat == nil then
      report_error("no chat session is open")
      return
    end
    local session_id = arguments.args ~= "" and arguments.args or nil
    nvim.ui.input({ prompt = "louiselm markdown path (blank for default): " }, function(path)
      if path == nil then
        return
      end
      local written_path, write_error = chat:to_markdown(session_id, path ~= "" and path or nil)
      if written_path == nil then
        report_error(write_error)
        return
      end
      nvim.notify("louiselm: exported transcript to " .. written_path, nvim.log.levels.INFO)
    end)
  end, {
    nargs = "?",
    desc = "Export a louiselm session transcript to markdown; takes an optional session id",
    force = true,
  })

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

  nvim.api.nvim_create_user_command("LouiselmPermissions", function()
    local current = ensure_chat()
    if current == nil then
      return
    end
    local _, permissions_error = current:manage_permissions()
    report_error(permissions_error)
  end, { desc = "Inspect and revoke remembered louiselm permissions", force = true })

  nvim.api.nvim_create_user_command("LouiselmInline", function()
    if inline == nil then
      local definitions = configured and configured_agents(configured)
      if definitions == nil then
        definitions = { default = default_agent_definition() }
      end
      local names = sorted_agent_names(definitions)
      local default_policy, policy_error = configured_skill_policy(configured or {})
      if default_policy == nil then
        nvim.notify("louiselm: " .. (policy_error or "invalid skills policy"), nvim.log.levels.ERROR)
        return
      end
      local sessions, session_errors = session_module().new(definitions, default_policy)
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
