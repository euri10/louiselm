local MiniTest = require("mini.test")
local Events = require("louiselm.session.events")

local T = MiniTest.new_set()

T["new"] = MiniTest.new_set()

T["new"]["subscribes and unsubscribes event listeners"] = function()
  local emitter = Events.new()
  local received = {}
  local unsubscribe = emitter:on(function(event)
    received[#received + 1] = event
  end)

  emitter:emit({ type = "chunk", session_id = "session-1", data = "one" })
  unsubscribe()
  emitter:emit({ type = "chunk", session_id = "session-1", data = "two" })

  MiniTest.expect.equality(received, {
    { type = "chunk", session_id = "session-1", data = "one" },
  })
end

return T
