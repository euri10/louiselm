local MiniTest = require("mini.test")

local Protocol = require("louiselm.acp.protocol")
local Command = require("louiselm.ui.chat.command")
local Beads = require("louiselm.ui.beads")
local ForensicsStore = require("louiselm.forensics.store")
local Louiselm = require("louiselm")
local Session = require("louiselm.session")

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

local LIMITS_READ_METHOD = "_io.github.euri10.louiselm/account_limits/read"

local function limits_capabilities()
  return {
    _meta = {
      ["io.github.euri10.louiselm"] = {
        accountLimits = {
          version = 1,
          readMethod = LIMITS_READ_METHOD,
          updatedMethod = "_io.github.euri10.louiselm/account_limits/updated",
        },
      },
    },
  }
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

---@param mode "q"|"qa"
---@param recoverable boolean
---@return table result
---@return string state_root
---@return string continued_marker
---@return string disposed_marker
local function run_exit_child(mode, recoverable)
  local root = nvim.fn.tempname()
  local state_root = nvim.fs.joinpath(root, "state")
  local script = nvim.fs.joinpath(root, "exit.lua")
  local continued_marker = nvim.fs.joinpath(root, "continued")
  local disposed_marker = nvim.fs.joinpath(root, "disposed")
  assert(nvim.fn.mkdir(root, "p") == 1)
  assert(nvim.fn.writefile({
    "vim.opt.rtp:prepend(vim.env.LOUISELM_TEST_ROOT)",
    "local Protocol = require('louiselm.acp.protocol')",
    "local Command = require('louiselm.ui.chat.command')",
    "local Session = require('louiselm.session')",
    "local process",
    "vim.system = function(_, options)",
    "  process = { options = options, writes = {} }",
    "  process.handle = {",
    "    write = function(_, data) process.writes[#process.writes + 1] = data end,",
    "    kill = function() vim.fn.writefile({ 'disposed' }, vim.env.LOUISELM_TEST_DISPOSED) end,",
    "    is_closing = function() return false end,",
    "  }",
    "  return process.handle",
    "end",
    "Command.configure({ agents = { codex = { command = 'agent', args = {} } } })",
    "Command.register()",
    "vim.cmd('LouiselmChat')",
    "local capabilities = vim.env.LOUISELM_TEST_RECOVERABLE == '1' and { loadSession = true } or {}",
    "process.options.stdout(nil, assert(Protocol.encode(Protocol.response(1, { protocolVersion = 1, agentCapabilities = capabilities }))) .. '\\n')",
    "process.options.stdout(nil, assert(Protocol.encode(Protocol.response(2, { sessionId = 'child-acp' }))) .. '\\n')",
    "if vim.env.LOUISELM_TEST_EXIT == 'qa' then",
    "  vim.cmd('LouiselmMentionBuffer')",
    "  assert(Session.exit_verdict()[1].session:prompt('working'))",
    "end",
    "vim.cmd(vim.env.LOUISELM_TEST_EXIT)",
    "vim.fn.writefile({ 'continued' }, vim.env.LOUISELM_TEST_MARKER)",
    "vim.cmd('qa!')",
  }, script) == 0)
  local result = nvim
    .system({ nvim.v.progpath, "--headless", "--clean", "-u", "NONE", "-c", "lua dofile(vim.env.LOUISELM_TEST_SCRIPT)" }, {
      text = true,
      env = {
        XDG_STATE_HOME = state_root,
        LOUISELM_TEST_ROOT = project_root,
        LOUISELM_TEST_SCRIPT = script,
        LOUISELM_TEST_MARKER = continued_marker,
        LOUISELM_TEST_DISPOSED = disposed_marker,
        LOUISELM_TEST_EXIT = mode,
        LOUISELM_TEST_RECOVERABLE = recoverable and "1" or "0",
      },
    })
    :wait(5000)
  return result, state_root, continued_marker, disposed_marker
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

T["command"] = MiniTest.new_set({
  hooks = {
    post_case = function()
      -- A failing expectation skips a test's own cleanup.
      Session.dispose_all()
      delete_chat_buffers()
    end,
  },
})

