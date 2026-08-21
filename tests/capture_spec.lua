local MiniTest = require("mini.test")

local Capture = require("louiselm.capture")
local CaptureCommand = require("louiselm.capture.command")

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim
local T = MiniTest.new_set()

local function fake_runtime()
  local original_system = nvim.system
  local original_schedule = nvim.schedule
  local original_stdpath = nvim.fn.stdpath
  local state_directory = nvim.fn.tempname()
  assert(nvim.fn.mkdir(state_directory, "p") == 1)
  local processes = {}
  local scheduled = {}

  nvim.fn.stdpath = function(kind)
    if kind == "state" then
      return state_directory
    end
    return original_stdpath(kind)
  end
  nvim.schedule = function(callback)
    scheduled[#scheduled + 1] = callback
  end
  rawset(nvim, "system", function(command, options, callback)
    local process = { command = command, options = options, callback = callback }
    process.handle = {
      kill = function(_, signal)
        process.signal = signal
      end,
    }
    processes[#processes + 1] = process
    return process.handle
  end)

  return {
    processes = processes,
    scheduled = scheduled,
    restore = function()
      rawset(nvim, "system", original_system)
      nvim.schedule = original_schedule
      nvim.fn.stdpath = original_stdpath
      nvim.fn.delete(state_directory, "rf")
    end,
  }
end

T["recorder"] = MiniTest.new_set()

T["recorder"]["rejects unsafe direct configuration without starting work"] = function()
  local capture, error_message = Capture.new({ recorder = { "pw-record" } })

  MiniTest.expect.equality(capture, nil)
  MiniTest.expect.equality(error_message, "capture recorder must contain {output} exactly once")
end

T["recorder"]["records, stops, then ingests only after leaving the fast event"] = function()
  local runtime = fake_runtime()
  local ok, error_message = pcall(function()
    local capture = assert(Capture.new({
      recorder = { "test-recorder", "--output", "{output}" },
      service = { "test-capture-service" },
    }))

    local id = assert(capture:start())
    local output = runtime.processes[1].command[3]
    nvim.fn.writefile({ "audio" }, output, "b")
    MiniTest.expect.equality(runtime.processes[1].command[1], "test-recorder")
    MiniTest.expect.equality(output:match("%.wav$") ~= nil, true)
    MiniTest.expect.equality(capture:is_recording(), true)

    local completed
    assert(capture:stop(function(result, completion_error)
      completed = { result = result, error_message = completion_error }
    end))
    MiniTest.expect.equality(runtime.processes[1].signal, 2)
    runtime.processes[1].callback({ code = 0, signal = 2, stdout = "", stderr = "" })

    MiniTest.expect.equality(#runtime.processes, 1)
    MiniTest.expect.equality(#runtime.scheduled, 1)
    runtime.scheduled[1]()
    MiniTest.expect.equality(runtime.processes[2].command[1], "test-capture-service")
    MiniTest.expect.equality(runtime.processes[2].command[2], "ingest-local")
    MiniTest.expect.equality(runtime.processes[2].command[3], "--file")
    MiniTest.expect.equality(runtime.processes[2].command[4], output)
    MiniTest.expect.equality(nvim.tbl_contains(runtime.processes[2].command, id), true)

    runtime.processes[2].callback({
      code = 0,
      signal = 0,
      stdout = nvim.json.encode({ id = id, outcome = "created" }),
      stderr = "",
    })
    MiniTest.expect.equality(completed, nil)
    runtime.scheduled[2]()
    MiniTest.expect.equality(completed.result.id, id)
    MiniTest.expect.equality(completed.error_message, nil)
    MiniTest.expect.equality(nvim.uv.fs_stat(output), nil)
    MiniTest.expect.equality(capture:is_recording(), false)
  end)
  runtime.restore()
  assert(ok, error_message)
end

T["recorder"]["reports recorder failure without invoking ingestion"] = function()
  local runtime = fake_runtime()
  local ok, error_message = pcall(function()
    local capture = assert(Capture.new({ recorder = { "bad-recorder", "{output}" } }))
    assert(capture:start())
    local completed
    assert(capture:stop(function(result, completion_error)
      completed = { result = result, error_message = completion_error }
    end))

    runtime.processes[1].callback({ code = 1, signal = 0, stdout = "", stderr = "microphone unavailable\n" })
    runtime.scheduled[1]()

    MiniTest.expect.equality(#runtime.processes, 1)
    MiniTest.expect.equality(completed.result, nil)
    MiniTest.expect.equality(completed.error_message, "recorder failed: microphone unavailable")
  end)
  runtime.restore()
  assert(ok, error_message)
end

T["service"] = MiniTest.new_set()

T["service"]["decodes capture listings after scheduling"] = function()
  local runtime = fake_runtime()
  local ok, error_message = pcall(function()
    local capture = assert(Capture.new({ service = { "test-capture-service" } }))
    local listed

    assert(capture:list(function(result, list_error)
      listed = { result = result, error_message = list_error }
    end))
    runtime.processes[1].callback({
      code = 0,
      signal = 0,
      stdout = nvim.json.encode({ { record = { id = "capture-id" }, state = {} } }),
      stderr = "",
    })

    MiniTest.expect.equality(listed, nil)
    runtime.scheduled[1]()
    MiniTest.expect.equality(listed.result[1].record.id, "capture-id")
    MiniTest.expect.equality(listed.error_message, nil)
  end)
  runtime.restore()
  assert(ok, error_message)
end

T["commands"] = MiniTest.new_set()

T["commands"]["register exposes capture workflow commands"] = function()
  assert(CaptureCommand.configure({}))
  assert(CaptureCommand.register())
  local commands = nvim.api.nvim_get_commands({ builtin = false })

  MiniTest.expect.equality(commands.LouiselmCapture ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmInbox ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmCaptureSetup ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmCapturePair ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmCaptureRevoke ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmCaptureStatus ~= nil, true)
  MiniTest.expect.equality(commands.LouiselmCaptureRetry ~= nil, true)
end

T["commands"]["pairing QR is black on white independently of the colorscheme"] = function()
  local runtime = fake_runtime()
  local ok, error_message = pcall(function()
    assert(CaptureCommand.configure({}))
    assert(CaptureCommand.register())
    nvim.cmd("LouiselmCapturePair https://192.0.2.1:7391")
    runtime.processes[1].callback({ code = 0, signal = 0, stdout = " █ \n██ ", stderr = "" })
    runtime.scheduled[1]()

    local highlight = nvim.api.nvim_get_hl(0, { name = "LouiselmCaptureQr" })
    MiniTest.expect.equality(highlight.fg, 0x000000)
    MiniTest.expect.equality(highlight.bg, 0xffffff)

    local namespace = nvim.api.nvim_get_namespaces().louiselm_capture_qr
    local marks = nvim.api.nvim_buf_get_extmarks(0, namespace, 0, -1, { details = true })
    MiniTest.expect.equality(#marks, 2)
    MiniTest.expect.equality(marks[1][4].hl_group, "LouiselmCaptureQr")
    MiniTest.expect.equality(marks[2][4].hl_group, "LouiselmCaptureQr")
  end)
  runtime.restore()
  assert(ok, error_message)
end

return T
