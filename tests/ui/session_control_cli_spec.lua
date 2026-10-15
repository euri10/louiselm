local MiniTest = require("mini.test")

---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local T = MiniTest.new_set()
local root = nvim.fn.getcwd()

local function server(source)
  local directory = nvim.fn.tempname()
  assert(nvim.fn.mkdir(directory .. "/lua", "p") == 1)
  if source then
    assert(nvim.fn.writefile(nvim.split(source, "\n", { plain = true }), directory .. "/lua/louiselm.lua") == 0)
  end
  local child = MiniTest.new_child_neovim()
  MiniTest.finally(function()
    child.stop()
    nvim.fn.delete(directory, "rf")
  end)
  child.start({ "--noplugin", "-u", "NONE" })
  child.lua("vim.fn.serverstart(...)", { directory .. "/editor.sock" })
  child.lua("vim.opt.runtimepath:prepend(...)", { directory })
  return child, directory
end

local function run(directory, arguments, input, socket)
  local command = { root .. "/scripts/session-control" }
  nvim.list_extend(command, arguments)
  return nvim
    .system(command, {
      cwd = directory,
      text = true,
      stdin = input,
      env = { NVIM = socket or directory .. "/editor.sock" },
    })
    :wait(8000)
end

local function records(result)
  local output = {}
  for line in result.stdout:gmatch("[^\n]+") do
    output[#output + 1] = nvim.json.decode(line)
  end
  return output
end

local stub = [[
local M = { requests = {} }
function M.session_control(request)
  M.requests[#M.requests + 1] = request
  return { id = request.id, revision = 1, phase = "completed", result = request.result and { text = "answer", truncated = false } or nil }
end
return M
]]

T["start forwards JSON data from a file or stdin without evaluating prompt text"] = function()
  local child, directory = server(stub)
  local request = {
    id = "qa-quoted",
    agent = "mock",
    cwd = directory,
    prompt = "quotes '\" \\ and $(touch /tmp/not-executed)\nreturn error('not code')",
    options = { { id = "mode", value = false } },
    timeout_ms = false,
  }
  local encoded = nvim.json.encode(request)
  assert(nvim.fn.writefile({ encoded }, directory .. "/request.json") == 0)
  local result = run(directory, { "start", "request.json" })
  MiniTest.expect.equality(result.code, 0)
  MiniTest.expect.equality(records(result)[1].id, request.id)
  request.action = "start"
  MiniTest.expect.equality(child.lua_get("require('louiselm').requests[1]"), request)
  local stdin_result = run(directory, { "start", "-" }, encoded)
  MiniTest.expect.equality(stdin_result.code, 0)
  MiniTest.expect.equality(child.lua_get("require('louiselm').requests[2]"), request)
end

T["inspect result and cancel send only their selected operation"] = function()
  local child, directory = server(stub)
  local inspected = run(directory, { "inspect", "qa", "--result" })
  MiniTest.expect.equality(inspected.code, 0)
  MiniTest.expect.equality(records(inspected)[1].result.text, "answer")
  local cancelled = run(directory, { "cancel", "qa" })
  MiniTest.expect.equality(cancelled.code, 0)
  MiniTest.expect.equality(child.lua_get("require('louiselm').requests"), {
    { action = "inspect", id = "qa", result = true },
    { action = "cancel", id = "qa" },
  })
end

T["discover forwards only explicit selectors without starting a child"] = function()
  local child, directory = server(stub)
  local result = run(directory, { "discover", "mock", "known", "model" })
  MiniTest.expect.equality(result.code, 0)
  MiniTest.expect.equality(child.lua_get("require('louiselm').requests"), {
    { action = "discover", agent = "mock", session_id = "known", option_id = "model" },
  })
  MiniTest.expect.equality(run(directory, { "discover" }).code, 0)
end

local function manifest(directory)
  local result = {
    agent = "mock",
    cwd = directory,
    branch = "main",
    timeout_ms = false,
    options = { { id = "model", value = "sol" }, { id = "effort", value = "max" }, { id = "mode", value = "full" } },
    tasks = {},
  }
  for index = 1, 5 do
    result.tasks[index] =
      { id = "task-" .. index, issue = "issue-" .. index, name = "Task " .. index, prompt = "Do issue-" .. index }
  end
  return result
end

T["batch validates every task and checks the branch before any start"] = function()
  local child, directory = server(stub)
  assert(nvim.system({ "git", "init", "--initial-branch=other", directory }):wait().code == 0)
  local input = manifest(directory)
  local result = run(directory, { "batch", "-" }, nvim.json.encode(input))
  MiniTest.expect.equality(result.code, 1)
  assert(records(result)[1].error:find("branch", 1, true))
  input.branch, input.tasks[5].prompt = nil, ""
  result = run(directory, { "batch", "-" }, nvim.json.encode(input))
  MiniTest.expect.equality(result.code, 1)
  input.tasks[5].prompt, input.tasks[5].id = "valid", input.tasks[1].id
  result = run(directory, { "batch", "-" }, nvim.json.encode(input))
  MiniTest.expect.equality(result.code, 1)
  MiniTest.expect.equality(child.lua_get("#require('louiselm').requests"), 0)
end

T["batch returns dispatched children and preserves per-ID partial failures"] = function()
  local child, directory = server([[
local M = { starts = {}, inspections = {} }
function M.session_control(request)
  if request.action == "start" then
    M.starts[#M.starts + 1] = request
    if request.id == "task-2" then return nil, "option unavailable" end
    return { id = request.id, phase = "starting", revision = 1 }
  end
  M.inspections[request.id] = (M.inspections[request.id] or 0) + 1
  return { id = request.id, phase = "running", revision = 2,
    admission = "dispatched", status = request.id == "task-3" and "waiting_permission" or "prompting",
    session_id = request.id .. "-session", turn_id = request.id .. "-turn", options = { model = "sol", effort = "max", mode = "full" } }
end
return M
]])
  assert(nvim.system({ "git", "init", "--initial-branch=main", directory }):wait().code == 0)
  local result = run(directory, { "batch", "-", "--timeout-ms", "2000" }, nvim.json.encode(manifest(directory)))
  MiniTest.expect.equality(result.code, 1)
  local rows = records(result)
  MiniTest.expect.equality(#rows, 5)
  assert(#result.stdout < 10240)
  local by_id = {}
  for _, row in ipairs(rows) do
    by_id[row.id] = row
  end
  MiniTest.expect.equality(by_id["task-2"].error, "option unavailable")
  MiniTest.expect.equality(by_id["task-3"].status, "waiting_permission")
  MiniTest.expect.equality(by_id["task-5"].admission, "dispatched")
  MiniTest.expect.equality(by_id["task-5"].issue, "issue-5")
  local starts = child.lua_get("require('louiselm').starts")
  MiniTest.expect.equality(#starts, 5)
  for index, request in ipairs(starts) do
    MiniTest.expect.equality(request.cwd, directory)
    MiniTest.expect.equality(request.timeout_ms, false)
    MiniTest.expect.equality(request.options, manifest(directory).options)
    MiniTest.expect.equality(request.prompt, "Do issue-" .. index)
  end
end

T["batch lost acknowledgments report original IDs without replay or cancellation"] = function()
  local child, directory = server([[
local M = { requests = {} }
function M.session_control(request)
  M.requests[#M.requests + 1] = request
  if #M.requests == 1 then
    return { id = request.id, phase = "completed", revision = 1 }
  end
  vim.wait(1000, function() return false end)
  return { id = request.id, phase = "starting", revision = 1 }
end

return M
]])
  local input = manifest(directory)
  input.branch = nil
  local result = run(directory, { "batch", "-", "--timeout-ms", "100" }, nvim.json.encode(input))
  MiniTest.expect.equality(result.code, 1)
  local rows = records(result)
  MiniTest.expect.equality(#rows, 5)
  for index, row in ipairs(rows) do
    MiniTest.expect.equality(row.id, "task-" .. index)
    if index == 1 then
      MiniTest.expect.equality(row.outcome, nil)
    else
      MiniTest.expect.equality(row.outcome, "unknown")
    end
  end
  MiniTest.expect.equality(rows[1].phase, "completed")
  -- The second request might have been admitted; preserve the first ACK and never replay either.
  MiniTest.expect.equality(child.lua_get("#require('louiselm').requests"), 2)
end

T["one batch starts five real configured mock Sessions exactly once in the background"] = function()
  local child, directory = server()
  child.lua("vim.opt.runtimepath:prepend(...)", { root })
  assert(nvim.system({ "git", "init", "--initial-branch=main", directory }):wait().code == 0)
  child.lua(
    [[
    local root = ...
    assert(require("louiselm").setup({ agents = { mock = {
      provider = "test-service", command = vim.v.progpath,
      args = { "--headless", "--noplugin", "-i", "NONE", "--cmd", "set runtimepath^=" .. vim.fn.fnameescape(root),
        "-u", root .. "/tests/mock/init.lua", "-l", root .. "/tests/mock/session_control.lua" },
    } } }))
    vim.api.nvim_buf_set_lines(0, 0, -1, false, { "human draft stays here" })
  ]],
    { root }
  )
  local buffer, windows = child.api.nvim_get_current_buf(), child.api.nvim_list_wins()
  local input = manifest(directory)
  local started = nvim.uv.hrtime()
  local result = run(directory, { "batch", "-", "--timeout-ms", "5000" }, nvim.json.encode(input))
  local elapsed_ms = (nvim.uv.hrtime() - started) / 1000000
  MiniTest.expect.equality(result.code, 0)
  local rows = records(result)
  MiniTest.expect.equality(#rows, 5)
  assert(#result.stdout < 10240)
  MiniTest.add_note(
    string.format(
      "five-child batch: one helper invocation, %d output bytes, %.1f ms (offline mock)",
      #result.stdout,
      elapsed_ms
    )
  )
  local repeated = run(directory, { "batch", "-", "--timeout-ms", "5000" }, nvim.json.encode(input))
  MiniTest.expect.equality(repeated.code, 0)
  MiniTest.expect.equality(child.api.nvim_get_current_buf(), buffer)
  MiniTest.expect.equality(child.api.nvim_list_wins(), windows)
  MiniTest.expect.equality(child.api.nvim_buf_get_lines(buffer, 0, -1, false), { "human draft stays here" })
  MiniTest.expect.equality(child.lua_get("#require('louiselm.session').exit_verdict()"), 5)
  for _, row in ipairs(rows) do
    MiniTest.expect.equality(row.admission, "dispatched")
    MiniTest.expect.equality(row.options, { model = "sol", effort = "max", mode = "full" })
    assert(row.session_id and row.acp_session_id and row.turn_id)
    local actual = child.lua_get(
      [[(function(id)
      for _, verdict in ipairs(require("louiselm.session").exit_verdict()) do
        local state = verdict.session:inspect()
        if state.id == id then return { cwd = state.working_dir, turns = state.current_turn } end
      end
    end)(...)]],
      { row.session_id }
    )
    MiniTest.expect.equality(actual, { cwd = directory, turns = 1 })
    local found = run(directory, { "discover", "mock", row.session_id, "model" })
    MiniTest.expect.equality(found.code, 0)
    MiniTest.expect.equality(records(found)[1].options[1].current_value, "sol")
    local lines = child.api.nvim_buf_get_lines(row.buffer, 0, -1, false)
    local count = 0
    for _, line in ipairs(lines) do
      if line == "> Do " .. row.issue then
        count = count + 1
      end
    end
    MiniTest.expect.equality(count, 1)
  end
  child.lua("require('louiselm.ui.chat.command').register()")
end

T["watch emits changed snapshots once then stops at a terminal phase"] = function()
  local child, directory = server([[
local M = { calls = 0 }
function M.session_control(request)
  M.calls = M.calls + 1
  return { id = request.id, revision = M.calls < 3 and 1 or 2, phase = M.calls < 3 and "running" or "completed" }
end
return M
]])
  local result = run(directory, { "watch", "qa", "--timeout-ms", "2000" })
  MiniTest.expect.equality(result.code, 0)
  MiniTest.expect.equality(records(result), {
    { id = "qa", revision = 1, phase = "running" },
    { id = "qa", revision = 2, phase = "completed" },
  })
  MiniTest.expect.equality(child.lua_get("require('louiselm').calls"), 3)
end

T["watch deadline reports an error without cancelling the child"] = function()
  local child, directory = server([[
local M = { requests = {} }
function M.session_control(request)
  M.requests[#M.requests + 1] = request
  return { id = request.id, revision = 1, phase = "running" }
end
return M
]])
  local result = run(directory, { "watch", "qa", "--timeout-ms", "20" })
  MiniTest.expect.equality(result.code, 1)
  local output = records(result)
  MiniTest.expect.equality(output[1].phase, "running")
  MiniTest.expect.equality(output[#output].error, "watch deadline reached; current state unknown; child unchanged")
  MiniTest.expect.equality(child.lua_get("require('louiselm').requests[1].action"), "inspect")
  MiniTest.expect.equality(child.lua_get("#require('louiselm').requests"), 1)
end

T["watch deadline also bounds an editor that delays its RPC reply"] = function()
  local _, directory = server([[
return { session_control = function(request)
  vim.wait(1000, function() return false end)
  return { id = request.id, revision = 1, phase = "running" }
end }
]])
  local started = nvim.uv.hrtime()
  local result = run(directory, { "watch", "qa", "--timeout-ms", "20" })
  MiniTest.expect.equality(result.code, 1)
  MiniTest.expect.equality(records(result)[1].error, "watch deadline reached; current state unknown; child unchanged")
  MiniTest.expect.equality(records(result)[1].id, "qa")
  MiniTest.expect.equality(records(result)[1].outcome, "unknown")
  MiniTest.expect.equality((nvim.uv.hrtime() - started) / 1000000 < 900, true)
end

T["invalid arguments and bounded JSON fail before any RPC"] = function()
  local child, directory = server(stub)
  assert(nvim.fn.writefile({ string.rep(" ", 1024 * 1024 + 1) }, directory .. "/oversized.json") == 0)
  local cases = {
    { { "inspect", "qa", "--unknown" } },
    { { "cancel", "qa", "--result" } },
    { { "watch", "qa", "--timeout-ms", "60001" } },
    { { "watch", "qa", "--timeout-ms", "0" } },
    { { "watch", "qa", "--timeout-ms", "2.5" } },
    { { "start", "-" }, "{" },
    { { "start", "-" }, "[]" },
    { { "start", "-" }, '{"action":"cancel","id":"qa"}' },
    { { "start", "-" }, string.rep(" ", 1024 * 1024 + 1) },
    { { "start", "oversized.json" } },
  }
  for _, case in ipairs(cases) do
    local result = run(directory, case[1], case[2])
    MiniTest.expect.equality(result.code, 1)
    MiniTest.expect.equality(type(records(result)[1].error), "string")
  end
  MiniTest.expect.equality(child.lua_get("#require('louiselm').requests"), 0)
end

T["missing API and unavailable editor fail with JSON diagnostics"] = function()
  local _, directory = server("return {}")
  local missing = run(directory, { "inspect", "qa" })
  MiniTest.expect.equality(missing.code, 1)
  MiniTest.expect.equality(
    records(missing)[1].error,
    "configured Session controls are unavailable; update LouiseLM in the editor"
  )
  local disconnected = run(directory, { "inspect", "qa" }, nil, directory .. "/missing.sock")
  MiniTest.expect.equality(disconnected.code, 1)
  MiniTest.expect.equality(records(disconnected)[1].error, "could not connect to the editor in NVIM")
end

T["API failures remain explicit JSON errors"] = function()
  local _, directory = server([[return { session_control = function() return nil, "unknown operation" end }]])
  local result = run(directory, { "inspect", "unknown" })
  MiniTest.expect.equality(result.code, 1)
  MiniTest.expect.equality(records(result)[1].error, "unknown operation")
end

return T
