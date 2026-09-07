local MiniTest = require("mini.test")
local Session = require("louiselm.session")

---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local resources = {}
local directories = {}
local processes = {}

local function wait_for(predicate)
  assert(nvim.wait(6000, predicate, 10), "asynchronous recording did not finish")
end

local function query(path, sql)
  local result = nvim.system({ "sqlite3", "-json", path, sql }, { text = true }):wait()
  assert(result.code == 0, result.stderr)
  return result.stdout == "" and {} or nvim.json.decode(result.stdout)
end

local function new_session(directory, mode, load_id, extra_env)
  local root = nvim.fn.getcwd()
  local api = assert(Session.new({
    mock = {
      provider = "test-service",
      command = nvim.v.progpath,
      args = {
        "--headless",
        "--noplugin",
        "-u",
        root .. "/tests/mock/init.lua",
        "-c",
        "lua require('louiselm.dev.mock_agent').run()",
      },
      env = nvim.tbl_extend("force", { LOUISELM_MOCK_MODE = mode or "echo" }, extra_env or {}),
    },
  }, nil, { usage_directory = directory }))
  resources[#resources + 1] = api
  local session
  if load_id then
    session = assert(api:load_session("mock", load_id, { cwd = root }))
  else
    session = assert(api:create_session("mock", { cwd = root }))
  end
  wait_for(function()
    return session:inspect().status == "ready"
  end)
  return session, api
end

local T = MiniTest.new_set({
  hooks = {
    post_case = function()
      for _, process in ipairs(processes) do
        process:kill(9)
        process:wait()
      end
      processes = {}
      for _, api in ipairs(resources) do
        assert(api:dispose())
        local settled = false
        api:flush_recording(function()
          settled = true
        end)
        wait_for(function()
          return settled
        end)
      end
      resources = {}
      for _, directory in ipairs(directories) do
        nvim.fn.delete(directory, "rf")
      end
      directories = {}
    end,
  },
})

T["headless unmeasured turns survive reopen and resumed local ordinals"] = function()
  local directory = nvim.fn.tempname()
  directories[#directories + 1] = directory
  local first = new_session(directory)
  local done = false
  local id = assert(first:prompt("not stored", function(_, err)
    assert(err == nil, err)
    done = true
  end))
  MiniTest.expect.equality(first:inspect().status, "preparing")
  wait_for(function()
    return done and not first:inspect().recording_pending
  end)
  local second = new_session(directory, nil, first:inspect().acp_session_id)
  done = false
  local next_id = assert(second:prompt("also not stored", function(_, err)
    assert(err == nil, err)
    done = true
  end))
  wait_for(function()
    return done and not second:inspect().recording_pending
  end)
  MiniTest.expect.equality(id ~= next_id, true)
  local rows = query(directory .. "/turns.sqlite3", "SELECT * FROM turns ORDER BY prepared_at, id")
  MiniTest.expect.equality(#rows, 2)
  MiniTest.expect.equality(rows[1].provider, "test-service")
  MiniTest.expect.equality(
    query(directory .. "/turns.sqlite3", "SELECT COUNT(*) AS n FROM turn_events WHERE kind = 'outcome'")[1].n,
    2
  )
end

T["real mock observes its committed start through a transient write lock"] = function()
  local directory = nvim.fn.tempname()
  directories[#directories + 1] = directory
  local session = new_session(directory, nil, nil, { LOUISELM_MOCK_RECORDING_PROBE = directory .. "/turns.sqlite3" })
  local reply, done, completion_error
  session:on(function(event)
    if event.type == "state_changed" and event.data.status == "prompting" then
      -- Admission has committed; another writer can briefly exclude readers.
      -- Hold that window deliberately instead of relying on dispatch-write timing.
      local held = false
      processes[#processes + 1] = nvim.system({ "sqlite3", directory .. "/turns.sqlite3" }, {
        stdin = "BEGIN EXCLUSIVE;\nSELECT 'held';\n.shell sleep 0.5\nROLLBACK;\n",
        stdout = function(_, data)
          if data and data:find("held", 1, true) then
            held = true
          end
        end,
      })
      wait_for(function()
        return held
      end)
    end
    if event.type == "chunk" then
      reply = event.data.content.text
    end
  end)
  local id = assert(session:prompt("sensitive text never stored", function(_, err)
    completion_error = err
    done = true
  end))
  wait_for(function()
    return done and not session:inspect().recording_pending
  end)
  MiniTest.expect.equality(completion_error, nil)
  MiniTest.expect.equality(nvim.json.decode(reply), { { id = id } })
  local db = directory .. "/turns.sqlite3"
  MiniTest.expect.equality(nvim.uv.fs_stat(directory).mode % 512, 448)
  MiniTest.expect.equality(nvim.uv.fs_stat(db).mode % 512, 384)
  MiniTest.expect.equality(query(db, "PRAGMA journal_mode")[1].journal_mode, "delete")
end

T["concurrent writers preserve distinct turns without waiting in the editor"] = function()
  local directory = nvim.fn.tempname()
  directories[#directories + 1] = directory
  local first = new_session(directory)
  local second = new_session(directory)
  local done = 0
  local function completed(_, err)
    assert(err == nil, err)
    done = done + 1
  end
  local a = assert(first:prompt("a", completed))
  local b = assert(second:prompt("b", completed))
  MiniTest.expect.equality(first:inspect().status, "preparing")
  MiniTest.expect.equality(second:inspect().status, "preparing")
  wait_for(function()
    return done == 2 and not first:inspect().recording_pending and not second:inspect().recording_pending
  end)
  MiniTest.expect.equality(a ~= b, true)
  MiniTest.expect.equality(query(directory .. "/turns.sqlite3", "SELECT COUNT(*) AS n FROM turns")[1].n, 2)
end

for _, mode in ipairs({ "permission", "crash" }) do
  T["retains " .. mode .. " terminal facts"] = function()
    local directory = nvim.fn.tempname()
    directories[#directories + 1] = directory
    local session = new_session(directory, mode)
    local done, completion_error
    session:on(function(event)
      if event.type == "permission_requested" then
        assert(session:cancel())
      end
    end)
    assert(session:prompt("not recorded", function(_, err)
      done = true
      completion_error = err
    end))
    wait_for(function()
      return done and not session:inspect().recording_pending
    end)
    local outcome = query(directory .. "/turns.sqlite3", "SELECT data FROM turn_events WHERE kind='outcome'")[1]
    local data = nvim.json.decode(outcome.data)
    MiniTest.expect.equality(data.outcome, mode == "permission" and "cancelled" or "failed")
    MiniTest.expect.equality(data.peer_response, mode == "permission")
    MiniTest.expect.equality(completion_error ~= nil, mode == "crash")
    MiniTest.expect.equality(data.usage, nil)
  end
end

T["recording failure keeps active work alive and blocks later dispatch until recovery"] = function()
  local directory = nvim.fn.tempname()
  directories[#directories + 1] = directory
  local active, api = new_session(directory, "permission")
  local next_session = assert(api:create_session("mock", { cwd = nvim.fn.getcwd() }))
  wait_for(function()
    return next_session:inspect().status == "ready"
  end)
  local permission, active_done
  active:on(function(event)
    if event.type == "permission_requested" then
      permission = event
    end
  end)
  assert(active:prompt("active", function(_, err)
    assert(err == nil, err)
    active_done = true
  end))
  wait_for(function()
    return permission ~= nil and not active:inspect().recording_pending
  end)
  assert(nvim.uv.fs_chmod(directory, 320))
  local failed = false
  api:flush_recording(function(err)
    failed = true
    assert(err.code == "permissions")
  end)
  wait_for(function()
    return failed
  end)
  MiniTest.expect.equality(active:inspect().status, "waiting_permission")
  MiniTest.expect.equality(active:inspect().recording_error.code, "permissions")
  local blocked_calls = 0
  local blocked_id = assert(next_session:prompt("blocked", function(_, err)
    assert(err ~= nil)
    blocked_calls = blocked_calls + 1
  end))
  wait_for(function()
    return blocked_calls == 1
  end)
  MiniTest.expect.equality(next_session:inspect().status, "ready")
  assert(permission.respond({ outcome = { outcome = "selected", optionId = "allow-once" } }))
  wait_for(function()
    return active_done
  end)
  MiniTest.expect.equality(active:inspect().recording_error.code, "permissions")
  assert(nvim.uv.fs_chmod(directory, 448))
  local recovered
  api:flush_recording(function(err)
    assert(err == nil)
    recovered = true
  end)
  wait_for(function()
    return recovered
  end)
  MiniTest.expect.equality(active:inspect().recording_error, nil)
  local rows = query(directory .. "/turns.sqlite3", "SELECT turn_id FROM turn_events WHERE kind='dispatch'")
  MiniTest.expect.equality(#rows, 1)
  MiniTest.expect.equality(rows[1].turn_id ~= blocked_id, true)
  next_session:on(function(event)
    if event.type == "permission_requested" then
      assert(next_session:cancel())
    end
  end)
  local done
  assert(next_session:prompt("recovered", function(_, err)
    assert(err == nil, err)
    done = true
  end))
  wait_for(function()
    return done and not next_session:inspect().recording_pending
  end)
  MiniTest.expect.equality(blocked_calls, 1)
end

T["missing sqlite blocks initial dispatch and a retry preserves the failed attempt"] = function()
  local directory = nvim.fn.tempname()
  directories[#directories + 1] = directory
  local session = new_session(directory)
  local original_path = nvim.env.PATH
  local replies, completions = 0, 0
  session:on(function(event)
    if event.type == "chunk" then
      replies = replies + 1
    end
  end)
  nvim.env.PATH = directory
  local failure
  assert(session:prompt("unsent", function(_, err)
    failure = err
    completions = completions + 1
  end))
  wait_for(function()
    return completions == 1
  end)
  nvim.env.PATH = original_path
  MiniTest.expect.equality(session:inspect().recording_error.code, "unavailable")
  MiniTest.expect.equality(type(failure), "string")
  MiniTest.expect.equality(replies, 0)
  assert(session:prompt("retry", function(_, err)
    assert(err == nil, err)
    completions = completions + 1
  end))
  wait_for(function()
    return completions == 2 and not session:inspect().recording_pending
  end)
  MiniTest.expect.equality(replies, 1)
  local outcomes = query(
    directory .. "/turns.sqlite3",
    "SELECT json_extract(data,'$.outcome') AS outcome FROM turn_events WHERE kind='outcome' ORDER BY outcome"
  )
  MiniTest.expect.equality(outcomes, { { outcome = "completed" }, { outcome = "not_sent" } })
end

for _, action in ipairs({ "cancel", "dispose" }) do
  T["preparing observer " .. action .. " preserves recording order and the next Session"] = function()
    local directory = nvim.fn.tempname()
    directories[#directories + 1] = directory
    local first, api = new_session(directory)
    local witness = assert(api:create_session("mock", { cwd = nvim.fn.getcwd() }))
    wait_for(function()
      return witness:inspect().status == "ready"
    end)
    local settled = false
    witness:on(function(event)
      if event.type == "recording_changed" and (event.data.error or not event.data.pending) then
        settled = true
      end
    end)
    first:on(function(event)
      if event.type == "state_changed" and event.data.status == "preparing" then
        assert(first[action](first))
      end
    end)
    assert(first:prompt("never dispatched"))
    wait_for(function()
      return settled
    end)
    MiniTest.expect.equality(witness:inspect().recording_error, nil)
    MiniTest.expect.equality(
      query(directory .. "/turns.sqlite3", "SELECT COUNT(*) AS n FROM turn_events WHERE kind='dispatch'")[1].n,
      0
    )
    local done = false
    assert(witness:prompt("still works", function(_, err)
      assert(err == nil, err)
      done = true
    end))
    wait_for(function()
      return done and not witness:inspect().recording_pending
    end)
    MiniTest.expect.equality(query(directory .. "/turns.sqlite3", "SELECT COUNT(*) AS n FROM turns")[1].n, 2)
  end
end

return T
