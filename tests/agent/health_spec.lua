local MiniTest = require("mini.test")
local Health = require("louiselm.agent.health")

local T = MiniTest.new_set()

local function with_stubs(executable, system, callback)
  ---@diagnostic disable-next-line: undefined-global
  local original_executable = vim.fn.executable
  ---@diagnostic disable-next-line: undefined-global
  local original_system = vim.system
  ---@diagnostic disable-next-line: undefined-global
  vim.fn.executable = executable
  ---@diagnostic disable-next-line: undefined-global
  vim.system = system
  local ok, err = pcall(callback)
  ---@diagnostic disable-next-line: undefined-global
  vim.fn.executable = original_executable
  ---@diagnostic disable-next-line: undefined-global
  vim.system = original_system
  if not ok then
    error(err)
  end
end

T["check"] = MiniTest.new_set()

T["check"]["reports executable availability and detected version"] = function()
  local calls = {}
  local report
  local fake_handle = {}

  with_stubs(function(command)
    calls.executable = command
    return 1
  end, function(command, options, on_exit)
    calls.command = command
    calls.options = options
    on_exit({ code = 0, signal = 0, stdout = "claude-agent-acp 1.2.3\n", stderr = "" })
    return fake_handle
  end, function()
    local handle, err = Health.check({ command = "claude-agent-acp", args = {} }, function(result)
      report = result
    end)

    MiniTest.expect.equality(err, nil)
    MiniTest.expect.equality(handle, fake_handle)
  end)

  MiniTest.expect.equality(calls.executable, "claude-agent-acp")
  MiniTest.expect.equality(calls.command, { "claude-agent-acp", "--version" })
  MiniTest.expect.equality(calls.options, { text = true })
  MiniTest.expect.equality(report.ok, true)
  MiniTest.expect.equality(report.available, true)
  MiniTest.expect.equality(report.version, "claude-agent-acp 1.2.3")
end

