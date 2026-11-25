-- Isolated mock-ACP controller for the capture/Neovim phone-control gate.
---@diagnostic disable-next-line: undefined-global -- Neovim supplies the fixture runtime.
local nvim = vim
local root = assert(nvim.env.LOUISELM_TEST_PROJECT)
nvim.opt.runtimepath:prepend(root)
local Session = require("louiselm.session")
local Louise = require("louiselm")
local agent = {
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
}
assert(Louise.setup({
  phone_control = { enabled = true },
  capture = { service = { assert(nvim.env.LOUISELM_TEST_CAPTURE) } },
  agents = { codex = agent },
}))
local api = assert(Session.new({ codex = agent }, nil, { usage_directory = nvim.env.LOUISELM_TEST_USAGE }))
local session = assert(api:create_session("codex", { cwd = root, name = "same" }))
local other = assert(api:create_session("codex", { cwd = root, name = "same" }))
assert(nvim.wait(10000, function()
  return session:inspect().status == "ready"
    and other:inspect().status == "ready"
    and Louise.phone_control_subject(session) ~= nil
    and Louise.phone_control_subject(other) ~= nil
end))
local finished = false
assert(session:prompt("fixture request", function(_, err)
  assert(err == nil, err)
  finished = true
end))
assert(nvim.wait(10000, function()
  return finished
end))
finished = false
assert(other:prompt("other fixture request", function(_, err)
  assert(err == nil, err)
  finished = true
end))
assert(nvim.wait(10000, function()
  return finished
end))
local identity = assert(Louise.phone_control_subject(session))
io.stdout:write(
  nvim.json.encode({
    controller_id = identity.controller_id,
    subject_id = identity.subject_id,
    other_subject_id = assert(Louise.phone_control_subject(other)).subject_id,
  }),
  "\n"
)
io.stdout:flush()
local closed = false
local input = assert(nvim.uv.new_pipe(false))
assert(input:open(0) == 0)
input:read_start(function(_, data)
  if data == nil or data:find("stop", 1, true) then
    closed = true
  end
end)
assert(nvim.wait(300000, function()
  return closed
end))
input:close()
assert(api:dispose())
nvim.cmd("qa!")