T["command"]["minimal init exposes the canonical chat command"] = function()
  local commands = nvim.api.nvim_get_commands({ builtin = false })
  MiniTest.expect.equality(has_chat_command(), true)
  MiniTest.expect.equality(commands.LouiselmCancel ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmSessionNew ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmResume ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmPark ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmResume.bang, true)
  MiniTest.expect.equality(commands.LouiselmSessionSwitch ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmHandOff ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmSessionRename ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmSessionClose ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmSessionId ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmInspectBead ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmInspectProvenance ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmToMarkdown ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmInspectTool ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmSessionOptions ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmLimits ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmLimits.nargs, "?")
  MiniTest.expect.equality(commands.LouiselmPermissions ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmInline ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmPickSkill ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmPickFile ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmMentionBuffer ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmSendSelection ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmDiagnostics ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmTutor ~= nil, true)
  MiniTest.expect.equality(nvim.api.nvim_get_commands({ builtin = false }).LouisLMChat, nil)
  MiniTest.expect.equality(nvim.api.nvim_get_commands({ builtin = false }).LuiseLmChat, nil)
end

T["command"]["views current Forensics evidence availability without a chat"] = function()
  local root = nvim.fn.tempname()
  local store = assert(ForensicsStore.new(nvim.fs.joinpath(root, "forensics")))
  local record_path = assert(store:write({
    schema_version = 1,
    id = "record-1",
    observed_at = 100,
    subject = { agent = "codex", acp_session_id = "acp-1" },
    observations = {},
    evidence_sources = {
      {
        kind = "acp_log",
        state = "omitted",
        mutable = true,
        reason = "ACP adapter did not advertise a log path",
      },
      { kind = "git", state = "present", mutable = true },
    },
  }))
  MiniTest.finally(function()
    nvim.fn.delete(root, "rf")
  end)

  nvim.api.nvim_cmd({ cmd = "LouiselmForensicsView", args = { record_path } }, {})

  local buffer = nvim.api.nvim_get_current_buf()
  local inspected = nvim.json.decode(table.concat(nvim.api.nvim_buf_get_lines(buffer, 0, -1, false), "\n"))
  MiniTest.expect.equality(inspected.evidence_availability, {
    conversation_content = "missing",
    repository_state = "available",
    wire_ordering = "missing",
  })
  MiniTest.expect.equality(nvim.api.nvim_get_option_value("buftype", { buf = buffer }), "nofile")
  MiniTest.expect.equality(nvim.api.nvim_get_option_value("modifiable", { buf = buffer }), false)
end

T["command"]["opens the Tutor without a configured Agent"] = function()
  nvim.api.nvim_cmd({ cmd = "LouiselmTutor", args = {} }, {})

  local buffer = nvim.api.nvim_get_current_buf()
  MiniTest.finally(function()
    if nvim.api.nvim_buf_is_valid(buffer) then
      nvim.api.nvim_buf_delete(buffer, { force = true })
    end
  end)
  MiniTest.expect.equality(nvim.api.nvim_buf_get_name(buffer):match("docs/tutorial%.md$") ~= nil, true)
  MiniTest.expect.equality(nvim.api.nvim_get_option_value("modifiable", { buf = buffer }), false)
  MiniTest.expect.equality(buffer_contains(buffer, ":LouiselmChat"), true)
end

T["command"]["inspects Beads from the active Session workspace"] = function()
  Command.configure({ agents = { codex = { command = "codex-agent", args = {} } } })
  local process, original_system = fake_process()
  local original_inspect = Beads.inspect
  local original_cwd = nvim.fn.getcwd()
  local inspect_options
  rawset(Beads, "inspect", function(_, options)
    inspect_options = options
    return true
  end)
  MiniTest.finally(function()
    rawset(Beads, "inspect", original_inspect)
    rawset(nvim, "system", original_system)
    nvim.cmd("lcd " .. nvim.fn.fnameescape(original_cwd))
    Command.configure(nil)
  end)

  Command.register()
  nvim.api.nvim_cmd({ cmd = "LouiselmChat", args = {} }, {})
  respond(process, 1, { protocolVersion = 1, agentCapabilities = {} })
  respond(process, 2, { sessionId = "beads-cwd-acp" })

  nvim.cmd("lcd /tmp")
  nvim.api.nvim_cmd({ cmd = "LouiselmInspectBead", args = {} }, {})

  MiniTest.expect.equality(inspect_options.cwd, original_cwd)
end

