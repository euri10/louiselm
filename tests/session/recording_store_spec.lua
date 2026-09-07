local MiniTest = require("mini.test")
local Recording = require("louiselm.session.recording")

---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local directories, processes = {}, {}
local saved_path
local T = MiniTest.new_set({
  hooks = {
    post_case = function()
      if saved_path then
        nvim.env.PATH = saved_path
        saved_path = nil
      end
      for _, process in ipairs(processes) do
        process:kill(9)
        process:wait()
      end
      processes = {}
      for _, directory in ipairs(directories) do
        nvim.fn.delete(directory, "rf")
      end
      directories = {}
    end,
  },
})

local function wait_for(predicate)
  assert(nvim.wait(6000, predicate, 10), "recording callback did not arrive")
end

local function store()
  local directory = nvim.fn.tempname()
  directories[#directories + 1] = directory
  return assert(Recording.new(directory, function()
    assert(not nvim.in_fast_event())
  end))
end

local function prepared(id)
  return {
    id = id or "one",
    agent = "mock",
    provider = "service",
    acp_session_id = "session",
    prepared_at = "2026-09-07T12:00:00Z",
    options = { model = "small", enabled = false },
    model = "small",
  }
end

local function append(writer, record)
  local done, result
  writer:append(record, function(err)
    assert(not nvim.in_fast_event())
    done = true
    result = err
  end)
  wait_for(function()
    return done
  end)
  return result
end

local function query(writer, sql)
  local result = nvim.system({ "sqlite3", "-json", writer.path, sql }, { text = true }):wait()
  assert(result.code == 0, result.stderr)
  return result.stdout == "" and {} or nvim.json.decode(result.stdout)
end

T["canonical retries preserve typed options and reject conflicting facts"] = function()
  local writer = store()
  local turn = prepared("quote'\n.quit\0semicolon;")
  turn.options.model = "'); DROP TABLE turns; --\n.shell false"
  MiniTest.expect.equality(append(writer, turn), nil)
  turn.options = { enabled = false, model = turn.options.model }
  MiniTest.expect.equality(append(writer, turn), nil)
  local rows = query(writer, "SELECT hex(id) AS id_hex, options FROM turns")
  MiniTest.expect.equality(#rows, 1)
  MiniTest.expect.equality(
    rows[1].id_hex,
    turn.id:gsub(".", function(byte)
      return string.format("%02X", byte:byte())
    end)
  )
  MiniTest.expect.equality(nvim.json.decode(rows[1].options), turn.options)
  local event = {
    turn_id = turn.id,
    sequence = 1,
    kind = "outcome",
    observed_at = turn.prepared_at,
    data = { outcome = "completed", peer_response = true, usage = { input_tokens = 0, cached_write_tokens = 9 } },
  }
  MiniTest.expect.equality(append(writer, event), nil)
  MiniTest.expect.equality(append(writer, event), nil)
  MiniTest.expect.equality(query(writer, "SELECT COUNT(*) AS n FROM turn_events")[1].n, 1)
  event.data.usage.input_tokens = 10
  MiniTest.expect.equality(append(writer, event).code, "conflict")
  MiniTest.expect.equality(
    nvim.json.decode(query(writer, "SELECT data FROM turn_events")[1].data).usage.input_tokens,
    0
  )
end

T["missing sqlite and corrupt or unsafe files report typed failures"] = function()
  local writer = store()
  saved_path = nvim.env.PATH
  nvim.env.PATH = writer.directory
  MiniTest.expect.equality(append(writer, prepared()).code, "unavailable")
  nvim.env.PATH = saved_path
  saved_path = nil
  local done, recovered
  writer:flush(function(err)
    done = true
    recovered = err
  end)
  wait_for(function()
    return done
  end)
  MiniTest.expect.equality(recovered, nil)
  local corrupt = store()
  assert(nvim.fn.mkdir(corrupt.directory, "p", 448) == 1)
  nvim.fn.writefile({ "not a database" }, corrupt.path)
  assert(nvim.uv.fs_chmod(corrupt.path, 384))
  MiniTest.expect.equality(append(corrupt, prepared()).code, "corrupt")
  local unsafe = store()
  assert(nvim.fn.mkdir(unsafe.directory, "p", 493) == 1)
  MiniTest.expect.equality(append(unsafe, prepared()).code, "permissions")
  assert(nvim.uv.fs_chmod(unsafe.directory, 448))
  assert(nvim.uv.fs_symlink(writer.path, unsafe.path))
  done = false
  unsafe:flush(function(err)
    done = true
    MiniTest.expect.equality(err.code, "permissions")
  end)
  wait_for(function()
    return done
  end)
end

T["locked writes preserve their identity and acknowledge recovery once"] = function()
  local writer = store()
  MiniTest.expect.equality(append(writer, prepared()), nil)
  local held, exited = false, false
  local locker = nvim.system({ "sqlite3", writer.path }, {
    stdin = true,
    stdout = function(_, data)
      if data and data:find("held", 1, true) then
        held = true
      end
    end,
  }, function()
    exited = true
  end)
  processes[#processes + 1] = locker
  locker:write("BEGIN IMMEDIATE;\nSELECT 'held';\n")
  wait_for(function()
    return held
  end)
  local heartbeat = false
  nvim.defer_fn(function()
    heartbeat = true
  end, 20)
  local callbacks = 0
  local err ---@type louiselm.session.RecordingError?
  writer:append(prepared("two"), function(result)
    callbacks = callbacks + 1
    err = result
  end)
  wait_for(function()
    return callbacks == 1
  end)
  MiniTest.expect.equality(heartbeat, true)
  MiniTest.expect.equality(assert(err).code, "locked")
  locker:write("ROLLBACK;\n.quit\n")
  wait_for(function()
    return exited
  end)
  local recovered = false
  writer:flush(function(result)
    assert(result == nil)
    recovered = true
  end)
  wait_for(function()
    return recovered
  end)
  MiniTest.expect.equality(callbacks, 1)
  MiniTest.expect.equality(query(writer, "SELECT COUNT(*) AS n FROM turns")[1].n, 2)
end

T["incomplete turns have no invented terminal or telemetry facts"] = function()
  local writer = store()
  MiniTest.expect.equality(append(writer, prepared()), nil)
  MiniTest.expect.equality(
    append(writer, {
      turn_id = "one",
      sequence = 1,
      kind = "dispatch",
      observed_at = "2026-09-07T12:00:01Z",
      data = { request_id = 3, prompt = "never stored" },
    }),
    nil
  )
  local rows = query(writer, "SELECT kind, data FROM turn_events")
  MiniTest.expect.equality(rows, { { kind = "dispatch", data = '{"request_id":3}' } })
  MiniTest.expect.equality(
    append(writer, {
      turn_id = "one",
      sequence = 2,
      kind = "outcome",
      observed_at = "2026-09-07T12:00:02Z",
      data = { outcome = "completed", peer_response = true, usage = { input_tokens = -1 } },
    }).code,
    "invalid"
  )
  MiniTest.expect.equality(#query(writer, "SELECT * FROM turn_events"), 1)
  local rejected
  writer:flush(function(err)
    rejected = err
  end)
  wait_for(function()
    return rejected ~= nil
  end)
  MiniTest.expect.equality(rejected.code, "invalid")
end

T["refuses unsafe journals and future schemas before adding facts"] = function()
  local writer = store()
  MiniTest.expect.equality(append(writer, prepared()), nil)
  assert(nvim.uv.fs_symlink(writer.path, writer.path .. "-journal"))
  MiniTest.expect.equality(append(writer, prepared("two")).code, "permissions")
  assert(nvim.uv.fs_unlink(writer.path .. "-journal"))
  local changed = nvim.system({ "sqlite3", writer.path, "PRAGMA user_version=2;" }, { text = true }):wait()
  assert(changed.code == 0, changed.stderr)
  local err
  writer:flush(function(value)
    err = value
  end)
  wait_for(function()
    return err ~= nil
  end)
  MiniTest.expect.equality(err.code, "corrupt")
  MiniTest.expect.equality(query(writer, "SELECT COUNT(*) AS n FROM turns")[1].n, 1)
end

return T
