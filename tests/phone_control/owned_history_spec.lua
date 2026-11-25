local MiniTest = require("mini.test")
local Session = require("louiselm.session")
local Controller = require("louiselm.phone_control.controller")

---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local T = MiniTest.new_set()

T["reads replay that predates phone observation and rejects stale pages"] = function()
  local root = nvim.fn.getcwd()
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
      env = { LOUISELM_TEST_REPLAY_TURNS = "180" },
    },
  }, nil, { usage_directory = directory }))
  local controller = Controller.new({ service = { root .. "/tests/fixtures/phone_control_verify_stub.sh" } })
  MiniTest.finally(function()
    assert(controller:dispose())
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
  local session = assert(api:load_session("mock", "same-acp"))
  assert(nvim.wait(6000, function()
    return session:inspect().status == "ready"
  end))
  local controller_id = string.rep("1", 32)
  assert(controller:attach(controller_id))
  local subject_id = assert(controller:track(session))
  local request = {
    v = 1,
    type = "read_session",
    controller_id = controller_id,
    grant = { projects = { assert(nvim.uv.fs_realpath(root)) }, agents = { "mock" }, groups = { "read" } },
    proof = {},
    period = {},
    request = {
      controller_id = controller_id,
      subject_id = subject_id,
      operation_id = string.rep("2", 32),
      action = 1,
      payload = "00000000000000000032",
      expected_revision = 0,
    },
  }
  local function read(before, revision)
    request.request.payload = string.format("%016x0032", before)
    request.request.expected_revision = revision
    local result
    controller:read(nvim.json.encode(request), function(value)
      result = value
    end)
    assert(nvim.wait(6000, function()
      return result ~= nil
    end))
    return result
  end

  local page = read(0, 0)
  MiniTest.expect.equality(page.type, "session_page")
  MiniTest.expect.equality(page.v, 2)
  MiniTest.expect.equality(page.source, {
    replay = "unverified",
    archive = "unavailable",
    redaction = "unknown",
    missing = true,
    truncated = false,
  })
  MiniTest.expect.equality(page.complete, false) -- ACP replay is not a completeness guarantee.
  local revision, total = page.revision, 0
  repeat
    total = total + #page.entries
    if page.next_before ~= 0 then
      page = read(page.next_before, revision)
      MiniTest.expect.equality(page.type, "session_page")
    else
      break
    end
  until false
  MiniTest.expect.equality(total, 540)
  MiniTest.expect.equality(page.entries[1].seq, 1)
  MiniTest.expect.equality(page.entries[1].text, "prompt 1continued 1")

  session.emitter:emit({
    type = "chunk",
    session_id = session:inspect().id,
    data = { content = { type = "text", text = "later output" } },
  })
  MiniTest.expect.equality(read(2, revision).code, "stale_subject")
  local fresh = read(0, 0)
  MiniTest.expect.no_equality(fresh.revision, revision)
  MiniTest.expect.equality(fresh.entries[#fresh.entries].text:find("later output", 1, true) ~= nil, true)

  session.emitter:emit({
    type = "thought_chunk",
    session_id = session:inspect().id,
    data = { content = { type = "text", text = "private reasoning" } },
  })
  local withheld = read(0, 0)
  MiniTest.expect.equality(withheld.source.redaction, "present")
  MiniTest.expect.equality(nvim.json.encode(withheld):find("private reasoning", 1, true), nil)
end

return T
