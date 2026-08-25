---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim
local Agent = require("louiselm.agent")
local Workflow = require("louiselm.workflow")

local M = {}
local configured ---@type table?

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

---@return table<string, louiselm.agent.Definition>? definitions
local function require_configured_agents()
  local definitions = configured and configured.agents
  if type(definitions) == "table" and next(definitions) ~= nil then
    return definitions
  end
  nvim.notify(
    "louiselm: no Agent configured; add one to require('louiselm').setup({ agents = ... })",
    nvim.log.levels.WARN
  )
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
---@return string? skill_catalog
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

  local catalog, catalog_error = skills_module().inject(skills)
  if catalog == nil then
    return {}, nil, catalog_error
  end
  if #catalog.truncated > 0 or #catalog.omitted > 0 then
    nvim.notify(
      string.format(
        "louiselm: injected skill catalog shortened %d description(s) and omitted %d skill(s) to fit 8000 bytes",
        #catalog.truncated,
        #catalog.omitted
      ),
      nvim.log.levels.WARN
    )
  end
  return skills, catalog.text
end

---@param config table
---@return louiselm.ui.ContextItem? instructions_context
local function configured_instructions(config)
  local filename = type(config.context) == "table" and config.context.instructions_file or nil
  return instructions_module().link(filename)
end

---@param definitions louiselm.agent.Definitions
---@return louiselm.workflow.Coordinator? workflow
---@return string? error_message
local function configured_workflow(definitions)
  local path = nvim.fs.joinpath(nvim.fn.stdpath("state"), "louiselm", "routing-evidence.json")
  return Workflow.new(definitions, path)
end

---Notify from whichever context we're called in: directly on the main loop,
---or scheduled when invoked from a fast event (e.g. a vim.system callback).
---@param message string
---@param level integer
local function notify_safely(message, level)
  if nvim.in_fast_event() then
    nvim.schedule(function()
      nvim.notify(message, level)
    end)
  else
    nvim.notify(message, level)
  end
end

---Fire a non-blocking latest-version check for every configured agent and
---warn when the installed version trails it. `Agent.check` starts async
---vim.system() calls and returns immediately, so this never delays chat or
---session creation; the notification (if any) arrives whenever the
---independent checks resolve.
---@param definitions table<string, louiselm.agent.Definition>
local function check_agent_staleness(definitions)
  for name, definition in pairs(definitions) do
    Agent.check(definition, function(result)
      if not result.outdated then
        return
      end
      notify_safely(
        string.format(
          "louiselm: %s is outdated (%s installed, %s upstream)",
          name,
          result.version,
          result.latest_version
        ),
        nvim.log.levels.WARN
      )
    end)
  end
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

