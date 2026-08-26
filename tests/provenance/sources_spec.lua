local MiniTest = require("mini.test")

local Sources = require("louiselm.provenance.sources")

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim
local T = MiniTest.new_set()

local function fake_runtime()
  local original_system = nvim.system
  local original_schedule = nvim.schedule
  local processes = {}
  local scheduled = {}
  rawset(nvim, "system", function(command, options, callback)
    local process = { command = command, options = options, callback = callback }
    processes[#processes + 1] = process
    return process
  end)
  nvim.schedule = function(callback)
    scheduled[#scheduled + 1] = callback
  end
  return {
    processes = processes,
    scheduled = scheduled,
    restore = function()
      rawset(nvim, "system", original_system)
      nvim.schedule = original_schedule
    end,
  }
end

T["git log"] = MiniTest.new_set()

T["git log"]["uses an argument array and schedules completion"] = function()
  local runtime = fake_runtime()
  local ok, error_message = pcall(function()
    local completed
    local started, start_error = Sources.git_log("/repo", "HEAD~2..HEAD", function(commits, callback_error)
      completed = { commits = commits, error_value = callback_error }
    end)
    MiniTest.expect.equality(started, true)
    MiniTest.expect.equality(start_error, nil)
    MiniTest.expect.equality(runtime.processes[1].command, {
      "git",
      "log",
      "--no-decorate",
      "--no-color",
      "--format=%H%x00%B%x00%x1e",
      "HEAD~2..HEAD",
    })
    MiniTest.expect.equality(runtime.processes[1].options.cwd, "/repo")
    MiniTest.expect.equality(completed, nil)

    runtime.processes[1].callback({
      code = 0,
      stdout = "deadbeef" .. string.char(0) .. "chore: test\n" .. string.char(0) .. string.char(30),
      stderr = "",
    })
    MiniTest.expect.equality(completed, nil)
    MiniTest.expect.equality(#runtime.scheduled, 1)
    runtime.scheduled[1]()
    MiniTest.expect.equality(completed.error_value, nil)
    MiniTest.expect.equality(completed.commits[1], { id = "deadbeef", message = "chore: test\n" })
  end)
  runtime.restore()
  assert(ok, error_message)
end

T["git log"]["reports process failure as structured callback error"] = function()
  local runtime = fake_runtime()
  local ok, error_message = pcall(function()
    local completed
    assert(Sources.git_log("/repo", "HEAD", function(commits, callback_error)
      completed = { commits = commits, error_value = callback_error }
    end))
    runtime.processes[1].callback({ code = 128, stdout = "", stderr = "not a repository\n" })
    runtime.scheduled[1]()
    MiniTest.expect.equality(completed.commits, nil)
    MiniTest.expect.equality(completed.error_value.code, "git_failed")
    MiniTest.expect.equality(completed.error_value.exit_code, 128)
    MiniTest.expect.equality(completed.error_value.detail, "not a repository")
  end)
  runtime.restore()
  assert(ok, error_message)
end

T["git log"]["rejects missing inputs before spawning"] = function()
  local runtime = fake_runtime()
  local ok, error_message = pcall(function()
    local started, start_error = Sources.git_log("", "HEAD", function() end)
    MiniTest.expect.equality(started, false)
    assert(start_error ~= nil)
    MiniTest.expect.equality(start_error.code, "invalid_cwd")
    MiniTest.expect.equality(#runtime.processes, 0)
  end)
  runtime.restore()
  assert(ok, error_message)
end

return T
