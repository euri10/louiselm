local MiniTest = require("mini.test")

local Protocol = require("louiselm.acp.protocol")
local Command = require("louiselm.ui.chat.command")
local Louiselm = require("louiselm")

local T = MiniTest.new_set()

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim
local project_root = nvim.fn.getcwd()

local function has_chat_command()
  return nvim.api.nvim_get_commands({ builtin = false }).LouiselmChat ~= nil
end

local function fake_process()
  local process = { writes = {} }
  local original_system = nvim.system
  rawset(nvim, "system", function(command, options, on_exit)
    process.command = command
    process.options = options
    process.on_exit = on_exit
    process.handle = {
      write = function(_, data)
        process.writes[#process.writes + 1] = data
      end,
      kill = function() end,
      is_closing = function()
        return false
      end,
    }
    return process.handle
  end)
  return process, original_system
end

local function respond(process, id, result)
  process.options.stdout(nil, assert(Protocol.encode(Protocol.response(id, result))) .. "\n")
end

local function mock_definition()
  return {
    command = nvim.v.progpath,
    args = {
      "--headless",
      "--noplugin",
      "-i",
      "NONE",
      "-u",
      project_root .. "/tests/mock/init.lua",
      "-c",
      "lua require('louiselm.dev.mock_agent').run()",
    },
  }
end

local function restore_environment(name, value)
  nvim.env[name] = value
end

local function delete_chat_buffers()
  for _, buffer in ipairs(nvim.api.nvim_list_bufs()) do
    if nvim.api.nvim_buf_is_valid(buffer) and nvim.api.nvim_buf_get_name(buffer):match("^louiselm://") then
      nvim.api.nvim_buf_delete(buffer, { force = true })
    end
  end
end

---@param buffer integer
---@param needle string
---@return boolean
local function buffer_contains(buffer, needle)
  for _, line in ipairs(nvim.api.nvim_buf_get_lines(buffer, 0, -1, false)) do
    if line:find(needle, 1, true) ~= nil then
      return true
    end
  end
  return false
end

T["command"] = MiniTest.new_set()

T["command"]["minimal init exposes the canonical chat command"] = function()
  local commands = nvim.api.nvim_get_commands({ builtin = false })
  MiniTest.expect.equality(has_chat_command(), true)
  MiniTest.expect.equality(commands.LouiselmCancel ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmNewSession ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmResume ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmResume.bang, true)
  MiniTest.expect.equality(commands.LouiselmSwitchSession ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmRenameSession ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmCloseSession ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmSessionId ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmToMarkdown ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmSessionOptions ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmPermissions ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmInline ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmPickSkill ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmPickFile ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmMentionBuffer ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmSendSelection ~= nil, true)
  MiniTest.expect.equality(nvim.api.nvim_get_commands({ builtin = false }).LouisLMChat, nil)
  MiniTest.expect.equality(nvim.api.nvim_get_commands({ builtin = false }).LuiseLmChat, nil)
end

T["command"]["reports when no session id is available"] = function()
  local original_notify = nvim.notify
  local notification
  rawset(nvim, "notify", function(message, level)
    notification = { message = message, level = level }
  end)
  Command.register()

  nvim.api.nvim_cmd({ cmd = "LouiselmSessionId", args = {} }, {})

  rawset(nvim, "notify", original_notify)
  MiniTest.expect.equality(notification, {
    message = "louiselm: no chat session is open",
    level = nvim.log.levels.ERROR,
  })
end

T["command"]["copies and reports the current ACP session id"] = function()
  Command.configure({ agents = { codex = { command = "codex-agent", args = {} } } })
  local process, original_system = fake_process()
  local original_notify = nvim.notify
  local original_clipboard = nvim.fn.getreg("+")
  local notification
  rawset(nvim, "notify", function(message, level)
    notification = { message = message, level = level }
  end)
  Command.register()
  nvim.api.nvim_cmd({ cmd = "LouiselmChat", args = {} }, {})
  respond(process, 1, { protocolVersion = 1, agentCapabilities = {} })
  respond(process, 2, { sessionId = "prior-acp" })

  nvim.api.nvim_cmd({ cmd = "LouiselmSessionId", args = {} }, {})

  MiniTest.expect.equality(nvim.fn.getreg("+"), "codex/prior-acp")
  MiniTest.expect.equality(notification, {
    message = "louiselm: copied session id codex/prior-acp",
    level = nvim.log.levels.INFO,
  })
  nvim.fn.setreg("+", original_clipboard)
  rawset(nvim, "notify", original_notify)
  rawset(nvim, "system", original_system)
  Command.configure(nil)
  delete_chat_buffers()
end

T["command"]["reports when no chat session is open for LouiselmToMarkdown"] = function()
  local original_notify = nvim.notify
  local notification
  rawset(nvim, "notify", function(message, level)
    notification = { message = message, level = level }
  end)
  Command.register()

  nvim.api.nvim_cmd({ cmd = "LouiselmToMarkdown", args = {} }, {})

  rawset(nvim, "notify", original_notify)
  MiniTest.expect.equality(notification, {
    message = "louiselm: no chat session is open",
    level = nvim.log.levels.ERROR,
  })
end

T["command"]["reports when no chat session is open for the context-picker commands"] = function()
  for _, name in ipairs({ "LouiselmPickSkill", "LouiselmPickFile", "LouiselmMentionBuffer", "LouiselmSendSelection" }) do
    local original_notify = nvim.notify
    local notification
    rawset(nvim, "notify", function(message, level)
      notification = { message = message, level = level }
    end)
    Command.register()

    nvim.api.nvim_cmd({ cmd = name, args = {} }, {})

    rawset(nvim, "notify", original_notify)
    MiniTest.expect.equality(notification, {
      message = "louiselm: no chat session is open",
      level = nvim.log.levels.ERROR,
    })
  end
end

T["command"]["queues the source buffer as context through LouiselmMentionBuffer and sends it with the next prompt"] = function()
  local process, original_system = fake_process()
  assert(Louiselm.setup({ agents = { claude = { command = "claude-agent-acp", args = {} } } }))
  Command.register()
  nvim.api.nvim_cmd({ cmd = "LouiselmChat", args = {} }, {})
  respond(process, 1, { protocolVersion = 1, agentCapabilities = {} })
  respond(process, 2, { sessionId = "mention-acp" })

  nvim.api.nvim_cmd({ cmd = "LouiselmMentionBuffer", args = {} }, {})
  local buffer = nvim.api.nvim_get_current_buf()
  MiniTest.expect.equality(buffer_contains(buffer, "[context: buffer:"), true)

  local prompt_line = nvim.api.nvim_buf_line_count(buffer) - 1
  local line = nvim.api.nvim_buf_get_lines(buffer, prompt_line, prompt_line + 1, false)[1]
  nvim.api.nvim_buf_set_lines(buffer, prompt_line, prompt_line + 1, false, { line .. "hello" })
  local submit
  for _, mapping in ipairs(nvim.api.nvim_buf_get_keymap(buffer, "i")) do
    if mapping.desc == "Submit louiselm prompt" then
      submit = mapping.callback
      break
    end
  end
  assert(type(submit) == "function")
  nvim.api.nvim_buf_call(buffer, submit)
  local prompt = assert(Protocol.decode(process.writes[3]:sub(1, -2))).params.prompt

  rawset(nvim, "system", original_system)
  Command.configure(nil)

  MiniTest.expect.equality(prompt[1].text:find("buffer", 1, true) ~= nil, true)
  MiniTest.expect.equality(prompt[2], { type = "text", text = "hello" })
  delete_chat_buffers()
end

T["command"]["prompts for a path and exports the current session's transcript to a generated default"] = function()
  Command.configure({ agents = { codex = { command = "codex-agent", args = {} } } })
  local process, original_system = fake_process()
  local original_input = nvim.ui.input
  local input_prompt
  nvim.ui.input = function(options, callback)
    input_prompt = options.prompt
    callback("")
  end
  local original_notify = nvim.notify
  local notification
  rawset(nvim, "notify", function(message, level)
    notification = { message = message, level = level }
  end)
  Command.register()
  nvim.api.nvim_cmd({ cmd = "LouiselmChat", args = {} }, {})
  respond(process, 1, { protocolVersion = 1, agentCapabilities = {} })
  respond(process, 2, { sessionId = "acp-1" })

  nvim.api.nvim_cmd({ cmd = "LouiselmToMarkdown", args = {} }, {})

  nvim.ui.input = original_input
  rawset(nvim, "notify", original_notify)
  rawset(nvim, "system", original_system)
  Command.configure(nil)

  MiniTest.expect.equality(input_prompt, "louiselm markdown path (blank for default): ")
  MiniTest.expect.equality(notification ~= nil and notification.level, nvim.log.levels.INFO)
  local path = notification and notification.message:match("^louiselm: exported transcript to (.+)$")
  MiniTest.expect.equality(type(path), "string")
  MiniTest.expect.equality(nvim.fn.filereadable(path), 1)
  if type(path) == "string" then
    nvim.fn.delete(path)
  end
  delete_chat_buffers()
end

T["command"]["passes an explicit session id argument to LouiselmToMarkdown"] = function()
  Command.configure({ agents = { codex = { command = "codex-agent", args = {} } } })
  local process, original_system = fake_process()
  local original_input = nvim.ui.input
  nvim.ui.input = function(_, callback)
    callback("")
  end
  local original_notify = nvim.notify
  local notification
  rawset(nvim, "notify", function(message, level)
    notification = { message = message, level = level }
  end)
  Command.register()
  nvim.api.nvim_cmd({ cmd = "LouiselmChat", args = {} }, {})
  respond(process, 1, { protocolVersion = 1, agentCapabilities = {} })
  respond(process, 2, { sessionId = "acp-1" })

  nvim.api.nvim_cmd({ cmd = "LouiselmToMarkdown", args = { "does-not-exist" } }, {})

  nvim.ui.input = original_input
  rawset(nvim, "notify", original_notify)
  rawset(nvim, "system", original_system)
  Command.configure(nil)

  MiniTest.expect.equality(notification, {
    message = "louiselm: session is not attached",
    level = nvim.log.levels.ERROR,
  })
  delete_chat_buffers()
end

T["command"]["manual init exposes the canonical chat command"] = function()
  local original_add = nvim.pack.add
  nvim.pack.add = function() end
  local ok, error_message = pcall(dofile, "manual_init.lua")
  nvim.pack.add = original_add
  MiniTest.expect.equality(ok, true)
  MiniTest.expect.equality(error_message, nil)
  MiniTest.expect.equality(has_chat_command(), true)
end

T["command"]["register is repeatable"] = function()
  local registered = Command.register()
  MiniTest.expect.equality(registered, true)
  MiniTest.expect.equality(has_chat_command(), true)
end

T["command"]["launches the default adapter through the debug wrapper"] = function()
  local original_api_key = nvim.env.DEEPSEEK_API_KEY
  local original_command = nvim.env.LOUISELM_AGENT_COMMAND
  nvim.env.DEEPSEEK_API_KEY = "test-key"
  nvim.env.LOUISELM_AGENT_COMMAND = nil
  local process, original_system = fake_process()

  Command.register()
  nvim.api.nvim_cmd({ cmd = "LouiselmChat", args = {} }, {})

  rawset(nvim, "system", original_system)
  restore_environment("DEEPSEEK_API_KEY", original_api_key)
  restore_environment("LOUISELM_AGENT_COMMAND", original_command)

  MiniTest.expect.equality(process.command, {
    "/home/lotso/code/acp-llm-adapter/acp-debug.sh",
    "acp-llm-adapter",
    "serve",
    "--backend",
    "deepseek",
  })
  MiniTest.expect.equality(process.options.env, { LLM_API_KEY = "test-key" })
  delete_chat_buffers()
end

T["command"]["keeps an explicit executable override unchanged"] = function()
  local original_api_key = nvim.env.DEEPSEEK_API_KEY
  local original_command = nvim.env.LOUISELM_AGENT_COMMAND
  nvim.env.DEEPSEEK_API_KEY = "test-key"
  nvim.env.LOUISELM_AGENT_COMMAND = "custom-acp-agent"
  local process, original_system = fake_process()

  Command.register()
  nvim.api.nvim_cmd({ cmd = "LouiselmChat", args = {} }, {})

  rawset(nvim, "system", original_system)
  restore_environment("DEEPSEEK_API_KEY", original_api_key)
  restore_environment("LOUISELM_AGENT_COMMAND", original_command)

  MiniTest.expect.equality(process.command, { "custom-acp-agent" })
  MiniTest.expect.equality(process.options.env, nil)
  delete_chat_buffers()
end

T["command"]["launches a configured named agent"] = function()
  Command.configure({
    agents = {
      claude = { command = "claude-agent-acp", args = { "--test" }, env = { TOKEN = "secret" } },
    },
  })
  local process, original_system = fake_process()

  Command.register()
  nvim.api.nvim_cmd({ cmd = "LouiselmChat", args = {} }, {})

  rawset(nvim, "system", original_system)
  Command.configure(nil)

  MiniTest.expect.equality(process.command, { "claude-agent-acp", "--test" })
  MiniTest.expect.equality(process.options.env, { TOKEN = "secret" })
  delete_chat_buffers()
end

T["command"]["uses the configuration published by setup"] = function()
  assert(Louiselm.setup({ agents = { claude = { command = "configured-agent" } }, skills = { paths = {} } }))
  local process, original_system = fake_process()

  Command.register()
  nvim.api.nvim_cmd({ cmd = "LouiselmChat", args = {} }, {})

  rawset(nvim, "system", original_system)
  Command.configure(nil)

  MiniTest.expect.equality(process.command, { "configured-agent" })
  delete_chat_buffers()
end

T["command"]["does not block a native session when local picker discovery lacks lyaml"] = function()
  local skill_root = nvim.fn.tempname()
  local skill_dir = nvim.fs.joinpath(skill_root, "local-skill")
  assert(nvim.fn.mkdir(skill_dir, "p") == 1)
  assert(
    nvim.fn.writefile(
      { "---", "name: local-skill", "description: Local skill", "---" },
      nvim.fs.joinpath(skill_dir, "SKILL.md")
    ) == 0
  )
  assert(Louiselm.setup({
    agents = { claude = { command = "configured-agent" } },
    skills = { paths = { skill_root }, policy = "native" },
  }))
  local process, original_system = fake_process()
  local original_notify = nvim.notify
  local notification
  rawset(nvim, "notify", function(message, level)
    notification = { message = message, level = level }
  end)
  local loaded = package.loaded.lyaml
  local preload = package.preload.lyaml
  package.loaded.lyaml = nil
  rawset(package.preload, "lyaml", function()
    error("forced missing lyaml")
  end)
  Command.register()

  nvim.api.nvim_cmd({ cmd = "LouiselmChat", args = {} }, {})

  package.loaded.lyaml = loaded
  rawset(package.preload, "lyaml", preload)
  rawset(nvim, "notify", original_notify)
  rawset(nvim, "system", original_system)
  Command.configure(nil)
  nvim.fn.delete(skill_root, "rf")
  MiniTest.expect.equality(process.command, { "configured-agent" })
  MiniTest.expect.equality(notification, nil)
  delete_chat_buffers()
end

T["command"]["reports a terse lyaml error for an injected session"] = function()
  local skill_root = nvim.fn.tempname()
  local skill_dir = nvim.fs.joinpath(skill_root, "local-skill")
  assert(nvim.fn.mkdir(skill_dir, "p") == 1)
  assert(
    nvim.fn.writefile(
      { "---", "name: local-skill", "description: Local skill", "---" },
      nvim.fs.joinpath(skill_dir, "SKILL.md")
    ) == 0
  )
  assert(Louiselm.setup({
    agents = { claude = { command = "configured-agent" } },
    skills = { paths = { skill_root }, policy = "inject" },
  }))
  local original_notify = nvim.notify
  local notification
  rawset(nvim, "notify", function(message, level)
    notification = { message = message, level = level }
  end)
  local loaded = package.loaded.lyaml
  local preload = package.preload.lyaml
  package.loaded.lyaml = nil
  rawset(package.preload, "lyaml", function()
    error("module 'lyaml' not found", 0)
  end)
  Command.register()

  nvim.api.nvim_cmd({ cmd = "LouiselmChat", args = {} }, {})

  package.loaded.lyaml = loaded
  rawset(package.preload, "lyaml", preload)
  rawset(nvim, "notify", original_notify)
  Command.configure(nil)
  nvim.fn.delete(skill_root, "rf")
  MiniTest.expect.equality(notification, {
    message = "louiselm: Neovim cannot load lyaml; run :checkhealth louiselm",
    level = nvim.log.levels.ERROR,
  })
  delete_chat_buffers()
end

T["command"]["resume discovers the current workspace and bang discovers all without creating a session"] = function()
  Command.configure({ agents = { codex = { command = "codex-agent", args = {} } } })
  local process, original_system = fake_process()
  local original_notify = nvim.notify
  local notifications = 0
  rawset(nvim, "notify", function()
    notifications = notifications + 1
  end)
  Command.register()

  nvim.api.nvim_cmd({ cmd = "LouiselmResume", args = {} }, {})
  respond(process, 1, {
    protocolVersion = 1,
    agentCapabilities = { sessionCapabilities = { list = {} } },
  })
  local current_request = assert(Protocol.decode(process.writes[2]:sub(1, -2)))
  MiniTest.expect.equality(current_request.method, "session/list")
  MiniTest.expect.equality(current_request.params, { cwd = nvim.fn.getcwd() })
  respond(process, 2, { sessions = {} })

  nvim.api.nvim_cmd({ cmd = "LouiselmResume", args = {}, bang = true }, {})
  respond(process, 1, {
    protocolVersion = 1,
    agentCapabilities = { sessionCapabilities = { list = {} } },
  })
  local all_request = assert(Protocol.decode(process.writes[4]:sub(1, -2)))
  MiniTest.expect.equality(all_request.method, "session/list")
  MiniTest.expect.equality(all_request.params, {})
  respond(process, 2, { sessions = {} })
  MiniTest.expect.equality(
    nvim.wait(100, function()
      return notifications == 2
    end, 1),
    true
  )

  for _, write in ipairs(process.writes) do
    local message = assert(Protocol.decode(write:sub(1, -2)))
    MiniTest.expect.equality(message.method == "session/new", false)
  end
  for _, buffer in ipairs(nvim.api.nvim_list_bufs()) do
    MiniTest.expect.equality(nvim.api.nvim_buf_get_name(buffer):match("^louiselm://"), nil)
  end

  rawset(nvim, "system", original_system)
  rawset(nvim, "notify", original_notify)
  Command.configure(nil)
  delete_chat_buffers()
end

T["command"]["schedules real ACP discovery before notifying the UI"] = function()
  Command.configure({ agents = { mock = mock_definition() } })
  local original_notify = nvim.notify
  local notification
  rawset(nvim, "notify", function(message)
    notification = { message = message, fast = nvim.in_fast_event() }
  end)
  Command.register()

  nvim.api.nvim_cmd({ cmd = "LouiselmResume", args = {} }, {})
  local completed = nvim.wait(3000, function()
    return notification ~= nil
  end, 10)

  rawset(nvim, "notify", original_notify)
  Command.configure(nil)
  MiniTest.expect.equality(completed, true)
  MiniTest.expect.equality(notification, { message = "louiselm: no recoverable sessions found", fast = false })
  delete_chat_buffers()
end

T["command"]["warns without blocking chat creation when a configured agent trails its latest-version check"] = function()
  local definition = mock_definition()
  definition.latest = { command = "npm", args = { "view", "mock-acp", "version" } }
  Command.configure({ agents = { mock = definition } })

  local original_system = nvim.system
  local original_executable = nvim.fn.executable
  local calls = {}
  rawset(nvim, "system", function(command, options, on_exit)
    local entry = { command = command, options = options, on_exit = on_exit }
    calls[#calls + 1] = entry
    entry.handle = {
      write = function() end,
      kill = function() end,
      is_closing = function()
        return false
      end,
    }
    return entry.handle
  end)
  rawset(nvim.fn, "executable", function()
    return 1
  end)

  local original_notify = nvim.notify
  local notifications = {}
  rawset(nvim, "notify", function(message, level)
    notifications[#notifications + 1] = { message = message, level = level }
  end)

  Command.register()
  local buffer_before = nvim.api.nvim_get_current_buf()
  nvim.api.nvim_cmd({ cmd = "LouiselmChat", args = {} }, {})
  local buffer_after = nvim.api.nvim_get_current_buf()

  -- The chat buffer opens synchronously; the staleness checks below are
  -- still pending vim.system calls at this point, proving they cannot have
  -- delayed chat creation.
  MiniTest.expect.equality(buffer_after ~= buffer_before, true)

  local version_call, latest_call
  for _, entry in ipairs(calls) do
    if entry.command[1] == "npm" then
      latest_call = entry
    elseif entry.command[#entry.command] == "--version" then
      version_call = entry
    end
  end
  assert(version_call ~= nil, "expected a --version health check to have been spawned")
  assert(latest_call ~= nil, "expected the configured latest-version check to have been spawned")

  latest_call.on_exit({ code = 0, signal = 0, stdout = "9.9.9\n", stderr = "" })
  MiniTest.expect.equality(notifications, {})
  version_call.on_exit({ code = 0, signal = 0, stdout = "mock-acp 1.0.0\n", stderr = "" })

  rawset(nvim, "system", original_system)
  rawset(nvim.fn, "executable", original_executable)
  rawset(nvim, "notify", original_notify)
  Command.configure(nil)

  MiniTest.expect.equality(notifications, {
    { message = "louiselm: mock is outdated (mock-acp 1.0.0 installed, 9.9.9 upstream)", level = nvim.log.levels.WARN },
  })
  delete_chat_buffers()
end

T["command"]["injects the hidden bounded catalog through the normal chat path"] = function()
  local skill_root = nvim.fn.tempname()
  local source_root = nvim.fs.joinpath(skill_root, "source")
  local generated_root = nvim.fs.joinpath(skill_root, "generated")
  local function write_linked_skill(name, description, metadata)
    local source_dir = nvim.fs.joinpath(source_root, name)
    local generated_dir = nvim.fs.joinpath(generated_root, name)
    assert(nvim.fn.mkdir(source_dir, "p") == 1)
    assert(nvim.fn.mkdir(generated_dir, "p") == 1)
    local lines = { "---", "name: " .. name, "description: " .. description }
    nvim.list_extend(lines, metadata or {})
    lines[#lines + 1] = "---"
    local source = nvim.fs.joinpath(source_dir, "SKILL.md")
    assert(nvim.fn.writefile(lines, source) == 0)
    assert(nvim.uv.fs_symlink(source, nvim.fs.joinpath(generated_dir, "SKILL.md")))
  end
  write_linked_skill("alpha-skill", "First skill")
  write_linked_skill("beta-skill", "Second skill", { "triggers:", "  - beta" })

  local process, original_system = fake_process()
  assert(Louiselm.setup({
    agents = { claude = { command = "claude-agent-acp", args = {} } },
    skills = { paths = { generated_root }, policy = "inject" },
  }))
  Command.register()
  nvim.api.nvim_cmd({ cmd = "LouiselmChat", args = {} }, {})
  respond(process, 1, { protocolVersion = 1, agentCapabilities = {} })
  respond(process, 2, { sessionId = "skills-acp" })

  local buffer = nvim.api.nvim_get_current_buf()
  MiniTest.expect.equality(buffer_contains(buffer, "skill-index"), false)
  MiniTest.expect.equality(buffer_contains(buffer, "available_skills"), false)
  nvim.api.nvim_buf_set_lines(buffer, 5, 6, false, { "> list skills" })
  local submit
  for _, mapping in ipairs(nvim.api.nvim_buf_get_keymap(buffer, "i")) do
    if mapping.desc == "Submit louiselm prompt" then
      submit = mapping.callback
      break
    end
  end
  assert(type(submit) == "function")
  nvim.api.nvim_buf_call(buffer, submit)
  local prompt = assert(Protocol.decode(process.writes[3]:sub(1, -2))).params.prompt
  local index = prompt[1].text

  rawset(nvim, "system", original_system)
  Command.configure(nil)

  MiniTest.expect.equality(process.command, { "claude-agent-acp" })
  MiniTest.expect.equality(#index <= 8000, true)
  MiniTest.expect.equality(index:find("<name>alpha-skill</name>", 1, true) ~= nil, true)
  MiniTest.expect.equality(index:find("<name>beta-skill</name>", 1, true) ~= nil, true)
  delete_chat_buffers()
  nvim.fn.delete(skill_root, "rf")
end

T["command"]["attaches a project instructions resource_link on a new session through the normal chat path"] = function()
  local project = nvim.fn.tempname()
  assert(nvim.fn.mkdir(project, "p") == 1)
  local instructions_path = nvim.fs.joinpath(project, "AGENTS.md")
  assert(nvim.fn.writefile({ "# Contract" }, instructions_path) == 0)
  local original_cwd = nvim.fn.getcwd()
  nvim.fn.chdir(project)

  local process, original_system = fake_process()
  assert(Louiselm.setup({
    agents = { claude = { command = "claude-agent-acp", args = {} } },
    context = { instructions_file = "AGENTS.md" },
  }))
  Command.register()
  nvim.api.nvim_cmd({ cmd = "LouiselmChat", args = {} }, {})
  respond(process, 1, { protocolVersion = 1, agentCapabilities = {} })
  respond(process, 2, { sessionId = "instructions-acp" })

  local buffer = nvim.api.nvim_get_current_buf()
  local prompt_line = nvim.api.nvim_buf_line_count(buffer) - 1
  nvim.api.nvim_buf_set_lines(buffer, prompt_line, prompt_line + 1, false, { "> [context: AGENTS.md] hello" })
  local submit
  for _, mapping in ipairs(nvim.api.nvim_buf_get_keymap(buffer, "i")) do
    if mapping.desc == "Submit louiselm prompt" then
      submit = mapping.callback
      break
    end
  end
  assert(type(submit) == "function")
  nvim.api.nvim_buf_call(buffer, submit)
  local prompt = assert(Protocol.decode(process.writes[3]:sub(1, -2))).params.prompt

  rawset(nvim, "system", original_system)
  Command.configure(nil)
  nvim.fn.chdir(original_cwd)

  MiniTest.expect.equality(process.command, { "claude-agent-acp" })
  MiniTest.expect.equality(prompt, {
    { type = "resource_link", uri = "file://" .. instructions_path, name = "AGENTS.md" },
    { type = "text", text = "hello" },
  })
  delete_chat_buffers()
  nvim.fn.delete(project, "rf")
end

return T
