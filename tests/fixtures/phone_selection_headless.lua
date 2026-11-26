-- Disposable configured owner with no Sessions for the private selection gate.
---@diagnostic disable-next-line: undefined-global -- Neovim supplies the fixture runtime.
local nvim = vim
local root = assert(nvim.env.LOUISELM_TEST_PROJECT)
nvim.opt.runtimepath:prepend(root)
local session = require("louiselm.session")
local agent = {
  provider = "test-service",
  command = assert(nvim.env.LOUISELM_TEST_AGENT_WRAPPER),
  env = { LOUISELM_MOCK_MODE = "static", LOUISELM_TEST_AGENT_MARKER = assert(nvim.env.LOUISELM_TEST_AGENT_MARKER) },
  args = {
    nvim.v.progpath,
    "--headless",
    "--noplugin",
    "-u",
    root .. "/tests/mock/init.lua",
    "-c",
    "lua require('louiselm.dev.mock_agent').run()",
  },
}
assert(require("louiselm").setup({
  phone_control = { enabled = true },
  capture = { service = { assert(nvim.env.LOUISELM_TEST_CAPTURE) } },
  agents = { codex = agent, unapproved = agent },
}))
assert(#session.exit_verdict() == 0)
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
assert(#session.exit_verdict() == 0)
nvim.cmd("qa!")
