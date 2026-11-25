-- Isolated dummy Run and mock ACP Session for the real phone-control boundary.
---@diagnostic disable-next-line: undefined-global -- Neovim supplies the fixture runtime.
local nvim = vim
local root = assert(nvim.env.LOUISELM_TEST_PROJECT)
local run_id = assert(nvim.env.LOUISELM_TEST_RUN_ID)
nvim.opt.runtimepath:prepend(root)
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
local phone = assert(require("louiselm.phone_control").start({
  phone_control = { enabled = true },
  capture = { service = { assert(nvim.env.LOUISELM_TEST_CAPTURE) } },
}))
local api =
  assert(require("louiselm.session").new({ codex = agent }, nil, { usage_directory = nvim.env.LOUISELM_TEST_USAGE }))
local session = assert(api:create_session("codex", { cwd = root, name = "Run fixture" }))
local executor = assert(require("louiselm.workflow.beads").new({
  envelope = { run_id = run_id, max_sessions = 6, bead_scope = { issue_ids = { "a", "b", "c" }, max_mutations = 3 } },
  bead_ids = { "a", "b", "c" },
  agent_id = "codex",
  prepared = {
    registration_id = assert(nvim.env.LOUISELM_TEST_REGISTRATION),
    revision = 1,
    selection_sha256 = assert(nvim.env.LOUISELM_TEST_SELECTION_SHA256),
  },
  prepare = function()
    return false, "fixture never starts a worker"
  end,
  on_worker = function() end,
}))
assert(executor.run:adopt_session(session))
-- Capture has already admitted this exact dummy registration. The fixture
-- does not dispatch broker authority or execute a paid Run.
executor.phone_admitted, executor.status = true, "active"
executor.current_progress = { run_id = run_id, bead_id = "a", index = 1, total = 3, phase = "working" }
assert(phone:track_run(executor))
assert(nvim.wait(10000, function()
  return session:inspect().status == "ready" and phone:subject(session) ~= nil
end))
local finished = false
assert(session:prompt("dummy Run fixture request", function(_, err)
  assert(err == nil, err)
  finished = true
end))
assert(nvim.wait(10000, function()
  return finished
end))
local identity = assert(phone:subject(session))
local snapshot = assert(phone.controller.runs:snapshot(run_id, identity.controller_id, function(value)
  return phone:subject(value)
end))
io.stdout:write(
  nvim.json.encode({
    controller_id = identity.controller_id,
    subject_id = identity.subject_id,
    owner_id = snapshot.owner_id,
    acp_session_id = assert(session:inspect().acp_session_id),
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
phone:untrack_run(executor)
assert(phone:dispose())
assert(executor:dispose())
assert(api:dispose())
nvim.cmd("qa!")