T["command"]["shows and refreshes the active Agent account limits"] = function()
  Command.configure({ agents = { codex = { command = "codex-agent", args = {} } } })
  local process, original_system = fake_process()
  local original_notify = nvim.notify
  rawset(nvim, "notify", function() end)
  MiniTest.finally(function()
    rawset(nvim, "notify", original_notify)
  end)
  Command.register()

  nvim.api.nvim_cmd({ cmd = "LouiselmChat", args = {} }, {})
  respond(process, 1, { protocolVersion = 1, agentCapabilities = limits_capabilities() })
  respond(process, 2, { sessionId = "codex-acp" })
  nvim.api.nvim_cmd({ cmd = "LouiselmLimits", args = {} }, {})

  MiniTest.expect.equality(assert(Protocol.decode(process.writes[3]:sub(1, -2))).method, LIMITS_READ_METHOD)
  MiniTest.expect.equality(nvim.api.nvim_buf_get_name(0), "louiselm://limits/codex")
  respond(process, 3, {
    defaultBucketId = "codex",
    buckets = {
      {
        id = "codex",
        label = "Codex",
        windows = { { usedPercent = 82, windowDurationMins = 300, resetsAt = 4102444800 } },
        planType = "plus",
        credits = { balance = 7.5, unlimited = false },
      },
      {
        id = "other",
        label = "Other",
        windows = {
          { usedPercent = 25, windowDurationMins = 90, resetsAt = 4102444800 },
          { usedPercent = 33, windowDurationMins = 10080, resetsAt = 4102448400 },
        },
      },
    },
    resetCredits = { availableCount = 1 },
  })
  local rendered = nvim.wait(1000, function()
    return buffer_contains(nvim.api.nvim_get_current_buf(), "18% left · 82% used")
  end)

  local buffer = nvim.api.nvim_get_current_buf()
  MiniTest.expect.equality(rendered, true)
  MiniTest.expect.equality(buffer_contains(buffer, "Agent: codex"), true)
  MiniTest.expect.equality(buffer_contains(buffer, "Codex (codex)"), true)
  MiniTest.expect.equality(buffer_contains(buffer, "Window: 5h"), true)
  MiniTest.expect.equality(buffer_contains(buffer, "Plan: plus"), true)
  MiniTest.expect.equality(buffer_contains(buffer, "Credits: 7.5"), true)
  MiniTest.expect.equality(buffer_contains(buffer, "Other (other)"), true)
  MiniTest.expect.equality(buffer_contains(buffer, "75% left · 25% used"), true)
  MiniTest.expect.equality(buffer_contains(buffer, "67% left · 33% used"), true)
  MiniTest.expect.equality(buffer_contains(buffer, "Window: 90m"), true)
  MiniTest.expect.equality(buffer_contains(buffer, "Window: 7d"), true)
  MiniTest.expect.equality(buffer_contains(buffer, "Reset credits: 1"), true)
  MiniTest.expect.equality(nvim.api.nvim_get_option_value("modifiable", { buf = buffer }), false)

  rawset(nvim, "system", original_system)
  Command.configure(nil)
end

T["command"]["shows an explicitly named unobserved Agent without starting it"] = function()
  Command.configure({ agents = { codex = { command = "codex-agent", args = {} } } })
  local process, original_system = fake_process()
  Command.register()

  nvim.api.nvim_cmd({ cmd = "LouiselmLimits", args = { "codex" } }, {})
  local buffer = nvim.api.nvim_get_current_buf()

  MiniTest.expect.equality(process.command, nil)
  MiniTest.expect.equality(nvim.api.nvim_buf_get_name(buffer), "louiselm://limits/codex")
  MiniTest.expect.equality(buffer_contains(buffer, "Status: not observed"), true)

  rawset(nvim, "system", original_system)
  Command.configure(nil)
end

