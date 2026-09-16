---@class louiselm.health.Configuration
---@field config unknown Validated user configuration.
---@field schema louiselm.schema.Schema Schema used to validate the configuration.
---@field preflight_handle? louiselm.PreflightRead Owned pending artifact read.
---@field preflight_items? louiselm.PostureHealthItem[] Explicitly selected snapshot, never live state.
---@field preflight_error? string Fixed read/validation diagnostic.
---@field preflight_command? boolean Whether this lifecycle registered the preview command.

local Agent = require("louiselm.agent")
local Schema = require("louiselm.schema")
local Skills = require("louiselm.skills")
local Preflight = require("louiselm.preflight")
local Config = require("louiselm.config")

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

---Build the human-readable report for an agent health result.
---@param result louiselm.agent.HealthResult
---@return string message
---@return boolean ok Whether the version check itself succeeded.
---@return boolean outdated Whether the installed version trails the configured latest check.
local function describe_agent_result(result)
  local message = result.command
  if result.version ~= nil then
    message = message .. " — " .. result.version
  end
  if not result.ok then
    return message .. ": " .. (result.error or "version check failed"), false, false
  end
  if result.outdated then
    message = message .. " (latest " .. (result.latest_version or "unknown") .. " available)"
  end
  return message, true, result.outdated == true
end

---@param result louiselm.agent.HealthResult
local function report_agent_result(result)
  local message, ok, outdated = describe_agent_result(result)
  if not ok then
    report(message, false)
  elseif outdated then
    nvim().health.warn(message)
  else
    report(message, true)
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
    local message, ok, outdated = describe_agent_result(result)
    local level = editor.log.levels.INFO
    if not ok then
      level = editor.log.levels.ERROR
    elseif outdated then
      level = editor.log.levels.WARN
    end
    editor.notify("louiselm health: " .. message, level)
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

