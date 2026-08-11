local MiniTest = require("mini.test")

local Protocol = require("louiselm.acp.protocol")
local Command = require("louiselm.ui.chat.command")
local Louiselm = require("louiselm")
local Schema = require("louiselm.schema")

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
  nvim.system = function(command, options, on_exit)
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
  end
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
  MiniTest.expect.equality(commands.LouiselmSessionOptions ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmInline ~= nil, true)
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
  nvim.system = original_system
  Command.configure(nil)
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

  nvim.system = original_system
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

  nvim.system = original_system
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

  nvim.system = original_system
  Command.configure(nil)

  MiniTest.expect.equality(process.command, { "claude-agent-acp", "--test" })
  MiniTest.expect.equality(process.options.env, { TOKEN = "secret" })
  delete_chat_buffers()
end

T["command"]["uses the configuration published by setup"] = function()
  local schema = assert(Schema.define({
    agents = {
      type = "table",
      fields = {
        claude = {
          type = "table",
          fields = {
            command = { type = "string" },
          },
        },
      },
    },
    skills = {
      type = "table",
      fields = {
        paths = { type = "array-of", items = "string" },
      },
    },
  }))
  assert(Louiselm.setup({ agents = { claude = { command = "configured-agent" } }, skills = { paths = {} } }, schema))
  local process, original_system = fake_process()

  Command.register()
  nvim.api.nvim_cmd({ cmd = "LouiselmChat", args = {} }, {})

  nvim.system = original_system
  Command.configure(nil)

  MiniTest.expect.equality(process.command, { "configured-agent" })
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

  nvim.system = original_system
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

T["command"]["injects the configured skill index while native loading stays un-injected"] = function()
  local skill_root = nvim.fn.tempname()
  local skill_dir = nvim.fs.joinpath(skill_root, "grill-me")
  assert(nvim.fn.mkdir(skill_dir, "p") == 1)
  assert(
    nvim.fn.writefile({ "---", "name: grill-me", "description: Stress test an idea", "---" }, skill_dir .. "/SKILL.md")
      == 0
  )

  local process, original_system = fake_process()
  Command.configure({
    agents = { claude = { command = "claude-agent-acp", args = {} } },
    skills = { paths = { skill_root }, policy = "inject" },
  })
  Command.register()
  nvim.api.nvim_cmd({ cmd = "LouiselmChat", args = {} }, {})
  local injected_lines = nvim.api.nvim_buf_get_lines(nvim.api.nvim_get_current_buf(), 0, -1, false)

  nvim.system = original_system
  delete_chat_buffers()
  local native_process, native_original_system = fake_process()
  Command.configure({
    agents = { claude = { command = "claude-agent-acp", args = {} } },
    skills = { paths = { skill_root }, policy = "native" },
  })
  Command.register()
  nvim.api.nvim_cmd({ cmd = "LouiselmChat", args = {} }, {})
  local native_lines = nvim.api.nvim_buf_get_lines(nvim.api.nvim_get_current_buf(), 0, -1, false)
  nvim.system = native_original_system
  Command.configure(nil)

  MiniTest.expect.equality(table.concat(injected_lines, "\n"):find("[context: skill-index]", 1, true) ~= nil, true)
  MiniTest.expect.equality(table.concat(native_lines, "\n"):find("[context: skill-index]", 1, true) ~= nil, false)
  MiniTest.expect.equality(process.command, { "claude-agent-acp" })
  MiniTest.expect.equality(native_process.command, { "claude-agent-acp" })
  delete_chat_buffers()
  nvim.fn.delete(skill_root, "rf")
end

return T