T["command"]["refuses real :q and :q! for an unrecoverable Session"] = function()
  Command.configure({ agents = { codex = { command = "codex-agent", args = {} } } })
  local process, original_system = fake_process()
  local original_notify = nvim.notify
  local notifications = {}
  rawset(nvim, "notify", function(message, level)
    notifications[#notifications + 1] = { message = message, level = level }
  end)
  Command.register()

  nvim.api.nvim_cmd({ cmd = "LouiselmChat", args = {} }, {})
  respond(process, 1, { protocolVersion = 1, agentCapabilities = {} })
  respond(process, 2, { sessionId = "first-acp" })

  local chat_buffer = nvim.api.nvim_get_current_buf()
  nvim.api.nvim_cmd({ cmd = "quit" }, {})
  nvim.api.nvim_cmd({ cmd = "quit", bang = true }, {})

  rawset(nvim, "system", original_system)
  rawset(nvim, "notify", original_notify)
  Command.configure(nil)

  MiniTest.expect.equality(#nvim.api.nvim_list_wins(), 1)
  MiniTest.expect.equality(nvim.api.nvim_get_current_buf(), chat_buffer)
  MiniTest.expect.equality(#notifications, 2)
  MiniTest.expect.equality(notifications[1].message:find("codex: cannot resume", 1, true) ~= nil, true)
  MiniTest.expect.equality(notifications[1].message:find(":qa to exit anyway", 1, true) ~= nil, true)
  MiniTest.expect.equality(notifications[2], notifications[1])
end

T["command"]["refuses real :q for a recoverable Session with a turn in flight"] = function()
  Command.configure({ agents = { codex = { command = "codex-agent", args = {} } } })
  local process, original_system = fake_process()
  local original_notify = nvim.notify
  local notification
  rawset(nvim, "notify", function(message, level)
    notification = { message = message, level = level }
  end)
  Command.register()
  nvim.api.nvim_cmd({ cmd = "LouiselmChat", args = {} }, {})
  respond(process, 1, { protocolVersion = 1, agentCapabilities = { loadSession = true } })
  respond(process, 2, { sessionId = "active-acp" })
  assert(Session.exit_verdict()[1].session:prompt("working"))

  nvim.api.nvim_cmd({ cmd = "quit" }, {})

  rawset(nvim, "system", original_system)
  rawset(nvim, "notify", original_notify)
  Command.configure(nil)
  MiniTest.expect.equality(notification.message:find("codex: turn in flight", 1, true) ~= nil, true)
end

T["command"]["refuses real :q when a recoverable Session has Staged context"] = function()
  Command.configure({ agents = { codex = { command = "codex-agent", args = {} } } })
  local process, original_system = fake_process()
  local original_notify = nvim.notify
  local notification
  rawset(nvim, "notify", function(message, level)
    notification = { message = message, level = level }
  end)
  Command.register()
  nvim.api.nvim_cmd({ cmd = "LouiselmChat", args = {} }, {})
  respond(process, 1, { protocolVersion = 1, agentCapabilities = { loadSession = true } })
  respond(process, 2, { sessionId = "staged-acp" })
  nvim.api.nvim_cmd({ cmd = "LouiselmMentionBuffer", args = {} }, {})

  nvim.api.nvim_cmd({ cmd = "quit" }, {})

  rawset(nvim, "system", original_system)
  rawset(nvim, "notify", original_notify)
  Command.configure(nil)
  MiniTest.expect.equality(notification.message:find("1 queued context", 1, true) ~= nil, true)
end

T["command"]["guards a headless API Session with no chat buffer"] = function()
  local process, original_system = fake_process()
  local original_notify = nvim.notify
  local notification
  rawset(nvim, "notify", function(message, level)
    notification = { message = message, level = level }
  end)
  Command.register()
  local api = assert(Session.new({ codex = { command = "codex-agent", args = {} } }))
  assert(api:create_session("codex"))
  respond(process, 1, { protocolVersion = 1, agentCapabilities = {} })
  respond(process, 2, { sessionId = "headless-acp" })

  nvim.api.nvim_cmd({ cmd = "quit" }, {})

  rawset(nvim, "system", original_system)
  rawset(nvim, "notify", original_notify)
  MiniTest.expect.equality(notification.message:find("codex: cannot resume", 1, true) ~= nil, true)
end

T["command"]["allows recoverable idle :q and surfaces its breadcrumb once at setup"] = function()
  local result, state_root, continued_marker, disposed_marker = run_exit_child("q", true)
  local path = nvim.fs.joinpath(state_root, "nvim", "louiselm", "abandoned.json")

  MiniTest.expect.equality(result.code, 0)
  MiniTest.expect.equality(nvim.fn.filereadable(continued_marker), 0)
  MiniTest.expect.equality(nvim.fn.filereadable(disposed_marker), 1)
  MiniTest.expect.equality(nvim.fn.filereadable(path), 1)
  local record = nvim.json.decode(table.concat(nvim.fn.readfile(path), "\n"))
  MiniTest.expect.equality(record.sessions[1].recoverable, true)

  local original_state_home = nvim.env.XDG_STATE_HOME
  local original_notify = nvim.notify
  local notification
  nvim.env.XDG_STATE_HOME = state_root
  rawset(nvim, "notify", function(message, level)
    notification = { message = message, level = level }
  end)
  assert(Louiselm.setup({ agents = { codex = { command = "agent", args = {} } } }))
  rawset(nvim, "notify", original_notify)
  nvim.env.XDG_STATE_HOME = original_state_home

  MiniTest.expect.equality(notification.message:find(":LouiselmResume", 1, true) ~= nil, true)
  MiniTest.expect.equality(nvim.fn.filereadable(path), 0)
  nvim.fn.delete(nvim.fs.dirname(state_root), "rf")
end

T["command"]["allows :qa past refusal clauses and records an unrecoverable Session"] = function()
  local result, state_root, continued_marker, disposed_marker = run_exit_child("qa", false)
  local path = nvim.fs.joinpath(state_root, "nvim", "louiselm", "abandoned.json")

  MiniTest.expect.equality(result.code, 0)
  MiniTest.expect.equality(nvim.fn.filereadable(continued_marker), 0)
  MiniTest.expect.equality(nvim.fn.filereadable(disposed_marker), 1)
  MiniTest.expect.equality(nvim.fn.filereadable(path), 1)
  local record = nvim.json.decode(table.concat(nvim.fn.readfile(path), "\n"))
  MiniTest.expect.equality(record.sessions[1].recoverable, false)
  MiniTest.expect.equality(record.sessions[1].turn_active, true)
  MiniTest.expect.equality(record.sessions[1].staged.contexts, 1)
  nvim.fn.delete(nvim.fs.dirname(state_root), "rf")
end

T["command"]["routes a window bar click through its clicked window"] = function()
  Command.configure({ agents = { codex = { command = "codex-agent", args = {} } } })
  local process, original_system = fake_process()
  local original_getmousepos = nvim.fn.getmousepos
  MiniTest.finally(function()
    rawset(nvim, "system", original_system)
    nvim.fn.getmousepos = original_getmousepos
    Command.configure(nil)
    delete_chat_buffers()
  end)
  Command.register()

  nvim.api.nvim_cmd({ cmd = "LouiselmChat", args = {} }, {})
  respond(process, 1, { protocolVersion = 1, agentCapabilities = {} })
  respond(process, 2, { sessionId = "first-acp" })
  local first_buffer = nvim.api.nvim_get_current_buf()
  nvim.api.nvim_cmd({ cmd = "LouiselmSessionNew", args = {} }, {})
  respond(process, 3, { sessionId = "second-acp" })
  local clicked_window = nvim.api.nvim_get_current_win()
  nvim.cmd.vsplit()
  local other_window = nvim.api.nvim_get_current_win()
  nvim.fn.getmousepos = function()
    return { winid = clicked_window }
  end

  Command.winbar_click(1, 1, "l", "")

  MiniTest.expect.equality(nvim.api.nvim_get_current_win(), clicked_window)
  MiniTest.expect.equality(nvim.api.nvim_get_current_buf(), first_buffer)
  nvim.api.nvim_win_close(other_window, true)
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

T["command"]["copies the current OpenCode session id without an environment variable"] = function()
  Command.configure({ agents = { opencode = { command = "opencode", args = {} } } })
  local process, original_system = fake_process()
  local original_clipboard = nvim.fn.getreg("+")
  Command.register()
  nvim.api.nvim_cmd({ cmd = "LouiselmChat", args = {} }, {})
  respond(process, 1, { protocolVersion = 1, agentCapabilities = {} })
  respond(process, 2, { sessionId = "ses-current" })

  nvim.api.nvim_cmd({ cmd = "LouiselmSessionId", args = {} }, {})

  MiniTest.expect.equality(nvim.fn.getreg("+"), "opencode/ses-current")
  nvim.fn.setreg("+", original_clipboard)
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
  for _, name in ipairs({
    "LouiselmPickSkill",
    "LouiselmPickFile",
    "LouiselmMentionBuffer",
    "LouiselmSendSelection",
    "LouiselmDiagnostics",
    "LouiselmHandOff",
  }) do
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

T["command"]["rejects an unknown session id without prompting for a path"] = function()
  Command.configure({ agents = { codex = { command = "codex-agent", args = {} } } })
  local process, original_system = fake_process()
  local original_input = nvim.ui.input
  -- The sibling test above stubs vim.ui.input to invoke its callback at once,
  -- so it cannot see whether the prompt was opened before the session id was
  -- rejected. Counting calls is the whole point of this test (louiselm-euj).
  local input_calls = 0
  nvim.ui.input = function(_, callback)
    input_calls = input_calls + 1
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

  -- Buffers are dropped before asserting: this spec has no post_case hook, so
  -- a failed expectation would otherwise abort the test with its chat buffer
  -- still open and cascade "Buffer with this name already exists" into every
  -- later case (louiselm-f0n2).
  delete_chat_buffers()

  MiniTest.expect.equality(input_calls, 0)
  MiniTest.expect.equality(notification, {
    message = "louiselm: session is not attached",
    level = nvim.log.levels.ERROR,
  })
end

T["command"]["still prompts for a path when an explicit session id is attached"] = function()
  Command.configure({ agents = { codex = { command = "codex-agent", args = {} } } })
  local process, original_system = fake_process()
  local original_input = nvim.ui.input
  local input_calls = 0
  nvim.ui.input = function(_, callback)
    input_calls = input_calls + 1
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

  -- Views are keyed by the session's own state.id, not by the ACP sessionId
  -- echoed above, and the chat buffer is named "louiselm://<state.id>" -- so
  -- read the id back rather than hardcoding an internal one.
  local session_id = nvim.api.nvim_buf_get_name(nvim.api.nvim_get_current_buf()):match("^louiselm://(.+)$")

  nvim.api.nvim_cmd({ cmd = "LouiselmToMarkdown", args = { session_id } }, {})

  nvim.ui.input = original_input
  rawset(nvim, "notify", original_notify)
  rawset(nvim, "system", original_system)
  Command.configure(nil)

  local path = notification and notification.message:match("^louiselm: exported transcript to (.+)$")
  if type(path) == "string" then
    nvim.fn.delete(path)
  end
  delete_chat_buffers()

  MiniTest.expect.equality(input_calls, 1)
  MiniTest.expect.equality(type(path), "string")
end

T["command"]["manual init configures current logged agents"] = function()
  local original_add = nvim.pack.add
  local original_setup = Louiselm.setup
  local original_api_key = nvim.env.DEEPSEEK_API_KEY
  local configured
  nvim.pack.add = function() end
  rawset(Louiselm, "setup", function(config)
    configured = config
    return true
  end)
  nvim.env.DEEPSEEK_API_KEY = "test-key"
  local ok, error_message = pcall(dofile, "manual_init.lua")
  nvim.pack.add = original_add
  rawset(Louiselm, "setup", original_setup)
  restore_environment("DEEPSEEK_API_KEY", original_api_key)

  MiniTest.expect.equality(ok, true)
  MiniTest.expect.equality(error_message, nil)
  MiniTest.expect.equality(configured, {
    agents = {
      claude = {
        command = "acp-proxy",
        args = { "--", "claude-agent-acp" },
        version = { command = "claude-agent-acp", args = { "--version" } },
      },
      deepseek = {
        command = "acp-llm-adapter",
        args = { "serve", "--backend", "deepseek" },
        env = { ACP_LOG = "1", LLM_API_KEY = "test-key" },
        version = { command = "acp-llm-adapter", args = { "--version" } },
      },
    },
  })
end

T["command"]["quickstart installs LouiseLM and configures Codex"] = function()
  local original_add = nvim.pack.add
  local original_setup = Louiselm.setup
  local added
  local configured
  nvim.pack.add = function(spec, options)
    added = { spec = spec, options = options }
  end
  rawset(Louiselm, "setup", function(config)
    configured = config
    return true
  end)
  MiniTest.finally(function()
    nvim.pack.add = original_add
    rawset(Louiselm, "setup", original_setup)
  end)

  local ok, error_message = pcall(dofile, project_root .. "/examples/quickstart.lua")

  MiniTest.expect.equality(ok, true)
  MiniTest.expect.equality(error_message, nil)
  MiniTest.expect.equality(added, {
    spec = {
      {
        src = "https://github.com/euri10/louiselm.git",
        name = "louiselm.nvim",
      },
    },
    options = { confirm = false },
  })
  MiniTest.expect.equality(configured, { agents = { codex = { command = "codex-acp" } } })
end

T["command"]["register is repeatable"] = function()
  local registered = Command.register()
  MiniTest.expect.equality(registered, true)
  MiniTest.expect.equality(has_chat_command(), true)
end

T["command"]["warns and stops when no Agent is configured"] = function()
  local original_api_key = nvim.env.DEEPSEEK_API_KEY
  local original_command = nvim.env.LOUISELM_AGENT_COMMAND
  nvim.env.DEEPSEEK_API_KEY = "test-key"
  nvim.env.LOUISELM_AGENT_COMMAND = "custom-acp-agent"
  local process, original_system = fake_process()
  local original_notify = nvim.notify
  local notifications = {}
  rawset(nvim, "notify", function(message, level)
    notifications[#notifications + 1] = { message = message, level = level }
  end)
  local original_input = nvim.ui.input
  local input_calls = 0
  nvim.ui.input = function()
    input_calls = input_calls + 1
  end

  Command.register()
  nvim.api.nvim_cmd({ cmd = "LouiselmChat", args = {} }, {})
  nvim.api.nvim_cmd({ cmd = "LouiselmResume", args = {} }, {})
  nvim.api.nvim_cmd({ cmd = "LouiselmInline", args = {} }, {})

  nvim.ui.input = original_input
  rawset(nvim, "notify", original_notify)
  rawset(nvim, "system", original_system)
  restore_environment("DEEPSEEK_API_KEY", original_api_key)
  restore_environment("LOUISELM_AGENT_COMMAND", original_command)

  MiniTest.expect.equality(process.command, nil)
  MiniTest.expect.equality(input_calls, 0)
  MiniTest.expect.equality(notifications, {
    {
      message = "louiselm: no Agent configured; add one to require('louiselm').setup({ agents = ... })",
      level = nvim.log.levels.WARN,
    },
    {
      message = "louiselm: no Agent configured; add one to require('louiselm').setup({ agents = ... })",
      level = nvim.log.levels.WARN,
    },
    {
      message = "louiselm: no Agent configured; add one to require('louiselm').setup({ agents = ... })",
      level = nvim.log.levels.WARN,
    },
  })
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
  MiniTest.expect.equality(process.writes[4]:find('"params":{}', 1, true) ~= nil, true)
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

T["command"]["ignores a stale health result after a newer chat check starts"] = function()
  local definition = mock_definition()
  definition.latest = { command = "npm", args = { "view", "mock-acp", "version" } }
  Command.configure({ agents = { mock = definition } })

  local original_system = nvim.system
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
  local original_executable = nvim.fn.executable
  rawset(nvim.fn, "executable", function()
    return 1
  end)
  local original_notify = nvim.notify
  local notifications = {}
  rawset(nvim, "notify", function(message, level)
    notifications[#notifications + 1] = { message = message, level = level }
  end)

  local function health_calls(start)
    local version_call, latest_call
    for index = start, #calls do
      local entry = calls[index]
      if entry.command[1] == "npm" then
        latest_call = entry
      elseif entry.command[#entry.command] == "--version" then
        version_call = entry
      end
    end
    return assert(version_call), assert(latest_call)
  end

  Command.register()
  nvim.api.nvim_cmd({ cmd = "LouiselmChat", args = {} }, {})
  local first_version, first_latest = health_calls(1)
  Session.dispose_all()
  delete_chat_buffers()

  local second_start = #calls + 1
  Command.register()
  nvim.api.nvim_cmd({ cmd = "LouiselmChat", args = {} }, {})
  local second_version, second_latest = health_calls(second_start)

  second_latest.on_exit({ code = 0, signal = 0, stdout = "1.7.0\n", stderr = "" })
  second_version.on_exit({ code = 0, signal = 0, stdout = "mock-acp 1.7.0\n", stderr = "" })
  first_latest.on_exit({ code = 0, signal = 0, stdout = "1.7.0\n", stderr = "" })
  first_version.on_exit({ code = 0, signal = 0, stdout = "mock-acp 1.6.2\n", stderr = "" })

  rawset(nvim, "system", original_system)
  rawset(nvim.fn, "executable", original_executable)
  rawset(nvim, "notify", original_notify)
  Command.configure(nil)

  MiniTest.expect.equality(notifications, {})
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
