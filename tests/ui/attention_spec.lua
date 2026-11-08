local MiniTest = require("mini.test")
local AttentionClient = require("louiselm.workflow.attention_client")
local RunClient = require("louiselm.workflow.run_client")
local Attention = require("louiselm.ui.attention")
local AttentionPaste = require("louiselm.ui.attention_paste")

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local T = MiniTest.new_set()

T["disposal clears durable turn alerts even without local entries and cancels delayed delivery"] = function()
  local original_read = RunClient.read_operator_capability
  local original_connect = AttentionClient.connect
  local paste_dispatcher = AttentionPaste.new()
  local attention
  MiniTest.finally(function()
    if attention ~= nil then
      attention:dispose()
    end
    paste_dispatcher:dispose()
    RunClient.read_operator_capability = original_read
    AttentionClient.connect = original_connect
  end)
  -- Live receiver generation 3475 retained a turn_ready for a Session absent
  -- from the editor on 2026-11-08 (louiselm-3phjr). Other kinds must survive.
  local retained = {
    { subject_id = "ended", kind = "turn_ready" },
    { subject_id = "ended", kind = "skill_unverified" },
    { subject_id = "other", kind = "turn_ready" },
  }
  local scheduled = {}
  local deliveries = 0
  local fake = { ready = false }
  function fake:clear_session_kind(session_id, kind, callback)
    retained = nvim.tbl_filter(function(item)
      return item.subject_id ~= session_id or item.kind ~= kind
    end, retained)
    callback({ generation = 1, items = retained })
    return true
  end
  function fake:upsert(item, callback)
    retained[#retained + 1] = item
    callback({ generation = 2, items = retained })
    return true
  end
  function fake:set_eligible(_, _, callback)
    deliveries = deliveries + 1
    callback({ generation = 3, items = retained })
    return true
  end
  function fake:dispose()
    return true
  end
  local connected
  ---@diagnostic disable-next-line: duplicate-set-field
  RunClient.read_operator_capability = function(_, callback)
    callback("capability", nil)
    return true
  end
  ---@diagnostic disable-next-line: duplicate-set-field
  AttentionClient.connect = function(_, on_snapshot)
    connected = on_snapshot
    return fake
  end
  attention = assert(Attention.new({
    paste_dispatcher = paste_dispatcher,
    schedule = function(_, callback)
      scheduled[#scheduled + 1] = callback
    end,
  }))
  attention:session_disposed("ended")
  MiniTest.expect.equality(type(connected), "function")
  fake.ready = true
  connected({ generation = 0, items = retained })
  MiniTest.expect.equality(retained, {
    { subject_id = "ended", kind = "skill_unverified" },
    { subject_id = "other", kind = "turn_ready" },
  })
  attention:turn_done({ status = "ready", acp_session_id = "new", agent = "codex", current_turn = 1 }, false)
  MiniTest.expect.equality(#retained, 3)
  attention:session_disposed("new")
  for _, callback in ipairs(scheduled) do
    callback()
  end
  MiniTest.expect.equality(deliveries, 0)
  MiniTest.expect.equality(#retained, 2)
end

T["resuming in a fresh controller durably clears only that Session failure"] = function()
  local original_read = RunClient.read_operator_capability
  local original_connect = AttentionClient.connect
  local paste_dispatcher = AttentionPaste.new()
  local attention
  MiniTest.finally(function()
    if attention ~= nil then
      attention:dispose()
    end
    paste_dispatcher:dispose()
    RunClient.read_operator_capability = original_read
    AttentionClient.connect = original_connect
  end)
  local retained = {
    { subject_id = "resumed", kind = "session_failed" },
    { subject_id = "resumed", kind = "session_failed" },
    { subject_id = "other", kind = "session_failed" },
    { subject_id = "resumed", kind = "permission_required" },
    { subject_id = "resumed", kind = "skill_approval_pending" },
    { subject_id = "resumed", kind = "turn_ready" },
  }
  local fake = { ready = false }
  function fake:clear_session_kind(session_id, kind, callback)
    retained = nvim.tbl_filter(function(item)
      return item.subject_id ~= session_id or item.kind ~= kind
    end, retained)
    callback({ generation = 1, items = retained })
    return true
  end
  function fake:dispose()
    return true
  end
  local connected
  ---@diagnostic disable-next-line: duplicate-set-field
  RunClient.read_operator_capability = function(_, callback)
    callback("capability", nil)
    return true
  end
  ---@diagnostic disable-next-line: duplicate-set-field
  AttentionClient.connect = function(_, on_snapshot)
    connected = on_snapshot
    return fake
  end
  attention = assert(Attention.new({
    socket_path = "/tmp/attention.sock",
    capability_path = "/tmp/operator-capability",
    paste_dispatcher = paste_dispatcher,
  }))
  attention:session_resumed("resumed")
  MiniTest.expect.equality(#retained, 6)
  MiniTest.expect.equality(type(connected), "function")
  fake.ready = true
  connected({ generation = 0, items = retained })
  MiniTest.expect.equality(retained, {
    { subject_id = "other", kind = "session_failed" },
    { subject_id = "resumed", kind = "permission_required" },
    { subject_id = "resumed", kind = "skill_approval_pending" },
    { subject_id = "resumed", kind = "turn_ready" },
  })
end

T["emits unseen turns only after inactivity and clears when seen"] = function()
  local original_read = RunClient.read_operator_capability
  local original_connect = AttentionClient.connect
  local paste_dispatcher = AttentionPaste.new()
  local attention
  MiniTest.finally(function()
    if attention ~= nil then
      attention:dispose()
    end
    paste_dispatcher:dispose()
    RunClient.read_operator_capability = original_read
    AttentionClient.connect = original_connect
  end)
  local fake = {
    ready = true,
    disposed = false,
    upserts = {},
    clears = {},
    clear_sessions = {},
    clear_session_kinds = {},
    eligibilities = {},
  }
  function fake:upsert(value, callback)
    self.upserts[#self.upserts + 1] = value
    callback({ generation = 1, items = {} })
    return true
  end
  function fake:set_eligible(key, value, callback)
    self.eligibilities[#self.eligibilities + 1] = { key = key, value = value }
    callback({ generation = 2, items = {} })
    return true
  end
  function fake:clear(key, callback)
    self.clears[#self.clears + 1] = key
    callback({ generation = 3, items = {} })
    return true
  end
  function fake:clear_session(session_id, callback)
    self.clear_sessions[#self.clear_sessions + 1] = session_id
    callback({ generation = 3, items = {} })
    return true
  end
  function fake:clear_session_kind(session_id, kind, callback)
    self.clear_session_kinds[#self.clear_session_kinds + 1] = { session_id = session_id, kind = kind }
    callback({ generation = 3, items = {} })
    return true
  end
  function fake:dispose()
    self.disposed = true
    return true
  end
  local connected
  ---@diagnostic disable-next-line: duplicate-set-field
  RunClient.read_operator_capability = function(_, callback)
    callback("capability", nil)
    return true
  end
  ---@diagnostic disable-next-line: duplicate-set-field
  AttentionClient.connect = function(_, on_snapshot)
    connected = on_snapshot
    fake.ready = false
    return fake
  end

  local scheduled = {}
  attention = assert(Attention.new({
    socket_path = "/tmp/attention.sock",
    capability_path = "/tmp/operator-capability",
    paste_dispatcher = paste_dispatcher,
    schedule = function(_, callback)
      scheduled[#scheduled + 1] = callback
    end,
    now_ms = function()
      return 123
    end,
    on_error = function(message)
      error(message)
    end,
  }))
  local state = {
    id = "session-7",
    name = "Android inbox polish",
    status = "ready",
    agent = "codex",
    acp_session_id = "session-1",
    current_turn = 4,
  }
  attention:turn_done(state, false)
  MiniTest.expect.equality(fake.upserts, {})
  fake.ready = true
  connected({ generation = 0, items = {} })
  MiniTest.expect.equality(#fake.upserts, 1)
  MiniTest.expect.equality(fake.upserts[1].subject_id, "session-1")
  MiniTest.expect.equality(fake.upserts[1].created_at_ms, 123)
  MiniTest.expect.equality(fake.upserts[1].session_name, "Android inbox polish")

  scheduled[#scheduled]()
  MiniTest.expect.equality(#fake.eligibilities, 1)
  MiniTest.expect.equality(fake.eligibilities[1].value, true)
  attention:seen("session-1")
  MiniTest.expect.equality(fake.clear_session_kinds, {
    { session_id = "session-1", kind = "turn_ready" },
    { session_id = "session-1", kind = "session_failed" },
    { session_id = "session-1", kind = "turn_ready" },
  })
  MiniTest.expect.equality(fake.clear_sessions, {})

  state.name = nil
  state.current_turn = 5
  attention:turn_done(state, false)
  MiniTest.expect.equality(fake.upserts[2].session_name, "session-7")
  state.name = "\n" .. string.rep("é", 200)
  state.current_turn = 6
  attention:turn_done(state, false)
  local bounded = fake.upserts[3].session_name
  assert(#bounded <= 256 and not bounded:find("\n", 1, true))
  assert(nvim.str_utfindex(bounded, "utf-32") > 0)
  for _, name in ipairs({ "", "\t\n", string.rep(" ", 300), "  " }) do
    state.name = name
    state.current_turn = state.current_turn + 1
    attention:turn_done(state, false)
    MiniTest.expect.equality(fake.upserts[#fake.upserts].session_name, "session-7")
  end

  attention:dispose()
  MiniTest.expect.equality(fake.disposed, true)
  RunClient.read_operator_capability = original_read
  AttentionClient.connect = original_connect
end

T["deduplicates typed conditions and clears their authoritative transitions"] = function()
  local original_read = RunClient.read_operator_capability
  local original_connect = AttentionClient.connect
  local paste_dispatcher = AttentionPaste.new()
  local attention
  MiniTest.finally(function()
    if attention ~= nil then
      attention:dispose()
    end
    paste_dispatcher:dispose()
    RunClient.read_operator_capability = original_read
    AttentionClient.connect = original_connect
  end)
  local fake = {
    ready = true,
    disposed = false,
    upserts = {},
    clears = {},
    clear_sessions = {},
    clear_session_kinds = {},
    eligibilities = {},
  }
  function fake:upsert(value, callback)
    self.upserts[#self.upserts + 1] = value
    callback({ generation = #self.upserts, items = {} })
    return true
  end
  function fake:set_eligible(key, value, callback)
    self.eligibilities[#self.eligibilities + 1] = { key = key, value = value }
    callback({ generation = 0, items = {} })
    return true
  end
  function fake:clear(key, callback)
    self.clears[#self.clears + 1] = key
    callback({ generation = #self.clears, items = {} })
    return true
  end
  function fake:clear_session(session_id, callback)
    self.clear_sessions[#self.clear_sessions + 1] = session_id
    callback({ generation = #self.clear_sessions, items = {} })
    return true
  end
  function fake:clear_session_kind(session_id, kind, callback)
    self.clear_session_kinds[#self.clear_session_kinds + 1] = { session_id = session_id, kind = kind }
    callback({ generation = #self.clear_session_kinds, items = {} })
    return true
  end
  function fake:dispose()
    self.disposed = true
    return true
  end
  local connected
  local connections = 0
  ---@diagnostic disable-next-line: duplicate-set-field
  RunClient.read_operator_capability = function(_, callback)
    callback("capability", nil)
    return true
  end
  ---@diagnostic disable-next-line: duplicate-set-field
  AttentionClient.connect = function(_, on_snapshot)
    connections = connections + 1
    connected = on_snapshot
    return fake
  end

  local scheduled = {}
  attention = assert(Attention.new({
    socket_path = "/tmp/attention.sock",
    capability_path = "/tmp/operator-capability",
    paste_dispatcher = paste_dispatcher,
    schedule = function(_, callback)
      scheduled[#scheduled + 1] = callback
    end,
    now_ms = function()
      return 123
    end,
    on_error = function(message)
      error(message)
    end,
  }))
  local state = {
    status = "error",
    id = "session-8",
    name = "Permissions test",
    acp_session_id = "session-1",
    current_turn = 4,
  }
  attention:permission_required(state, { request_id = 7 })
  connected({ generation = 0, items = {} })
  attention:permission_required(state, { request_id = 7 })
  MiniTest.expect.equality(#fake.upserts, 1)
  attention:permission_resolved("session-1", 7)
  MiniTest.expect.equality(fake.upserts[1].session_name, "Permissions test")
  MiniTest.expect.equality(#fake.clears, 1)

  attention:session_failed(state, "11111111-2222-4333-8444-555555555555")
  MiniTest.expect.equality(#fake.upserts, 2)
  MiniTest.expect.equality(fake.upserts[2].linked_run_id, "11111111-2222-4333-8444-555555555555")
  MiniTest.expect.equality(fake.upserts[2].session_name, "Permissions test")
  attention:prompt_started("session-1")
  MiniTest.expect.equality(#fake.clears, 1)
  MiniTest.expect.equality(fake.clear_session_kinds, {
    { session_id = "session-1", kind = "turn_ready" },
    { session_id = "session-1", kind = "session_failed" },
  })
  MiniTest.expect.equality(fake.clear_sessions, {})

  attention:run_parked("11111111-2222-4333-8444-555555555555")
  MiniTest.expect.equality(#fake.upserts, 3)
  MiniTest.expect.equality(fake.upserts[3].session_name, nil)
  attention:run_resumed("11111111-2222-4333-8444-555555555555")
  MiniTest.expect.equality(#fake.clears, 2)

  attention:dispose()
  for _, callback in ipairs(scheduled) do
    callback()
  end
  MiniTest.expect.equality(fake.eligibilities, {})
  local counts = {
    upserts = #fake.upserts,
    clears = #fake.clears,
    clear_session_kinds = #fake.clear_session_kinds,
    queue = #attention.queue,
    entries = nvim.tbl_count(attention.entries),
  }
  attention:turn_done({ status = "ready", acp_session_id = "session-2", agent = "codex", current_turn = 1 }, false)
  attention:seen("session-2")
  attention:prompt_started("session-2")
  attention:session_resumed("session-2")
  attention:permission_required({ acp_session_id = "session-2", current_turn = 1 }, { request_id = 1 })
  attention:permission_resolved("session-2", 1)
  attention:permission_cancelled("session-2", { 1 })
  attention:session_failed({ acp_session_id = "session-2", current_turn = 1 })
  attention:run_parked("11111111-2222-4333-8444-555555555555")
  attention:run_resumed("11111111-2222-4333-8444-555555555555")
  attention:session_disposed("session-1")
  attention:activity()
  MiniTest.expect.equality(connections, 1)
  MiniTest.expect.equality({
    upserts = #fake.upserts,
    clears = #fake.clears,
    clear_session_kinds = #fake.clear_session_kinds,
    queue = #attention.queue,
    entries = nvim.tbl_count(attention.entries),
  }, counts)
  RunClient.read_operator_capability = original_read
  AttentionClient.connect = original_connect
end

return T
