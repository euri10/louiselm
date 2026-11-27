---@diagnostic disable-next-line: undefined-global -- Run by a disposable headless Neovim.
local nvim = vim
local root, directory = assert(nvim.env.LOUISELM_GATE_ROOT), assert(nvim.env.LOUISELM_GATE_TMP)
nvim.opt.runtimepath:prepend(root)
local Session = require("louiselm.session")
local Gate = require("louiselm.phone_control.admission_gate")
local Process = require("louiselm.phone_control.process_identity")
local Clock = require("louiselm.phone_control.boot_clock")

local api = assert(Session.new({
  mock = {
    provider = "test-service",
    command = nvim.v.progpath,
    env = { LOUISELM_MOCK_MODE = "static" },
    args = {
      "--headless",
      "--noplugin",
      "-u",
      root .. "/tests/mock/init.lua",
      "-c",
      "lua require('louiselm.dev.mock_agent').run()",
    },
  },
}, nil, { usage_directory = directory .. "/usage" }))
local session = assert(api:create_session("mock", { cwd = root }))
assert(nvim.wait(60000, function()
  return session:inspect().status == "ready"
end))
local pipe = assert(nvim.uv.new_pipe(false))
assert(pipe:open(tonumber(nvim.env.LOUISELM_GATE_SOCKET_FD)) == 0)
local receiver_pid = assert(tonumber(nvim.env.LOUISELM_GATE_RECEIVER_PID))
assert(receiver_pid > 0 and receiver_pid % 1 == 0)
---@cast receiver_pid integer -- The fixture validates the process ID above.
local process = assert(Process.new(receiver_pid, assert(nvim.uv.fs_readlink("/proc/self/ns/pid"))))
local clock = Clock.new()
local function open(object)
  local result
  object:open(function(ok)
    result = ok
  end)
  assert(nvim.wait(10000, function()
    return result ~= nil
  end))
  assert(result)
end
open(process)
open(clock)
local text = "disposable gate test"
local revision = session.history.revision
local started = assert(clock:sample())
if nvim.env.LOUISELM_GATE_CLOCK_FD then
  local inherited = assert(tonumber(nvim.env.LOUISELM_GATE_CLOCK_FD))
  local original, replacement = assert(nvim.uv.fs_fstat(clock.fd)), assert(nvim.uv.fs_fstat(inherited))
  assert(original.dev == replacement.dev and original.ino == replacement.ino)
  assert(nvim.uv.fs_close(clock.fd))
  clock.fd = inherited
end
local gate = assert(Gate.registered({
  controller = pipe,
  receiver_process = process,
  clock = clock,
  started = started,
  remaining_ms = 5000,
  session = session,
  projection = { revision = revision },
  expected_revision = revision,
  payload_hash = nvim.fn.sha256(string.char(1) .. text),
  controller_id = string.rep("01", 16),
  operation_id = string.rep("02", 16),
  gate_id = string.rep("03", 16),
}))
local turn, err = session:prompt_phone(text, gate)
assert(
  nvim.fn.writefile(
    { nvim.json.encode({ admitted = turn ~= nil, current_turn = session:inspect().current_turn, error = err or "" }) },
    directory .. "/result.json"
  ) == 0
)
process:dispose()
clock:dispose()
pipe:close()
assert(api:dispose())
