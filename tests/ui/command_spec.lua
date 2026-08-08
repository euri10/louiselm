local MiniTest = require("mini.test")

local Command = require("louiselm.ui.chat.command")
local Louiselm = require("louiselm")
local Schema = require("louiselm.schema")

local T = MiniTest.new_set()

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

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
  MiniTest.expect.equality(commands.LouiselmSwitchSession ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmRenameSession ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmCloseSession ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmSessionOptions ~= nil, true)
  MiniTest.expect.equality(nvim.api.nvim_get_commands({ builtin = false }).LouisLMChat, nil)
  MiniTest.expect.equality(nvim.api.nvim_get_commands({ builtin = false }).LuiseLmChat, nil)
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