---Stub `vim.fn.executable`/`vim.system` without invoking `vim.system`'s
---completion callback synchronously, so tests can drive each independent
---process completion in whichever order a real fast-event scheduler would.
---@param executable function
---@param callback fun(calls: table[])
local function with_deferred_stubs(executable, callback)
  ---@diagnostic disable-next-line: undefined-global
  local original_executable = vim.fn.executable
  ---@diagnostic disable-next-line: undefined-global
  local original_system = vim.system
  local calls = {}
  ---@diagnostic disable-next-line: undefined-global
  vim.fn.executable = executable
  ---@diagnostic disable-next-line: undefined-global
  vim.system = function(command, options, on_exit)
    local entry = { command = command, options = options, on_exit = on_exit, handle = {} }
    calls[#calls + 1] = entry
    return entry.handle
  end
  local ok, err = pcall(callback, calls)
  ---@diagnostic disable-next-line: undefined-global
  vim.fn.executable = original_executable
  ---@diagnostic disable-next-line: undefined-global
  vim.system = original_system
  if not ok then
    error(err)
  end
end

T["check"]["includes wrapper args before --version"] = function()
  local report

  with_deferred_stubs(function()
    return 1
  end, function(calls)
    local handle, err = Health.check({
      command = "acp-debug.sh",
      args = { "codex-acp" },
    }, function(result)
      report = result
    end)

    MiniTest.expect.equality(err, nil)
    assert(handle ~= nil)
    MiniTest.expect.equality(calls[1].command, { "acp-debug.sh", "codex-acp", "--version" })

    calls[1].on_exit({ code = 0, signal = 0, stdout = "@agentclientprotocol/codex-acp 1.1.14\n", stderr = "" })
  end)

  assert(report ~= nil)
  MiniTest.expect.equality(report.ok, true)
  MiniTest.expect.equality(report.version, "@agentclientprotocol/codex-acp 1.1.14")
end

T["check"]["installed-version override"] = MiniTest.new_set()

T["check"]["installed-version override"]["uses the override command/args verbatim instead of appending --version to the launch args"] = function()
  local report

  with_deferred_stubs(function()
    return 1
  end, function(calls)
    local handle, err = Health.check({
      command = "/opt/acp-debug.sh",
      args = { "acp-llm-adapter", "serve", "--backend", "deepseek" },
      version = { command = "/opt/acp-debug.sh", args = { "acp-llm-adapter", "--version" } },
    }, function(result)
      report = result
    end)

    MiniTest.expect.equality(err, nil)
    assert(handle ~= nil)
    MiniTest.expect.equality(#calls, 1)
    MiniTest.expect.equality(calls[1].command, { "/opt/acp-debug.sh", "acp-llm-adapter", "--version" })

    calls[1].on_exit({ code = 0, signal = 0, stdout = "acp-llm-adapter 0.7.2\n", stderr = "" })
  end)

  assert(report ~= nil)
  MiniTest.expect.equality(report.ok, true)
  MiniTest.expect.equality(report.command, "/opt/acp-debug.sh")
  MiniTest.expect.equality(report.version, "acp-llm-adapter 0.7.2")
end

T["check"]["installed-version override"]["uses the override's own env instead of the agent's launch env"] = function()
  with_deferred_stubs(function()
    return 1
  end, function(calls)
    Health.check({
      command = "/opt/acp-debug.sh",
      args = { "acp-llm-adapter", "serve", "--backend", "deepseek" },
      env = { LLM_API_KEY = "secret" },
      version = { command = "/opt/acp-debug.sh", args = { "acp-llm-adapter", "--version" }, env = { NO_COLOR = "1" } },
    }, function() end)

    MiniTest.expect.equality(calls[1].options, { text = true, env = { NO_COLOR = "1" } })
  end)
end

T["check"]["installed-version override"]["reports a missing override executable without spawning"] = function()
  local report

  with_stubs(function(command)
    if command == "/opt/acp-debug.sh" then
      return 1
    end
    return 0
  end, function()
    error("vim.system must not be called when the override executable is missing")
  end, function()
    local handle, err = Health.check({
      command = "/opt/acp-debug.sh",
      args = { "acp-llm-adapter", "serve", "--backend", "deepseek" },
      version = { command = "missing-version-checker", args = {} },
    }, function(result)
      report = result
    end)

    MiniTest.expect.equality(handle, nil)
    MiniTest.expect.equality(err, "missing-version-checker: executable not found on PATH")
  end)

  MiniTest.expect.equality(report.ok, false)
  MiniTest.expect.equality(report.available, true)
  MiniTest.expect.equality(report.error, "executable not found on PATH")
end

T["check"]["latest version"] = MiniTest.new_set()

T["check"]["latest version"]["combines both independent completions into one outdated report"] = function()
  local report

  with_deferred_stubs(function()
    return 1
  end, function(calls)
    local handle, err = Health.check({
      command = "codex-agent-acp",
      args = {},
      latest = {
        command = "npm",
        args = { "view", "codex-acp", "version" },
        env = { NPM_CONFIG_LOGLEVEL = "error" },
      },
    }, function(result)
      report = result
    end)

    MiniTest.expect.equality(err, nil)
    assert(handle ~= nil)
    MiniTest.expect.equality(#calls, 2)
    MiniTest.expect.equality(calls[1].command, { "codex-agent-acp", "--version" })
    MiniTest.expect.equality(calls[1].options, { text = true })
    MiniTest.expect.equality(calls[2].command, { "npm", "view", "codex-acp", "version" })
    MiniTest.expect.equality(calls[2].options, { text = true, env = { NPM_CONFIG_LOGLEVEL = "error" } })

    -- Two independent vim.system calls; the latest check resolves first here
    -- to prove the combined callback waits for both regardless of order.
    calls[2].on_exit({ code = 0, signal = 0, stdout = "1.4.0\n", stderr = "" })
    MiniTest.expect.equality(report, nil)
    calls[1].on_exit({ code = 0, signal = 0, stdout = "codex-agent-acp 1.3.0\n", stderr = "" })
  end)

  assert(report ~= nil)
  MiniTest.expect.equality(report.ok, true)
  MiniTest.expect.equality(report.version, "codex-agent-acp 1.3.0")
  MiniTest.expect.equality(report.latest_version, "1.4.0")
  MiniTest.expect.equality(report.outdated, true)
end

T["check"]["latest version"]["fires the health callback exactly once for two independent completions"] = function()
  local calls_to_result = 0

  with_deferred_stubs(function()
    return 1
  end, function(calls)
    Health.check({
      command = "codex-agent-acp",
      args = {},
      latest = { command = "npm", args = { "view", "codex-acp", "version" } },
    }, function()
      calls_to_result = calls_to_result + 1
    end)

    calls[1].on_exit({ code = 0, signal = 0, stdout = "codex-agent-acp 1.3.0\n", stderr = "" })
    calls[2].on_exit({ code = 0, signal = 0, stdout = "1.3.0\n", stderr = "" })
  end)

  MiniTest.expect.equality(calls_to_result, 1)
end

T["check"]["latest version"]["reports outdated false when the installed banner carries a name prefix the latest check doesn't"] = function()
  local report

  with_deferred_stubs(function()
    return 1
  end, function(calls)
    Health.check({
      command = "codex-acp",
      args = {},
      latest = { command = "npm", args = { "view", "@agentclientprotocol/codex-acp", "version" } },
    }, function(result)
      report = result
    end)

    -- codex-acp's own --version banner includes the package name; npm view
    -- returns a bare semver. Both resolve to the same 1.6.0, so this must
    -- not be reported as outdated just because the raw strings differ.
    calls[1].on_exit({ code = 0, signal = 0, stdout = "@agentclientprotocol/codex-acp 1.6.0\n", stderr = "" })
    calls[2].on_exit({ code = 0, signal = 0, stdout = "1.6.0\n", stderr = "" })
  end)

  MiniTest.expect.equality(report.version, "@agentclientprotocol/codex-acp 1.6.0")
  MiniTest.expect.equality(report.latest_version, "1.6.0")
  MiniTest.expect.equality(report.outdated, false)
end

T["check"]["latest version"]["reports outdated false when the installed and latest versions match"] = function()
  local report

  with_deferred_stubs(function()
    return 1
  end, function(calls)
    Health.check({
      command = "codex-agent-acp",
      args = {},
      latest = { command = "npm", args = { "view", "codex-acp", "version" } },
    }, function(result)
      report = result
    end)

    calls[1].on_exit({ code = 0, signal = 0, stdout = "1.3.0\n", stderr = "" })
    calls[2].on_exit({ code = 0, signal = 0, stdout = "1.3.0\n", stderr = "" })
  end)

  MiniTest.expect.equality(report.version, "1.3.0")
  MiniTest.expect.equality(report.latest_version, "1.3.0")
  MiniTest.expect.equality(report.outdated, false)
end

T["check"]["latest version"]["records a latest-check error without failing the primary version check"] = function()
  local report

  with_deferred_stubs(function(command)
    if command == "npm" then
      return 0
    end
    return 1
  end, function(calls)
    Health.check({
      command = "codex-agent-acp",
      args = {},
      latest = { command = "npm", args = { "view", "codex-acp", "version" } },
    }, function(result)
      report = result
    end)

    -- The latest executable is missing, so only the primary version check spawns.
    MiniTest.expect.equality(#calls, 1)
    calls[1].on_exit({ code = 0, signal = 0, stdout = "codex-agent-acp 1.3.0\n", stderr = "" })
  end)

  MiniTest.expect.equality(report.ok, true)
  MiniTest.expect.equality(report.version, "codex-agent-acp 1.3.0")
  MiniTest.expect.equality(report.latest_version, nil)
  MiniTest.expect.equality(report.latest_error, "executable not found on PATH")
  MiniTest.expect.equality(report.outdated, nil)
end

T["check"]["latest version"]["does not spawn a second process when latest is not configured"] = function()
  with_deferred_stubs(function()
    return 1
  end, function(calls)
    Health.check({ command = "codex-agent-acp", args = {} }, function() end)
    calls[1].on_exit({ code = 0, signal = 0, stdout = "codex-agent-acp 1.3.0\n", stderr = "" })
    MiniTest.expect.equality(#calls, 1)
  end)
end

T["check"]["reports a missing executable without spawning"] = function()
  local process_started = false
  local report

  with_stubs(function()
    return 0
  end, function()
    process_started = true
    return {}
  end, function()
    local handle, err = Health.check({ command = "missing-agent", args = {} }, function(result)
      report = result
    end)

    MiniTest.expect.equality(handle, nil)
    MiniTest.expect.equality(err, "missing-agent: executable not found on PATH")
  end)

  MiniTest.expect.equality(process_started, false)
  MiniTest.expect.equality(report.ok, false)
  MiniTest.expect.equality(report.available, false)
  MiniTest.expect.equality(report.error, "executable not found on PATH")
end

return T
