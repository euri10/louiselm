local MiniTest = require("mini.test")
local Session = require("louiselm.session")
local Chat = require("louiselm.ui.chat")

---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim

local T = MiniTest.new_set()

T["a headless Session owns replay and shares its transcript with a later ChatView"] = function()
  local directory = nvim.fn.tempname()
  local api = assert(Session.new({
    mock = {
      provider = "test-service",
      command = nvim.v.progpath,
      args = {
        "--headless",
        "--noplugin",
        "-u",
        "tests/mock/init.lua",
        "-c",
        "luafile tests/fixtures/usage_replay.lua",
      },
      env = { LOUISELM_TEST_REPLAY_TURNS = "2" },
    },
  }, nil, { usage_directory = directory }))
  local chat
  MiniTest.finally(function()
    if chat then
      chat:dispose()
    end
    assert(api:dispose())
    local finished = false
    api:flush_recording(function()
      finished = true
    end)
    assert(nvim.wait(6000, function()
      return finished
    end))
    nvim.fn.delete(directory, "rf")
  end)
  local session = assert(api:load_session("mock", "prior-acp"))
  assert(nvim.wait(6000, function()
    return session:inspect().status == "ready"
  end))
  local before = session.transcript:snapshot()
  MiniTest.expect.equality(before[1].text, "prompt 1continued 1")
  MiniTest.expect.equality(before[2].text, "answer 1")
  local done = false
  assert(session:prompt("headless instruction", function(_, err)
    assert(err == nil, err)
    done = true
  end))
  assert(nvim.wait(6000, function()
    return done
  end))
  chat = assert(Chat.new(api, { agents = { "mock" }, start_insert_on_switch = false }))
  assert(chat:attach(session))
  MiniTest.expect.equality(chat.views[session:inspect().id].transcript == session.transcript, true)
  local entries = session.transcript:snapshot()
  MiniTest.expect.equality(entries[7], { kind = "user", text = "headless instruction" })
  assert(chat:submit("desktop instruction"))
  assert(nvim.wait(6000, function()
    return session:inspect().current_turn == 2 and session:inspect().status == "ready"
  end))
  local count = 0
  for _, entry in ipairs(session.transcript:snapshot()) do
    if entry.kind == "user" and entry.text == "desktop instruction" then
      count = count + 1
    end
  end
  MiniTest.expect.equality(count, 1)
end

return T