---@param paths unknown
---@return boolean
local function has_relative_path(paths)
  if type(paths) ~= "table" then
    return false
  end
  local editor = nvim()
  for _, path in ipairs(paths) do
    if type(path) == "string" then
      local normalized = editor.fs.normalize(path)
      if editor.fs.abspath(normalized) ~= normalized then
        return true
      end
    end
  end
  return false
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
    if definition.capabilities ~= nil and #definition.capabilities > 0 then
      nvim().health.info("agent " .. name .. " capabilities: " .. table.concat(definition.capabilities, ", "))
    else
      nvim().health.info("agent " .. name .. " capabilities: none declared")
    end
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
    nvim().health.info(
      "Local skill invocation is disabled; set skills.policy = 'native' or 'inject', or agents.<name>.skills.policy. Configure skills.paths; local discovery needs lyaml. See :help louiselm-config-skills."
    )
    return
  end

  local paths = configured_skill_paths(config.config)
  if paths == nil then
    nvim().health.info("no skill paths configured")
    return
  end

  local cwd = nvim().fn.getcwd()
  if has_relative_path(paths) then
    nvim().health.info("relative skill paths resolve against Neovim's current working directory: " .. cwd)
  end
  local skills, errors = Skills.discover(paths, cwd)
  for _, error_item in ipairs(errors) do
    local message = error_item.path .. ": " .. (error_item.detail or error_item.message)
    if error_item.severity == "warning" then
      nvim().health.warn(message)
    else
      nvim().health.warn(message)
    end
  end
  report(string.format("discovered %d skill%s", #skills, #skills == 1 and "" or "s"), true)
  if inject_enabled then
    local catalog, catalog_error = Skills.inject(skills)
    if catalog == nil then
      nvim().health.warn("injected skill catalog: " .. (catalog_error or "could not build catalog"))
      return
    end
    nvim().health.info(string.format("injected skill catalog: %d/8000 bytes", #catalog.text))
    if #catalog.truncated > 0 then
      nvim().health.warn("injected skill catalog shortened descriptions: " .. table.concat(catalog.truncated, ", "))
    end
    if #catalog.omitted > 0 then
      nvim().health.warn("injected skill catalog omitted skills: " .. table.concat(catalog.omitted, ", "))
    end
  end
end

---@param config louiselm.health.Configuration
local function check_capture(config)
  if not Config.enabled(config.config, "capture") then
    nvim().health.info(
      "Desktop capture is disabled; set capture.enabled = true. Requires a recorder and louiselm-capture; see :help louiselm-config-capture. Audio is stored locally; receiver, transcription and push are separate service opt-ins."
    )
    return
  end
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
      nvim().health.warn(
        item.label
          .. " is not executable: "
          .. item.command
          .. "; install it manually, then rerun :checkhealth louiselm (:help louiselm-config-capture)"
      )
    end
  end
end

local function check_optional(config)
  local health = nvim().health
  if Config.enabled(config.config, "beads") then
    if nvim().fn.executable("br") == 1 then
      local editor = nvim()
      local workspace = editor.fs.find(".beads", { path = editor.fn.getcwd(), upward = true, type = "directory" })[1]
      if workspace == nil then
        health.warn(
          "Beads inspection is blocked here: select an existing .beads workspace; never initialized automatically"
        )
      else
        health.ok("Beads inspection: br is executable and an existing workspace is available")
      end
    else
      health.warn("Beads inspection is blocked: install br manually, then rerun :checkhealth louiselm")
    end
    health.info(
      "bvr is needed only for Beads Provenance correlations; basic inspection needs only br. See :help louiselm-config-beads."
    )
  else
    health.info(
      "Beads inspection is disabled; set beads.enabled = true. Requires br and an existing .beads workspace; see :help louiselm-config-beads."
    )
  end
  for _, feature in ipairs({ "attention", "workflows" }) do
    if not Config.enabled(config.config, feature) then
      health.info(
        feature
          .. " is disabled; set "
          .. feature
          .. ".enabled = true. Requires the corresponding local service capability; see :help louiselm-optional-capabilities."
      )
    else
      health.info(
        feature
          .. " is enabled in the editor. Configure the service separately; this setting cannot start or reconfigure a shared daemon. See :help louiselm-optional-capabilities."
      )
      local editor = nvim()
      local root = editor.env.LOUISELM_CAPTURE_STATE_DIR
      if root == nil or root == "" then
        root = editor.env.XDG_STATE_HOME
      end
      if root == nil or root == "" then
        root = editor.fs.joinpath(editor.fn.expand("~"), ".local", "state")
      end
      local socket = feature == "attention" and "attention.sock" or "run.sock"
      local path = editor.fs.joinpath(root, "louiselm", "workflow", socket)
      local stat = editor.uv.fs_stat(path)
      if stat == nil or stat.type ~= "socket" then
        health.warn(
          feature
            .. " is blocked: no service socket at "
            .. path
            .. "; enable the service capability and start it manually"
        )
      else
        health.info(feature .. " service socket exists; authentication and operation readiness are checked on use")
      end
      if feature == "workflows" then
        for _, prerequisite in ipairs({ "attention", "beads" }) do
          if not Config.enabled(config.config, prerequisite) then
            health.warn("Cold Park is blocked: set " .. prerequisite .. ".enabled = true explicitly")
          end
        end
      end
    end
  end
  local value = type(config.config) == "table" and config.config.skills or nil
  local management = type(value) == "table" and value.management or nil
  if type(management) ~= "table" or management.enabled ~= true then
    health.info(
      "Trusted skill management is disabled; set skills.management.enabled = true. Requires louiselm-skills for :LouiselmPreflight; see :help louiselm-optional-capabilities. Installation does not grant Skill Admission or Verified posture."
    )
  elseif nvim().fn.executable("louiselm-skills") ~= 1 then
    health.warn(
      "Trusted skill management is blocked: install louiselm-skills manually; see skills-core/README.md, then rerun :checkhealth louiselm"
    )
  else
    health.ok(
      "Trusted skill management: louiselm-skills is executable; :LouiselmPreflight inspects explicitly selected artifacts"
    )
    health.info(
      "Packaging and Skill Admission remain explicit louiselm-skills CLI operations; no editor approval or verified-launch integration is implied. See skills-core/README.md."
    )
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
  M.reset()
  configuration = { config = nvim().deepcopy(config), schema = schema }
  return true
end

---Forget the configuration used by the healthcheck.
---@return boolean cleared Always true.
function M.reset()
  if configuration ~= nil then
    if configuration.preflight_handle ~= nil then
      configuration.preflight_handle.dispose()
    end
    if configuration.preflight_command then
      nvim().api.nvim_del_user_command("LouiselmPreflight")
    end
  end
  configuration = nil
  return true
end

---Select a prospective snapshot for health, replacing any pending read/result.
---The health configuration owns cancellation; reset/reconfigure suppresses late
---completion. This does not change any configured Agent's launch path.
---@param options louiselm.PreflightOptions Explicit input selection, only read.
---@param callback fun(ok: boolean, error_message: string?) Scheduled completion; suppressed on reset/replacement.
---@return boolean started
---@return string? error_message Fixed validation/spawn error; callback is not called.
function M.preview(options, callback)
  local owner = configuration
  if owner == nil then
    return false, "configure LouiseLM before selecting a preflight snapshot"
  end
  local config = type(owner.config) == "table" and owner.config or {}
  local management = config.skills and config.skills.management
  if management == nil or management.enabled ~= true then
    return false,
      "trusted skill management is disabled; set skills.management.enabled = true and run :checkhealth louiselm"
  end
  if type(callback) ~= "function" then
    return false, "preflight requires a completion callback"
  end
  if owner.preflight_handle ~= nil then
    owner.preflight_handle.dispose()
  end
  owner.preflight_handle, owner.preflight_items, owner.preflight_error = nil, nil, nil
  local handle, read_error = Preflight.read(options, function(preview, err)
    if configuration ~= owner then
      return
    end
    owner.preflight_handle = nil
    if preview ~= nil then
      owner.preflight_items, owner.preflight_error = Preflight.health_items(preview)
    else
      owner.preflight_error = err
    end
    callback(owner.preflight_items ~= nil, owner.preflight_error)
  end)
  if handle == nil then
    owner.preflight_error = read_error
    return false, read_error
  end
  owner.preflight_handle = handle
  return true
end

---Register explicit file selection and open health after the asynchronous read.
---@return boolean registered False if setup has not registered a configuration.
function M.register()
  if configuration == nil then
    return false
  end
  local editor = nvim()
  editor.api.nvim_create_user_command(
    "LouiselmPreflight",
    function(args)
      local files = args.fargs
      if #files ~= 1 and #files ~= 2 and #files ~= 4 then
        editor.notify(
          "Usage: LouiselmPreflight request [manifest [prior-request prior-manifest]]",
          editor.log.levels.ERROR
        )
        return
      end
      local started, err = M.preview({
        request = files[1],
        manifest = files[2],
        previous_request = files[3],
        previous_manifest = files[4],
      }, function()
        -- The reader schedules this callback, and reset/replacement suppresses it.
        editor.api.nvim_cmd({ cmd = "checkhealth", args = { "louiselm" } }, {})
      end)
      if not started then
        editor.notify(err, editor.log.levels.ERROR)
      end
    end,
    { nargs = "+", complete = "file", force = true, desc = "Inspect prospective artifacts without launching an Agent" }
  )
  configuration.preflight_command = true
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
  check_optional(configuration)
  report(
    "turn recording requires sqlite3 >= 3.38 with JSON support on PATH (features verified at each write)",
    nvim().fn.executable("sqlite3") == 1
  )
  health.info(
    "Direct vendor launch: no LouiseLM Verified posture exists. A wrapper does not establish Verified posture."
  )
  if configuration.preflight_handle ~= nil then
    health.info("Prospective artifact preflight is pending; no selected snapshot is available yet")
  elseif configuration.preflight_error ~= nil then
    health.warn(configuration.preflight_error)
  elseif configuration.preflight_items ~= nil then
    health.info("Selected prospective snapshot, not live state; refresh with :LouiselmPreflight before relying on it")
    for _, item in ipairs(configuration.preflight_items) do
      health[item.level == "error" and "warn" or item.level](item.message)
    end
  elseif
    type(configuration.config) == "table"
    and configuration.config.skills ~= nil
    and configuration.config.skills.management ~= nil
    and configuration.config.skills.management.enabled == true
  then
    health.info("No prospective snapshot selected; use :LouiselmPreflight with explicit request and manifest files")
  end
  return true
end

return M
