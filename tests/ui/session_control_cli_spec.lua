local MiniTest = require("mini.test")

---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local T = MiniTest.new_set()
local root = nvim.fn.getcwd()

local function server(source)
  local directory = nvim.fn.tempname()
  assert(nvim.fn.mkdir(directory .. "/lua", "p") == 1)
  assert(nvim.fn.writefile(nvim.split(source, "\n", { plain = true }), directory .. "/lua/louiselm.lua") == 0)
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
