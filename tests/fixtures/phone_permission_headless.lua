-- Isolated Session and desktop responder for the pinned Android permission test.
---@diagnostic disable-next-line: undefined-global -- Neovim provides the fixture runtime.
local nvim = vim
local root = assert(nvim.env.LOUISELM_TEST_PROJECT)
nvim.opt.runtimepath:prepend(root)
local Session = require("louiselm.session")
local Louise = require("louiselm")
local agent = {
  provider = "test-service",
  command = nvim.v.progpath,
  env = { LOUISELM_MOCK_MODE = "permission" },
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
local permission, completed
local session = assert(api:create_session("codex", {
  cwd = root,
  name = "Permission",
  on_event = function(event)
    if event.type == "permission_requested" then
      permission = event
    end
  end,
}))
assert(nvim.wait(10000, function()
  return session:inspect().status == "ready" and Louise.phone_control_subject(session) ~= nil
end))
assert(session:prompt("fixture request", function(_, err)
  assert(err == nil, err)
  completed = true
end))
assert(nvim.wait(10000, function()
  return permission ~= nil
end))
local target = assert(Louise.phone_control_subject(session))
io.stdout:write(nvim.json.encode(target), "\n")
io.stdout:flush()
local closed = false
local input = assert(nvim.uv.new_pipe(false))
assert(input:open(0) == 0)
input:read_start(function(err, data)
  assert(not err, err)
  if data == nil or data:find("stop", 1, true) then
    closed = true
  elseif data:find("resolve", 1, true) then
    nvim.schedule(function()
      assert(not completed)
      local response = { outcome = { outcome = "selected", optionId = "allow-once" } }
      assert(permission.respond(response))
      assert(not permission.respond(response))
      io.stdout:write("resolved\n")
      io.stdout:flush()
    end)
  end
end)
assert(nvim.wait(300000, function()
  return closed
end))
input:close()
assert(api:dispose())
nvim.cmd("qa!")