---Handle a click from a LouiseLM Session winbar; clicks before registration or without an active chat are ignored.
---@param _target integer Numeric target encoded in the winbar.
---@param _clicks integer Number of consecutive clicks.
---@param _button string Mouse button name.
---@param _modifiers string Active mouse modifiers.
function M.winbar_click(_target, _clicks, _button, _modifiers) end

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

  M.winbar_click = function(target, _, button)
    if button ~= "l" or chat == nil then
      return
    end
    local mouse_position = nvim.fn.getmousepos()
    local clicked_window = type(mouse_position.winid) == "number" and mouse_position.winid or nil
    local _, click_error = chat:winbar_click(target, clicked_window)
    report_error(click_error)
  end

  ---@return louiselm.ui.Chat? value
  local function ensure_chat()
    if chat ~= nil then
      return chat
    end
    local definitions = require_configured_agents()
    if definitions == nil then
      return nil
    end
    local names = sorted_agent_names(definitions)
    check_agent_staleness(definitions)
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
    local skills, skill_catalog, skills_error = {}, nil, nil
    local instructions_context
    if configured ~= nil then
      skills, skill_catalog, skills_error = configured_skills(configured, definitions, default_policy)
      instructions_context = configured_instructions(configured)
    end
    if skills_error ~= nil then
      nvim.notify("louiselm: " .. skills_error, nvim.log.levels.ERROR)
      return nil
    end
    local workflow, workflow_error = configured_workflow(definitions)
    if workflow == nil then
      nvim.notify("louiselm: " .. (workflow_error or "could not initialize workflow routing"), nvim.log.levels.ERROR)
      return nil
    end
    chat = assert(require("louiselm.ui.chat").new(sessions, {
      agents = names,
      skills = skills,
      skill_paths = configured and configured.skills and configured.skills.paths or nil,
      skill_catalog = skill_catalog,
      instructions_context = instructions_context,
      workflow = workflow,
    }))
    return chat
  end

  nvim.api.nvim_create_autocmd("QuitPre", {
    group = nvim.api.nvim_create_augroup("louiselm.chat.quit", { clear = true }),
    callback = function()
      if chat == nil or not chat:should_block_quit() then
        return
      end
      nvim.notify(
        "louiselm: multiple sessions are open; use :LouiselmSwitchSession or :LouiselmCloseSession before quitting",
        nvim.log.levels.WARN
      )
      -- QuitPre has no cancellation API in Neovim. Make the pending :q close
      -- a temporary duplicate window instead, leaving the chat window alive.
      nvim.api.nvim_open_win(nvim.api.nvim_get_current_buf(), true, { split = "below" })
    end,
    desc = "Protect multiple louiselm sessions from accidental last-window quit",
  })

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

  nvim.api.nvim_create_user_command("LouiselmHandOff", function()
    if chat == nil then
      report_error("no chat session is open")
      return
    end
    local _, hand_off_error = chat:hand_off()
    report_error(hand_off_error)
  end, { desc = "Hand the current session's reviewed transcript off to another agent", force = true })

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

  nvim.api.nvim_create_user_command("LouiselmForensics", function()
    if chat == nil then
      report_error("no chat session is open")
      return
    end
    local started, forensics_error = chat:collect_forensics(function(path, error_message)
      if error_message ~= nil then
        report_error(error_message)
      elseif path ~= nil then
        nvim.notify("louiselm: Forensics record written to " .. path, nvim.log.levels.INFO)
      end
    end)
    if not started then
      report_error(forensics_error)
    end
  end, { desc = "Collect private Forensics for the current Session", force = true })

  nvim.api.nvim_create_user_command("LouiselmForensicsView", function(arguments)
    local path = arguments.args
    if path == "" then
      report_error("Forensics record path is required")
      return
    end
    local lines = nvim.fn.readfile(path)
    if #lines == 0 then
      report_error("could not read Forensics record")
      return
    end
    local buffer = nvim.api.nvim_create_buf(false, true)
    nvim.api.nvim_buf_set_name(buffer, "louiselm://forensics-view")
    nvim.bo[buffer].buftype = "nofile"
    nvim.bo[buffer].bufhidden = "wipe"
    nvim.bo[buffer].swapfile = false
    nvim.bo[buffer].filetype = "json"
    nvim.api.nvim_buf_set_lines(buffer, 0, -1, false, lines)
    nvim.bo[buffer].modifiable = false
    nvim.api.nvim_set_current_buf(buffer)
  end, { nargs = 1, desc = "View a Forensics record", complete = "file", force = true })

  nvim.api.nvim_create_user_command("LouiselmToMarkdown", function(arguments)
    if chat == nil then
      report_error("no chat session is open")
      return
    end
    local session_id = arguments.args ~= "" and arguments.args or nil
    -- Validate an explicitly typed session id up front: prompting first and
    -- failing afterwards makes the user enter a destination path for an
    -- export that cannot happen (louiselm-euj). With no argument there is
    -- nothing to check here -- Chat:to_markdown resolves the current session.
    if session_id ~= nil then
      local attached, attach_error = chat:is_attached(session_id)
      if not attached then
        report_error(attach_error)
        return
      end
    end
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

  nvim.api.nvim_create_user_command("LouiselmInspectTool", function()
    if chat == nil then
      report_error("no chat session is open")
      return
    end
    local _, inspect_error = chat:inspect_tool()
    report_error(inspect_error)
  end, { desc = "Inspect the raw payload under the cursor", force = true })

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

  nvim.api.nvim_create_user_command("LouiselmLimits", function(arguments)
    local current = ensure_chat()
    if current == nil then
      return
    end
    local agent_name = arguments.args ~= "" and arguments.args or nil
    local _, limits_error = current:show_limits(agent_name)
    report_error(limits_error)
  end, {
    nargs = "?",
    complete = function()
      local definitions = configured and configured.agents
      return type(definitions) == "table" and sorted_agent_names(definitions) or {}
    end,
    desc = "Inspect account limits for the active or named louiselm Agent",
    force = true,
  })

  nvim.api.nvim_create_user_command("LouiselmPermissions", function()
    local current = ensure_chat()
    if current == nil then
      return
    end
    local _, permissions_error = current:manage_permissions()
    report_error(permissions_error)
  end, { desc = "Inspect and revoke remembered louiselm permissions", force = true })

  nvim.api.nvim_create_user_command("LouiselmPickSkill", function()
    if chat == nil then
      report_error("no chat session is open")
      return
    end
    local _, pick_error = chat:pick_skill()
    report_error(pick_error)
  end, { desc = "Pick a louiselm skill to invoke", force = true })

  nvim.api.nvim_create_user_command("LouiselmPickFile", function(arguments)
    if chat == nil then
      report_error("no chat session is open")
      return
    end
    local root = arguments.args ~= "" and arguments.args or nil
    local _, pick_error = chat:pick_file(root)
    report_error(pick_error)
  end, { nargs = "?", desc = "Pick a file to queue as louiselm context", force = true })

  nvim.api.nvim_create_user_command("LouiselmMentionBuffer", function()
    if chat == nil then
      report_error("no chat session is open")
      return
    end
    local _, mention_error = chat:mention_buffer()
    report_error(mention_error)
  end, { desc = "Queue the source buffer as louiselm context", force = true })

  nvim.api.nvim_create_user_command("LouiselmSendSelection", function()
    if chat == nil then
      report_error("no chat session is open")
      return
    end
    local _, selection_error = chat:send_selection()
    report_error(selection_error)
  end, { desc = "Queue the visual selection as louiselm context", force = true })

  nvim.api.nvim_create_user_command("LouiselmInline", function()
    if inline == nil then
      local definitions = require_configured_agents()
      if definitions == nil then
        return
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
