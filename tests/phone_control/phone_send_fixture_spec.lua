local MiniTest = require("mini.test")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local T = MiniTest.new_set()

local function read(path)
  local file = io.open(path, "rb")
  if file == nil then
    return nil
  end
  local contents = file:read("*a")
  file:close()
  return contents
end

local function start_time(pid)
  local stat = read("/proc/" .. pid .. "/stat")
  local fields = stat and stat:match("^%d+ %b() (.*)$")
  return fields and nvim.split(fields, " ", { plain = true })[20] or nil
end

local function descendants(pid, result)
  result = result or {}
  local children = read("/proc/" .. pid .. "/task/" .. pid .. "/children") or ""
  for child in children:gmatch("%d+") do
    local started = start_time(child)
    if started ~= nil then
      result[#result + 1] =
        { pid = tonumber(child), started = started, command = read("/proc/" .. child .. "/cmdline") }
      descendants(child, result)
    end
  end
  return result
end

local function survivors(processes)
  local result = {}
  for _, process in ipairs(processes) do
    if start_time(process.pid) == process.started then
      result[#result + 1] = process.pid
    end
  end
  return result
end

local function exercise(mode)
  local root, directory = nvim.fn.getcwd(), nvim.fn.tempname()
  local workflow = directory .. "/louiselm/workflow"
  assert(nvim.fn.mkdir(workflow, "p") == 1)
  assert(nvim.fn.writefile({ "00112233-4455-6677-8899-aabbccddeeff" }, workflow .. "/operator-capability", "b") == 0)
  local listener, peer = assert(nvim.uv.new_pipe(false)), nil
  assert(listener:bind(workflow .. "/phone-control.sock") == 0)
  assert(listener:listen(1, function(err)
    assert(not err, err)
    peer = assert(nvim.uv.new_pipe(false))
    assert(listener:accept(peer) == 0)
    peer:read_start(function(read_error, data)
      assert(not read_error, read_error)
      if data ~= nil then
        peer:write('{"v":1,"type":"attached","controller_id":"11111111111111111111111111111111"}\n')
      end
    end)
  end) == 0)

  local sentinel = nvim.system({ "/bin/sleep", "120" })
  local stdout, stderr, processes = "", "", {}
  ---@type {code: integer}?
  local result
  local editor = nvim.system({
    nvim.v.progpath,
    "--headless",
    "--noplugin",
    "-u",
    "NONE",
    "-l",
    root .. "/tests/fixtures/phone_send_headless.lua",
  }, {
    cwd = root,
    clear_env = true,
    env = {
      HOME = directory,
      PATH = "/usr/bin:/bin",
      LANG = "C.UTF-8",
      LOUISELM_CAPTURE_STATE_DIR = directory,
      LOUISELM_TEST_CAPTURE = "/bin/false",
      LOUISELM_TEST_PROJECT = root,
      LOUISELM_TEST_RECEIPTS = directory,
      LOUISELM_TEST_SEND_TEXT = "fixture prompt",
    },
    stdin = true,
    stdout = function(err, data)
      assert(not err, err)
      stdout = stdout .. (data or "")
    end,
    stderr = function(err, data)
      assert(not err, err)
      stderr = stderr .. (data or "")
    end,
  }, function(value)
    result = value
  end)

  MiniTest.finally(function()
    if result == nil then
      for _, process in ipairs(descendants(editor.pid)) do
        processes[#processes + 1] = process
      end
    end
    for index = #processes, 1, -1 do
      local process = processes[index]
      if start_time(process.pid) == process.started then
        nvim.uv.kill(process.pid, "sigkill")
      end
    end
    if result == nil then
      editor:kill("sigkill")
      nvim.wait(5000, function()
        return result ~= nil
      end)
    end
    if not sentinel:is_closing() then
      sentinel:kill("sigkill")
      sentinel:wait(5000)
    end
    if peer ~= nil and not peer:is_closing() then
      peer:close()
    end
    if not listener:is_closing() then
      listener:close()
    end
    nvim.fn.delete(directory, "rf")
  end)

  assert(
    nvim.wait(15000, function()
      return stdout:find("\n", 1, true) ~= nil or result ~= nil
    end),
    "fixture did not report readiness: " .. stderr
  )
  assert(result == nil, "fixture exited before readiness: " .. stderr)
  processes = descendants(editor.pid)
  local mock_agents = 0
  for _, process in ipairs(processes) do
    local executable = process.command and process.command:match("^[^%z]+")
    if
      executable
      and nvim.fn.fnamemodify(executable, ":t") == "nvim"
      and process.command:find("mock_agent", 1, true)
    then
      mock_agents = mock_agents + 1
    end
  end
  assert(mock_agents == 2, "both disposable echo Agents must be running")
  if mode == "assertion" then
    assert(nvim.fn.writefile({
      nvim.json.encode({
        method = "session/prompt",
        params = { prompt = { { type = "text", text = "unexpected" } } },
      }),
    }, directory .. "/selected", "a") == 0)
  end
  if mode == "eof" then
    editor:write(nil)
  else
    editor:write("stop\n")
  end
  assert(
    nvim.wait(15000, function()
      return result ~= nil
    end),
    "fixture did not exit"
  )
  assert(result ~= nil)
  if mode == "assertion" then
    assert(result.code ~= 0 and stderr:find("unexpected ACP prompt", 1, true), "expected prompt assertion: " .. stderr)
  else
    assert(result.code == 0, stderr)
    local reports = nvim.split(nvim.trim(stdout), "\n", { plain = true })
    local report = nvim.json.decode(reports[2])
    MiniTest.expect.equality(report.selected_prompts, 0)
    MiniTest.expect.equality(report.other_prompts, 0)
  end
  assert(
    nvim.wait(5000, function()
      return #survivors(processes) == 0
    end),
    "fixture descendants still exist: " .. table.concat(survivors(processes), ", ")
  )
  assert(not sentinel:is_closing(), "an unrelated disposable process was stopped")
end

T["reaps all echo Agent descendants after normal completion"] = function()
  exercise("stop")
end

T["reaps all echo Agent descendants after a failed assertion"] = function()
  exercise("assertion")
end

T["reaps all echo Agent descendants when the test closes stdin"] = function()
  exercise("eof")
end

T["relay records the real ACP prompt sent to the echo Agent"] = function()
  local root, wire = nvim.fn.getcwd(), nvim.fn.tempname()
  local stdout, processes = "", {}
  ---@type {code: integer}?
  local result
  local relay = nvim.system({
    "/usr/bin/python3",
    root .. "/tests/fixtures/phone_send_agent.py",
    nvim.v.progpath,
    "--headless",
    "--noplugin",
    "-u",
    root .. "/tests/mock/init.lua",
    "-c",
    "lua require('louiselm.dev.mock_agent').run()",
  }, {
    cwd = root,
    env = { LOUISELM_FIXTURE_WIRE = wire },
    stdin = true,
    stdout = function(err, data)
      assert(not err, err)
      stdout = stdout .. (data or "")
    end,
  }, function(value)
    result = value
  end)
  MiniTest.finally(function()
    if result == nil then
      processes = descendants(relay.pid)
      relay:write(nil)
      if not nvim.wait(5000, function()
        return result ~= nil
      end) then
        for _, process in ipairs(processes) do
          if start_time(process.pid) == process.started then
            nvim.uv.kill(process.pid, "sigkill")
          end
        end
        relay:kill("sigkill")
        nvim.wait(5000, function()
          return result ~= nil
        end)
      end
    end
    nvim.fn.delete(wire)
  end)
  local prompt = { { type = "text", text = "fixture prompt" } }
  local requests = {
    { jsonrpc = "2.0", id = 1, method = "initialize", params = { protocolVersion = 1 } },
    { jsonrpc = "2.0", id = 2, method = "session/new", params = { cwd = root, mcpServers = {} } },
    { jsonrpc = "2.0", id = 3, method = "session/prompt", params = { sessionId = "mock-session-1", prompt = prompt } },
  }
  for _, request in ipairs(requests) do
    relay:write(nvim.json.encode(request) .. "\n")
  end
  assert(
    nvim.wait(5000, function()
      processes = descendants(relay.pid)
      return stdout:find('"stopReason":"end_turn"', 1, true) ~= nil
    end),
    "mock Agent did not echo the prompt"
  )
  assert(#processes == 1, "relay must own exactly one mock Agent")
  local received = {}
  for _, line in ipairs(nvim.fn.readfile(wire)) do
    received[#received + 1] = nvim.json.decode(line)
  end
  MiniTest.expect.equality(received, requests)
  assert(stdout:find("fixture prompt", 1, true), "mock Agent did not echo the actual prompt")
  relay:write(nil)
  assert(
    nvim.wait(5000, function()
      return result ~= nil
    end),
    "relay did not exit"
  )
  assert(result ~= nil)
  assert(result.code == 0, "relay failed")
  MiniTest.expect.equality(survivors(processes), {})
end

T["relay reaps an unresponsive child even while its input pipe is full"] = function()
  local root, wire = nvim.fn.getcwd(), nvim.fn.tempname()
  local stdout, processes = "", {}
  ---@type {code: integer}?
  local result
  local relay = nvim.system({
    "/usr/bin/python3",
    root .. "/tests/fixtures/phone_send_agent.py",
    "/usr/bin/python3",
    "-u",
    "-c",
    "import signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); print('ready'); time.sleep(60)",
  }, {
    cwd = root,
    env = { LOUISELM_FIXTURE_WIRE = wire },
    stdin = true,
    stdout = function(err, data)
      assert(not err, err)
      stdout = stdout .. (data or "")
    end,
  }, function(value)
    result = value
  end)
  MiniTest.finally(function()
    if result == nil then
      processes = descendants(relay.pid)
      relay:kill("sigterm")
      if not nvim.wait(5000, function()
        return result ~= nil
      end) then
        for _, process in ipairs(processes) do
          if start_time(process.pid) == process.started then
            nvim.uv.kill(process.pid, "sigkill")
          end
        end
        relay:kill("sigkill")
        nvim.wait(5000, function()
          return result ~= nil
        end)
      end
    end
    nvim.fn.delete(wire)
  end)
  assert(
    nvim.wait(5000, function()
      return stdout:find("ready", 1, true) ~= nil
    end),
    "disposable child did not start"
  )
  processes = descendants(relay.pid)
  assert(#processes == 1, "relay must own exactly one disposable child")
  local payload = string.rep("x", 1024 * 1024) .. "\n"
  relay:write(payload)
  assert(
    nvim.wait(5000, function()
      local stat = nvim.uv.fs_stat(wire)
      return stat ~= nil and stat.size == #payload
    end),
    "relay did not receive the test input"
  )
  relay:kill("sigterm")
  assert(
    nvim.wait(5000, function()
      return result ~= nil
    end),
    "relay did not stop its unresponsive child"
  )
  assert(result ~= nil and result.code == 0, "relay failed")
  MiniTest.expect.equality(survivors(processes), {})
end

return T
