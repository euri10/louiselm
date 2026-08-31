local MiniTest = require("mini.test")
local AttentionClient = require("louiselm.workflow.attention_client")
local RunClient = require("louiselm.workflow.run_client")
local Attention = require("louiselm.ui.attention")

local T = MiniTest.new_set()

T["emits unseen turns only after inactivity and clears when seen"] = function()
  local original_read = RunClient.read_operator_capability
  local original_connect = AttentionClient.connect
  local fake = {
    ready = true,
    disposed = false,
    upserts = {},
    clears = {},
    clear_sessions = {},
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
  local attention = Attention.new({
    socket_path = "/tmp/attention.sock",
    capability_path = "/tmp/operator-capability",
    schedule = function(_, callback)
      scheduled[#scheduled + 1] = callback
    end,
    now_ms = function()
      return 123
    end,
    on_error = function(message)
      error(message)
    end,
  })
  local state = {
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

  scheduled[#scheduled]()
  MiniTest.expect.equality(#fake.eligibilities, 1)
  MiniTest.expect.equality(fake.eligibilities[1].value, true)
  attention:seen("session-1")
  MiniTest.expect.equality(fake.clear_sessions, { "session-1", "session-1" })

  attention:dispose()
  MiniTest.expect.equality(fake.disposed, true)
  RunClient.read_operator_capability = original_read
  AttentionClient.connect = original_connect
end

T["deduplicates typed conditions and clears their authoritative transitions"] = function()
  local original_read = RunClient.read_operator_capability
  local original_connect = AttentionClient.connect
  local fake = { ready = true, disposed = false, upserts = {}, clears = {}, clear_sessions = {} }
  function fake:upsert(value, callback)
    self.upserts[#self.upserts + 1] = value
    callback({ generation = #self.upserts, items = {} })
    return true
  end
  function fake:set_eligible(_, _, callback)
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
    return fake
  end

  local attention = Attention.new({
    socket_path = "/tmp/attention.sock",
    capability_path = "/tmp/operator-capability",
    schedule = function() end,
    now_ms = function()
      return 123
    end,
    on_error = function(message)
      error(message)
    end,
  })
  local state = {
    status = "error",
    acp_session_id = "session-1",
    current_turn = 4,
  }
  attention:permission_required(state, { request_id = 7 })
  connected({ generation = 0, items = {} })
  attention:permission_required(state, { request_id = 7 })
  MiniTest.expect.equality(#fake.upserts, 1)
  attention:permission_resolved("session-1", 7)
  MiniTest.expect.equality(#fake.clears, 1)

  attention:session_failed(state, "11111111-2222-4333-8444-555555555555")
  MiniTest.expect.equality(#fake.upserts, 2)
  MiniTest.expect.equality(fake.upserts[2].linked_run_id, "11111111-2222-4333-8444-555555555555")
  attention:prompt_started("session-1")
  MiniTest.expect.equality(fake.clear_sessions, { "session-1" })

  attention:run_parked("11111111-2222-4333-8444-555555555555")
  MiniTest.expect.equality(#fake.upserts, 3)
  attention:run_resumed("11111111-2222-4333-8444-555555555555")
  MiniTest.expect.equality(#fake.clears, 2)

  attention:dispose()
  RunClient.read_operator_capability = original_read
  AttentionClient.connect = original_connect
end

return T
