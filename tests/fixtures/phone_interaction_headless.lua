-- Real Session and desktop Chat owner for the isolated pinned Android test.
---@diagnostic disable-next-line: undefined-global -- Neovim provides the fixture runtime.
local nvim = vim
local root = assert(nvim.env.LOUISELM_TEST_PROJECT)
nvim.opt.runtimepath:prepend(root)
local Session = require("louiselm.session")
local Louise = require("louiselm")
local Chat = require("louiselm.ui.chat")
local Decisions = require("louiselm.ui.chat.decisions")
local agent = {
  provider = "test-service",
  command = nvim.v.progpath,
  env = { LOUISELM_MOCK_INTERACTION_FILE = root .. "/tests/fixtures/grok_ask_user_question.json" },
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
local interaction, completed
local session = assert(api:create_session("codex", {
  cwd = root,
  name = "Question",
  on_event = function(event)
    if event.type == "interaction_requested" then
      interaction = event
    end
  end,
}))
local decisions = Decisions.new({
  is_visible = function()
    return false
  end,
  is_live = function()
    return true
  end,
  report_error = function(message)
    error(message)
  end,
  resolved = function() end,
})
local chat =
  assert(Chat.new(api, { markdown_highlighting = false, start_insert_on_switch = false, decisions = decisions }))
assert(chat:attach(session))
assert(nvim.wait(10000, function()
  return session:inspect().status == "ready" and Louise.phone_control_subject(session) ~= nil
end))
assert(session:prompt("fixture request", function(_, err)
  assert(err == nil, err)
  completed = true
end))
assert(nvim.wait(10000, function()
  return interaction ~= nil and decisions:has_request(session, interaction.data.request_id, true)
end))
io.stdout:write(nvim.json.encode(assert(Louise.phone_control_subject(session))), "\n")
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
      local response = { outcome = "accepted", answers = { ["Which QA color?"] = { "Blue" } } }
      assert(interaction.respond(response))
      assert(not interaction.respond(response))
      assert(not decisions:has_request(session, interaction.data.request_id, true))
      io.stdout:write("resolved\n")
      io.stdout:flush()
    end)
  end
end)
assert(nvim.wait(300000, function()
  return closed
end))
input:close()
chat:dispose()
decisions:dispose()
assert(api:dispose())
nvim.cmd("qa!")
