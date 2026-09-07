local MiniTest = require("mini.test")
local Session = require("louiselm.session")
local Chat = require("louiselm.ui.chat")
local Usage = require("louiselm.routing.usage")

---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local directory, apis, chats
local expected_usage = {
  total_tokens = 30,
  input_tokens = 20,
  output_tokens = 10,
  thought_tokens = 0,
  cached_read_tokens = 7,
  cached_write_tokens = 2,
}
local function wait_for(predicate)
  assert(nvim.wait(6000, predicate, 10), "replay did not settle")
end
local function flush(api)
  local done
  api:flush_recording(function(err)
    assert(err == nil, err and err.message)
    done = true
  end)
  wait_for(function()
    return done
  end)
end
local T = MiniTest.new_set({
  hooks = {
    pre_case = function()
      directory, apis, chats = nvim.fn.tempname(), {}, {}
    end,
    post_case = function()
      for _, chat in ipairs(chats) do
        chat:dispose()
      end
      for _, api in ipairs(apis) do
        api:dispose()
        flush(api)
      end
      nvim.fn.delete(directory, "rf")
    end,
  },
})
local function load(turns)
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
      env = { LOUISELM_TEST_REPLAY_TURNS = tostring(turns) },
    },
  }, nil, { usage_directory = directory }))
  apis[#apis + 1] = api
  local session = assert(api:load_session("mock", "prior-acp"))
  MiniTest.expect.equality(session:inspect().acp_session_id, "prior-acp")
  MiniTest.expect.equality(session:inspect().status, "starting")
  return session, api
end
local function complete(session, api)
  wait_for(function()
    return session:inspect().status == "ready"
  end)
  local done
  local id = assert(session:prompt("next prompt", function(_, err)
    assert(err == nil, err)
    done = true
  end))
  wait_for(function()
    return done
  end)
  flush(api)
  return id
end
local function history(session)
  local records
  session:usage_history(function(result, err)
    assert(not nvim.in_fast_event())
    assert(err == nil, err and err.message)
    records = result
  end)
  wait_for(function()
    return records ~= nil
  end)
  return records
end
local function query(sql)
  local result = nvim.system({ "sqlite3", "-json", directory .. "/turns.sqlite3", sql }, { text = true }):wait()
  assert(result.code == 0, result.stderr)
  return nvim.json.decode(result.stdout)
end

T["headless replay associates stable IDs and skips unsent attempts"] = function()
  local session, api = load(2)
  wait_for(function()
    return session:inspect().status == "ready"
  end)
  local cancel = true
  session:on(function(event)
    if cancel and event.type == "state_changed" and event.data.status == "preparing" then
      cancel = false
      assert(session:cancel())
    end
  end)
  local unsent = assert(session:prompt("cancel before dispatch"))
  flush(api)
  local id = complete(session, api)
  MiniTest.expect.equality(id ~= unsent, true)
  MiniTest.expect.equality(history(session), { { id = id, turn = 3, usage = expected_usage } })
  assert(session:dispose())
  local restored, next_api = load(3)
  wait_for(function()
    return restored:inspect().status == "ready"
  end)
  MiniTest.expect.equality(history(restored), { { id = id, turn = 3, usage = expected_usage } })
  local next_id = complete(restored, next_api)
  MiniTest.expect.equality(next_id ~= id, true)
  MiniTest.expect.equality(history(restored), {
    { id = id, turn = 3, usage = expected_usage },
    { id = next_id, turn = 4, usage = expected_usage },
  })
  MiniTest.expect.equality(query("SELECT count(*) AS n FROM turns")[1].n, 3)
end

T["history requested at completion waits for the queued terminal observation"] = function()
  local session = load(0)
  wait_for(function()
    return session:inspect().status == "ready"
  end)
  local records
  local id = assert(session:prompt("next prompt", function(_, err)
    assert(err == nil, err)
    -- The prompt callback precedes the asynchronous terminal-write ACK.
    MiniTest.expect.equality(session:inspect().recording_pending, true)
    session:usage_history(function(result, read_error)
      assert(read_error == nil, read_error and read_error.message)
      records = result
    end)
  end))
  wait_for(function()
    return records ~= nil
  end)
  MiniTest.expect.equality(records, { { id = id, turn = 1, usage = expected_usage } })
end

T["Chat restores SQLite and legacy annotations without writing history"] = function()
  local first, api = load(1)
  local id = complete(first, api)
  assert(first:dispose())
  local legacy_path = directory .. "/legacy.json"
  assert(nvim.fn.writefile({
    nvim.json.encode({
      version = 1,
      records = {},
      turns = {
        { agent = "mock", session_id = "prior-acp", turn = 1, usage = { total_tokens = 11 } },
        { agent = "mock", session_id = "prior-acp", turn = 2, usage = { total_tokens = 999 } },
      },
    }),
  }, legacy_path) == 0)
  local legacy_before = nvim.fn.readfile(legacy_path)
  for round = 1, 2 do
    local restored, restored_api = load(2)
    local chat = assert(Chat.new(restored_api, { markdown_highlighting = false, start_insert_on_switch = false }))
    chats[#chats + 1] = chat
    chat.usage = assert(Usage.new(legacy_path))
    assert(chat:attach(restored))
    local other = assert(restored_api:create_session("mock"))
    assert(chat:attach(other))
    assert(chat:switch(restored:inspect().id))
    local buffer = assert(chat:buffer())
    wait_for(function()
      local text = table.concat(nvim.api.nvim_buf_get_lines(buffer, 0, -1, false), "\n")
      return text:find("cached_write_tokens=2", 1, true) ~= nil
    end)
    local lines = nvim.api.nvim_buf_get_lines(buffer, 0, -1, false)
    local positions, annotations = {}, {}
    for index, line in ipairs(lines) do
      if line:find("[usage]", 1, true) then
        annotations[#annotations + 1] = line
        positions[#positions + 1] = index
      end
    end
    MiniTest.expect.equality(#annotations, 2)
    MiniTest.expect.equality(annotations[1], "[usage] total_tokens=11")
    MiniTest.expect.equality(annotations[2]:find("999", 1, true), nil)
    MiniTest.expect.equality(
      positions[1] < assert(nvim.tbl_contains(lines, "> prompt 2") and nvim.fn.index(lines, "> prompt 2") + 1),
      true
    )
    MiniTest.expect.equality(history(restored), { { id = id, turn = 2, usage = expected_usage } })
    if round == 2 then
      local next_id = complete(restored, restored_api)
      MiniTest.expect.equality(next_id ~= id, true)
      wait_for(function()
        local count = 0
        for _, line in ipairs(nvim.api.nvim_buf_get_lines(buffer, 0, -1, false)) do
          if line:find("[usage]", 1, true) then
            count = count + 1
          end
        end
        return count == 3
      end)
    end
    chat:dispose()
  end
  MiniTest.expect.equality(nvim.fn.readfile(legacy_path), legacy_before)
  MiniTest.expect.equality(query("SELECT count(*) AS n FROM turns")[1].n, 2)
end

return T
