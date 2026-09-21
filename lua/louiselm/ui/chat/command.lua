---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim
local Agent = require("louiselm.agent")
local Beads = require("louiselm.ui.beads")
local ForensicsStore = require("louiselm.forensics.store")
local ForensicsView = require("louiselm.forensics.view")
local EvidenceExport = require("louiselm.forensics.export")
local Provenance = require("louiselm.ui.provenance")
local Abandonment = require("louiselm.ui.abandonment")
local Workflow = require("louiselm.routing")
local Paths = require("louiselm.paths")
local Config = require("louiselm.config")

local M = {}
local configured ---@type table?
local dispose_registered ---@type fun()?
local staleness_generation = 0

local function session_module()
  return require("louiselm.session")
end

---@return string path
local function abandonment_path()
  return nvim.fs.joinpath(Paths.state(), "abandoned.json")
end

---@param chat louiselm.ui.Chat?
---@return louiselm.ui.AbandonedSession[] sessions
---@return string[] blocked
local function exit_snapshot(chat)
  local staged = chat and chat:staged_context() or {}
  local sessions = {}
  local blocked = {}
  for _, verdict in ipairs(session_module().exit_verdict()) do
    local session_staged = staged[verdict.session] or { contexts = 0, pending_skill = false, queued_prompt = false }
    sessions[#sessions + 1] = {
      agent = verdict.agent,
      acp_session_id = verdict.acp_session_id,
      recoverable = verdict.recoverable,
      turn_active = verdict.turn_active,
      staged = session_staged,
    }
    local reasons = {}
    if not verdict.recoverable then
      reasons[#reasons + 1] = "cannot resume"
    end
    if verdict.turn_active then
      reasons[#reasons + 1] = "turn in flight"
    end
    if session_staged.contexts > 0 then
      reasons[#reasons + 1] =
        string.format("%d queued context%s", session_staged.contexts, session_staged.contexts == 1 and "" or "s")
    end
    if session_staged.pending_skill then
      reasons[#reasons + 1] = "skill selection pending"
    end
    if session_staged.queued_prompt then
      reasons[#reasons + 1] = "prompt queued"
    end
    if #reasons > 0 then
      blocked[#blocked + 1] = verdict.agent .. ": " .. table.concat(reasons, ", ")
    end
  end
  return sessions, blocked
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
---@return louiselm.routing.Coordinator? workflow
---@return string? error_message
local function configured_workflow(definitions)
  local path = nvim.fs.joinpath(Paths.state(), "routing-evidence.json")
  return Workflow.new(definitions, path)
end

---Fire a non-blocking latest-version check for every configured agent and
---warn when the installed version trails it. `Agent.check` starts async
---vim.system() calls and returns immediately, so this never delays chat or
---session creation; one notification arrives after all checks resolve.
---@param definitions table<string, louiselm.agent.Definition>
local function check_agent_staleness(definitions)
  staleness_generation = staleness_generation + 1
  local generation = staleness_generation
  local names = sorted_agent_names(definitions)
  local remaining = #names
  local results = {} ---@type table<string, louiselm.agent.HealthResult>

  local function publish()
    if generation ~= staleness_generation then
      return
    end
    local lines, commands = {}, {}
    local outdated = 0
    for _, name in ipairs(names) do
      local result = results[name]
      if result ~= nil and result.outdated then
        outdated = outdated + 1
        lines[#lines + 1] = string.format(
          "louiselm: %s is outdated (%s installed, %s upstream)",
          name,
          result.version,
          result.latest_version
        )
        local upgrade = definitions[name].upgrade
        if type(upgrade) == "string" then
          lines[#lines + 1] = "  Manual update: " .. upgrade:gsub("\n", "\n  ")
        elseif upgrade ~= nil then
          local argv = {}
          for _, argument in ipairs(upgrade) do
            argv[#argv + 1] = nvim.fn.shellescape(argument)
          end
          local command = table.concat(argv, " ")
          commands[#commands + 1] = command
          lines[#lines + 1] = "  Upgrade: " .. command
        else
          lines[#lines + 1] =
            "  No upgrade instructions configured. Check this agent's installation instructions before updating."
        end
      end
    end
    if outdated > 1 and #commands == outdated then
      lines[#lines + 1] = "Upgrade all: " .. table.concat(commands, " && ")
    end
    if #lines > 0 then
      nvim.notify(table.concat(lines, "\n"), nvim.log.levels.WARN)
    end
  end

  for _, name in ipairs(names) do
    local finished = false
    ---@param result? louiselm.agent.HealthResult
    local function complete(result)
      if finished or generation ~= staleness_generation then
        return
      end
      finished = true
      results[name] = result
      remaining = remaining - 1
      if remaining == 0 then
        if nvim.in_fast_event() then
          nvim.schedule(publish)
        else
          publish()
        end
      end
    end
    local _, check_error = Agent.check(definitions[name], complete)
    if check_error ~= nil then
      complete()
    end
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
  configured = nvim.deepcopy(config)
  return true
end

---Surface and clear recovery guidance left by the previous editor exit.
---@return boolean surfaced True when a valid breadcrumb was reported.
function M.surface_abandonment()
  local breadcrumb, breadcrumb_error = Abandonment.consume(abandonment_path())
  if breadcrumb ~= nil then
    nvim.notify(breadcrumb, nvim.log.levels.WARN)
    return true
  end
  if breadcrumb_error ~= nil then
    nvim.notify("louiselm: " .. breadcrumb_error, nvim.log.levels.WARN)
  end
  return false
end

---Handle a click from a LouiseLM Session winbar; clicks before registration or without an active chat are ignored.
---@param _target integer Numeric target encoded in the winbar.
---@param _clicks integer Number of consecutive clicks.
---@param _button string Mouse button name.
---@param _modifiers string Active mouse modifiers.
function M.winbar_click(_target, _clicks, _button, _modifiers) end

-- Winbar/statusline `%{N}@{Function}@` click syntax requires {Function} to be
-- a plain dotted name Vim can resolve at click time -- a call expression like
-- `v:lua.require('...').winbar_click` embedded in that field is never
-- invoked (louiselm-7ios: silently inert, no error). A stable global forwards
-- to the current `M.winbar_click`, which `M.register()` below reassigns.
_G.__louiselm_winbar_click = function(...)
  return M.winbar_click(...)
end

---Register the interactive chat command.
---@return boolean registered Always true after the command is registered.
function M.register()
  if dispose_registered ~= nil then
    dispose_registered()
    dispose_registered = nil
  end
  local chat
  local usage ---@type louiselm.ui.UsageView?
  local inline
  local export_cancel ---@type fun()?
  local disposed = false
  local restore_mousemove ---@type boolean?
  local mousemove_observer ---@type integer?

  ---@param message string?
  local function report_error(message)
    if message ~= nil then
      nvim.notify("louiselm: " .. message, nvim.log.levels.ERROR)
    end
  end

  ---@param method "switch_session"|"hand_off"|"inspect_tool"|"close_session"|"cancel"|"session_options"|"pick_skill"|"mention_buffer"|"send_selection"|"mention_diagnostics"
  ---@return fun()
  local function chat_command(method)
    return function()
      if chat == nil then
        report_error("no chat session is open")
        return
      end
      local _, command_error = chat[method](chat)
      report_error(command_error)
    end
  end

  local function open_tutor()
    local paths = nvim.api.nvim_get_runtime_file("docs/tutorial.md", false)
    local path = paths[1]
    if path == nil then
      report_error("could not find the bundled Tutor")
      return
    end

    local opened, open_error = pcall(nvim.api.nvim_cmd, { cmd = "edit", args = { path } }, {})
    if not opened then
      report_error(tostring(open_error))
      return
    end
    local buffer = nvim.api.nvim_get_current_buf()
    nvim.api.nvim_set_option_value("filetype", "markdown", { buf = buffer })
    nvim.api.nvim_set_option_value("modifiable", false, { buf = buffer })
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
      nvim.notify("louiselm: skill injection is unavailable: " .. skills_error, nvim.log.levels.WARN)
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
      attention = Config.enabled(configured, "attention"),
      workflows = Config.enabled(configured, "workflows"),
      beads = Config.enabled(configured, "beads"),
    }))
    -- The command registration owns the one interactive UI and its global option.
    -- Headless APIs and independently constructed Chat objects leave options alone.
    restore_mousemove = nvim.o.mousemoveevent
    nvim.o.mousemoveevent = true
    mousemove_observer = nvim.api.nvim_create_autocmd("OptionSet", {
      pattern = "mousemoveevent",
      callback = function()
        restore_mousemove = nil -- A later user change supersedes our saved value.
      end,
    })
    return chat
  end

  local exit_group = nvim.api.nvim_create_augroup("louiselm.exit", { clear = true })
  nvim.api.nvim_create_autocmd("ExitPre", {
    group = exit_group,
    callback = function()
      local sessions, blocked = exit_snapshot(chat)
      if #blocked == 0 then
        return
      end
      nvim.notify(
        string.format(
          "louiselm: %d live Session%s (%s). :qa to exit anyway.",
          #sessions,
          #sessions == 1 and "" or "s",
          table.concat(blocked, "; ")
        ),
        nvim.log.levels.WARN
      )
      -- ExitPre has no cancellation API in Neovim. Make the pending :q close
      -- a temporary duplicate window instead, leaving the chat window alive.
      nvim.api.nvim_open_win(nvim.api.nvim_get_current_buf(), true, { split = "below" })
    end,
    desc = "Protect live louiselm Sessions from accidental abandonment",
  })

  nvim.api.nvim_create_autocmd("VimLeavePre", {
    group = exit_group,
    callback = function()
      local sessions = exit_snapshot(chat)
      if #sessions > 0 then
        Abandonment.write(abandonment_path(), sessions)
      end
      session_module().dispose_all()
    end,
    desc = "Dispose live louiselm Sessions and record restart guidance",
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

  local function inspect_bead()
    if not Config.enabled(configured, "beads") then
      report_error("Beads is disabled; set beads.enabled = true and run :checkhealth louiselm")
      return
    end
    local buffer = chat and chat:buffer() or nil
    if buffer == nil or nvim.api.nvim_get_current_buf() ~= buffer then
      report_error("no chat session is open")
      return
    end
    local view = chat.current_id and chat.views[chat.current_id]
    local state = view and view.session:inspect()
    local _, inspect_error = Beads.inspect(buffer, {
      cwd = state and state.working_dir or nil,
      is_active = function()
        return chat ~= nil and chat:buffer() == buffer
      end,
      on_error = report_error,
      sibling_roots = configured and configured.beads and configured.beads.sibling_roots or nil,
    })
    report_error(inspect_error)
  end

  local function inspect_provenance()
    local buffer = nvim.api.nvim_get_current_buf()
    local _, inspect_error = Provenance.inspect(buffer, {
      definitions = configured and configured.agents or {},
      beads = Config.enabled(configured, "beads"),
      on_error = report_error,
    })
    report_error(inspect_error)
  end

  nvim.api.nvim_create_user_command("LouiselmChat", function()
    open_chat()
  end, { desc = "Open the louiselm chat buffer", force = true })

  nvim.api.nvim_create_user_command("LouiselmTutor", open_tutor, {
    desc = "Open the LouiseLM onboarding Tutor",
    force = true,
  })

  nvim.api.nvim_create_user_command("LouiselmUsage", function()
    if usage ~= nil then
      usage:dispose()
    end
    local err
    usage, err = require("louiselm.ui.usage").open()
    report_error(err and err.message)
  end, { desc = "Explore recorded usage: UTC ranges, joint filters, groups and turn details", force = true })

  nvim.api.nvim_create_user_command("LouiselmInspectBead", inspect_bead, {
    desc = "Inspect the Beads issue under the cursor",
    force = true,
  })

  nvim.api.nvim_create_user_command("LouiselmInspectProvenance", inspect_provenance, {
    desc = "Inspect commit or issue Provenance under the cursor",
    force = true,
  })

  nvim.api.nvim_create_user_command("LouiselmProvenanceDecisions", function()
    local _, decisions_error =
      Provenance.show_decisions({ beads = Config.enabled(configured, "beads"), on_error = report_error })
    report_error(decisions_error)
  end, { desc = "Show the Provenance Decision index", force = true })

  nvim.api.nvim_create_user_command("LouiselmSessionNew", function()
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

  nvim.api.nvim_create_user_command("LouiselmResumePark", function()
    if not Config.enabled(configured, "workflows") then
      report_error("workflows are disabled; set workflows.enabled = true and run :checkhealth louiselm")
      return
    end
    local current = ensure_chat()
    if current == nil then
      return
    end
    local _, resume_error = current:resume_park()
    report_error(resume_error)
  end, { desc = "Resume a durable cold-Parked Run", force = true })

  nvim.api.nvim_create_user_command("LouiselmPark", function()
    if not Config.enabled(configured, "workflows") then
      report_error("workflows are disabled; set workflows.enabled = true and run :checkhealth louiselm")
      return
    end
    local current = ensure_chat()
    if current == nil then
      return
    end
    local _, park_error = current:park()
    report_error(park_error)
  end, { desc = "Cold-Park the current louiselm Session", force = true })

  nvim.api.nvim_create_user_command(
    "LouiselmSessionSwitch",
    chat_command("switch_session"),
    { desc = "Switch between louiselm sessions", force = true }
  )

  nvim.api.nvim_create_user_command("LouiselmSessionOverview", function()
    local current = ensure_chat()
    if current == nil then
      report_error("could not open chat")
      return
    end
    local _, overview_error = current:session_overview()
    report_error(overview_error)
  end, {
    desc = "Open the current Session's file overview; closes when another Session gains focus",
    force = true,
  })

  nvim.api.nvim_create_user_command(
    "LouiselmHandOff",
    chat_command("hand_off"),
    { desc = "Hand the current session's reviewed transcript off to another agent", force = true }
  )

  nvim.api.nvim_create_user_command("LouiselmSessionRename", function()
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

  nvim.api.nvim_create_user_command("LouiselmForensics", function(arguments)
    local function report_path(path, error_message)
      if error_message ~= nil then
        report_error(error_message)
      elseif path ~= nil then
        nvim.notify("louiselm: Forensics record written to " .. path, nvim.log.levels.INFO)
      end
    end
    -- An explicit subject diagnoses any live Session, including one whose own
    -- chat is the thing that is broken. Only the bare form needs a chat, and
    -- only because that is what supplies the diagnosing Session and the queue.
    if #arguments.fargs > 0 then
      if #arguments.fargs ~= 2 then
        report_error("use :LouiselmForensics AGENT ACP_SESSION_ID, or no argument for the current Session")
        return
      end
      local started, forensics_error =
        session_module().collect_forensics(arguments.fargs[1], arguments.fargs[2], nil, report_path)
      if not started then
        report_error(forensics_error)
      end
      return
    end
    if chat == nil then
      report_error("no chat session is open; use :LouiselmForensics AGENT ACP_SESSION_ID to diagnose another Session")
      return
    end
    local started, forensics_error = chat:collect_forensics(report_path)
    if not started then
      report_error(forensics_error)
    end
  end, {
    nargs = "*",
    desc = "Collect private Forensics for the current or a named Session",
    force = true,
  })

  nvim.api.nvim_create_user_command("LouiselmForensicsView", function(arguments)
    local path = arguments.args
    if path == "" then
      if chat == nil then
        report_error("no chat session is open")
        return
      end
      local latest, latest_error = chat:latest_forensics_path()
      if latest == nil then
        report_error(latest_error)
        return
      end
      path = latest
    end
    local store, store_error = ForensicsStore.new(nvim.fs.dirname(path))
    if store == nil then
      report_error(store_error)
      return
    end
    local inspection, inspection_error = store:inspect(path)
    if inspection == nil then
      report_error(inspection_error)
      return
    end
    local rendered_ok, rendered = pcall(ForensicsView.lines, inspection)
    if not rendered_ok then
      report_error("could not render Forensics inspection")
      return
    end
    local buffer = nvim.api.nvim_create_buf(false, true)
    nvim.api.nvim_buf_set_name(buffer, "louiselm://forensics-view")
    nvim.bo[buffer].buftype = "nofile"
    nvim.bo[buffer].bufhidden = "wipe"
    nvim.bo[buffer].swapfile = false
    nvim.bo[buffer].filetype = "markdown"
    nvim.api.nvim_buf_set_lines(buffer, 0, -1, false, rendered)
    nvim.bo[buffer].modifiable = false
    nvim.api.nvim_set_current_buf(buffer)
  end, { nargs = "?", desc = "View a Forensics record", complete = "file", force = true })

  nvim.api.nvim_create_user_command("LouiselmForensicsExport", function(arguments)
    if #arguments.fargs < 3 then
      report_error("use :LouiselmForensicsExport RECORD OUTPUT observation:FIELD or source:INDEX:FIRST:LAST ...")
      return
    end
    if export_cancel then
      report_error("an Evidence export is already running")
      return
    end
    local selections = {}
    for index = 3, #arguments.fargs do
      selections[#selections + 1] = arguments.fargs[index]
    end
    local cancel, export_error = EvidenceExport.write(
      arguments.fargs[1],
      arguments.fargs[2],
      selections,
      function(path, err)
        export_cancel = nil
        if disposed then
          return
        end
        if path then
          nvim.notify(
            "louiselm: Evidence export written to " .. path .. "; inspect item states for omitted evidence",
            nvim.log.levels.INFO
          )
        end
        report_error(err)
      end
    )
    export_cancel = cancel
    report_error(export_error)
  end, { nargs = "+", desc = "Export selected redacted Forensics evidence to a new file", force = true })

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

  nvim.api.nvim_create_user_command(
    "LouiselmInspectTool",
    chat_command("inspect_tool"),
    { desc = "Inspect the raw payload under the cursor", force = true }
  )

  nvim.api.nvim_create_user_command(
    "LouiselmSessionClose",
    chat_command("close_session"),
    { desc = "Close the current louiselm session", force = true }
  )

  nvim.api.nvim_create_user_command(
    "LouiselmCancel",
    chat_command("cancel"),
    { desc = "Cancel the current louiselm turn", force = true }
  )

  nvim.api.nvim_create_user_command(
    "LouiselmSessionOptions",
    chat_command("session_options"),
    { desc = "Inspect session options; configure while idle", force = true }
  )

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

  nvim.api.nvim_create_user_command(
    "LouiselmPickSkill",
    chat_command("pick_skill"),
    { desc = "Pick a louiselm skill to invoke", force = true }
  )

  nvim.api.nvim_create_user_command("LouiselmPickFile", function(arguments)
    if chat == nil then
      report_error("no chat session is open")
      return
    end
    local root = arguments.args ~= "" and arguments.args or nil
    local _, pick_error = chat:pick_file(root)
    report_error(pick_error)
  end, { nargs = "?", desc = "Pick a file to queue as louiselm context", force = true })

  nvim.api.nvim_create_user_command(
    "LouiselmMentionBuffer",
    chat_command("mention_buffer"),
    { desc = "Queue the source buffer as louiselm context", force = true }
  )

  nvim.api.nvim_create_user_command(
    "LouiselmSendSelection",
    chat_command("send_selection"),
    { desc = "Queue the visual selection as louiselm context", force = true }
  )

  nvim.api.nvim_create_user_command(
    "LouiselmDiagnostics",
    chat_command("mention_diagnostics"),
    { desc = "Queue the source buffer's diagnostics as louiselm context", force = true }
  )

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
  dispose_registered = function()
    disposed = true
    if usage ~= nil then
      usage:dispose()
    end
    if mousemove_observer ~= nil then
      nvim.api.nvim_del_autocmd(mousemove_observer)
    end
    if restore_mousemove ~= nil then
      nvim.o.mousemoveevent = restore_mousemove
    end
    if export_cancel then
      export_cancel()
    end
    staleness_generation = staleness_generation + 1
    if chat ~= nil then
      chat:dispose()
      chat.api:dispose()
      chat = nil
    end
    if inline ~= nil then
      inline:dispose()
      inline = nil
    end
  end
  return true
end

return M
