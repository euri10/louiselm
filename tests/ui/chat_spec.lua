local MiniTest = require("mini.test")
local Chat = require("louiselm.ui.chat")

local T = MiniTest.new_set()

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local function fake_session(id, agent)
  local listeners = {}
  local session = {
    state = { id = id, agent = agent, status = "ready", current_turn = 0, config_options = {} },
    prompts = {},
    config_changes = {},
    disposed = false,
  }

  function session:on(callback)
    listeners[#listeners + 1] = callback
    return function()
      for index, listener in ipairs(listeners) do
        if listener == callback then
          table.remove(listeners, index)
          return
        end
      end
    end
  end

  function session:inspect()
    return self.state
  end

  function session:prompt(prompt)
    self.prompts[#self.prompts + 1] = prompt
    return #self.prompts
  end

  function session:set_config_option(option_id, value, callback)
    self.config_changes[#self.config_changes + 1] = { id = option_id, value = value }
    if callback ~= nil then
      callback(self.state.config_options)
    end
    return #self.config_changes
  end

  function session:cancel()
    self.state.status = "cancelling"
    return true
  end

  function session:dispose()
    self.disposed = true
    self.state.status = "disposed"
    return true
  end

  function session:emit(event)
    for _, listener in ipairs(listeners) do
      listener(event)
    end
  end

  return session
end

local function fake_api()
  return {
    registry = {},
    create_session = function()
      return nil, "not implemented in this test"
    end,
    get_session = function()
      return nil
    end,
    list_sessions = function()
      return {}
    end,
    dispose = function()
      return true
    end,
  }
end

local function buffer_lines(buffer)
  return nvim.api.nvim_buf_get_lines(buffer, 0, -1, false)
end

T["chat"] = MiniTest.new_set({
  hooks = {
    post_case = function()
      for _, buffer in ipairs(nvim.api.nvim_list_bufs()) do
        if nvim.api.nvim_buf_is_valid(buffer) and nvim.api.nvim_buf_get_name(buffer):match("^louiselm://") then
          nvim.api.nvim_buf_delete(buffer, { force = true })
        end
      end
    end,
  },
})

T["chat"]["renders session events and forwards slash prompts"] = function()
  local first = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))

  chat:submit("/compact")
  first:emit({
    type = "chunk",
    session_id = "session-1",
    data = { content = { type = "text", text = "hello **" } },
  })
  first:emit({
    type = "chunk",
    session_id = "session-1",
    data = { content = { type = "text", text = "world**" } },
  })
  first:emit({
    type = "tool_call_started",
    session_id = "session-1",
    data = { toolCallId = "tool-1", title = "Read file" },
  })
  first:emit({
    type = "tool_call_finished",
    session_id = "session-1",
    data = { toolCallId = "tool-1", status = "completed" },
  })
  nvim.wait(100, function()
    return #buffer_lines(chat:buffer()) == 6
  end, 1)

  MiniTest.expect.equality(first.prompts, { "/compact" })
  MiniTest.expect.equality(buffer_lines(chat:buffer()), {
    "# claude · session-1 · ready · Your turn",
    "",
    "> /compact",
    "",
    "hello **world**",
    "[tool] tool-1: Read file (completed)",
    "> ",
  })

  chat:dispose()
end

T["chat"]["keeps interleaved response and tool events chronological"] = function()
  local first = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))

  assert(chat:submit("hello"))
  first:emit({
    type = "chunk",
    session_id = "session-1",
    data = { content = { type = "text", text = "before tool" } },
  })
  first:emit({
    type = "tool_call_started",
    session_id = "session-1",
    data = { toolCallId = "tool-1", title = "Read file" },
  })
  first:emit({
    type = "tool_call_finished",
    session_id = "session-1",
    data = { toolCallId = "tool-1", status = "completed" },
  })
  first:emit({
    type = "chunk",
    session_id = "session-1",
    data = { content = { type = "text", text = "after tool" } },
  })

  nvim.wait(100, function()
    return #buffer_lines(chat:buffer()) == 7
  end, 1)

  MiniTest.expect.equality(buffer_lines(chat:buffer()), {
    "# claude · session-1 · ready · Your turn",
    "",
    "> hello",
    "",
    "before tool",
    "[tool] tool-1: Read file (completed)",
    "after tool",
    "> ",
  })

  chat:dispose()
end

T["chat"]["keeps multiline tool titles on one buffer line"] = function()
  local first = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))

  first:emit({
    type = "tool_call_started",
    session_id = "session-1",
    data = { toolCallId = "tool-1", title = "first line\nsecond line" },
  })
  first:emit({
    type = "tool_call_finished",
    session_id = "session-1",
    data = { toolCallId = "tool-1", status = "completed" },
  })

  nvim.wait(100, function()
    return #buffer_lines(chat:buffer()) == 4
  end, 1)

  MiniTest.expect.equality(buffer_lines(chat:buffer()), {
    "# claude · session-1 · ready · Your turn",
    "",
    "[tool] tool-1: first line second line (completed)",
    "> ",
  })

  chat:dispose()
end

T["chat"]["splits multiline error messages before inserting them"] = function()
  local first = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))

  first:emit({
    type = "error",
    session_id = "session-1",
    data = { message = "first line\nsecond line" },
  })
  nvim.wait(100, function()
    return #buffer_lines(chat:buffer()) == 5
  end, 1)

  MiniTest.expect.equality(buffer_lines(chat:buffer()), {
    "# claude · session-1 · ready · Your turn",
    "",
    "Error: first line",
    "second line",
    "> ",
  })

  chat:dispose()
end

T["chat"]["schedules session events before touching buffers"] = function()
  local first = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  assert(chat:submit("hello"))
  local original_schedule = nvim.schedule
  local scheduled = {}
  nvim.schedule = function(callback)
    scheduled[#scheduled + 1] = callback
  end

  first:emit({
    type = "chunk",
    session_id = "session-1",
    data = { content = { type = "text", text = "scheduled" } },
  })

  MiniTest.expect.equality(#scheduled, 1)
  MiniTest.expect.equality(
    buffer_lines(chat:buffer()),
    { "# claude · session-1 · ready · Your turn", "", "> hello", "", "> " }
  )
  scheduled[1]()
  nvim.schedule = original_schedule

  MiniTest.expect.equality(buffer_lines(chat:buffer()), {
    "# claude · session-1 · ready · Your turn",
    "",
    "> hello",
    "",
    "scheduled",
    "> ",
  })
  chat:dispose()
end

T["chat"]["keeps a blank boundary before the first scheduled assistant event"] = function()
  local first = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  assert(chat:submit("hello"))
  local original_schedule = nvim.schedule
  local scheduled = {}
  nvim.schedule = function(callback)
    scheduled[#scheduled + 1] = callback
  end

  first:emit({
    type = "chunk",
    session_id = "session-1",
    data = { content = { type = "text", text = "scheduled" } },
  })

  MiniTest.expect.equality(buffer_lines(chat:buffer()), {
    "# claude · session-1 · ready · Your turn",
    "",
    "> hello",
    "",
    "> ",
  })
  scheduled[1]()
  nvim.schedule = original_schedule

  MiniTest.expect.equality(buffer_lines(chat:buffer()), {
    "# claude · session-1 · ready · Your turn",
    "",
    "> hello",
    "",
    "scheduled",
    "> ",
  })
  chat:dispose()
end

T["chat"]["keeps the boundary when a tool call is the first event"] = function()
  local first = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  assert(chat:submit("hello"))
  local original_schedule = nvim.schedule
  local scheduled = {}
  nvim.schedule = function(callback)
    scheduled[#scheduled + 1] = callback
  end

  first:emit({
    type = "tool_call_started",
    session_id = "session-1",
    data = { toolCallId = "tool-1", title = "Read file" },
  })
  MiniTest.expect.equality(#scheduled, 1)
  scheduled[1]()
  nvim.schedule = original_schedule

  MiniTest.expect.equality(buffer_lines(chat:buffer()), {
    "# claude · session-1 · ready · Your turn",
    "",
    "> hello",
    "",
    "[tool] tool-1: Read file (started)",
    "> ",
  })
  chat:dispose()
end

T["chat"]["opens file permission requests in a scheduled diff review"] = function()
  local path = nvim.fn.tempname()
  nvim.fn.writefile({ "before" }, path)
  local first = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  local original_schedule = nvim.schedule
  local scheduled = {}
  nvim.schedule = function(callback)
    scheduled[#scheduled + 1] = callback
  end

  local response
  first:emit({
    type = "permission_requested",
    session_id = "session-1",
    data = {
      operation = { kind = "file_edit", path = path },
      toolCall = { rawInput = { path = path, content = "after\n" } },
      options = { { optionId = "allow-once", kind = "allow_once" }, "deny" },
    },
    respond = function(result)
      response = result
      return true
    end,
  })

  MiniTest.expect.equality(#scheduled, 1)
  MiniTest.expect.equality(chat.diff.buffer, nil)
  scheduled[1]()
  nvim.schedule = original_schedule

  MiniTest.expect.equality(nvim.api.nvim_buf_get_name(chat.diff.buffer), "louiselm-diff://" .. path)
  assert(chat.diff:accept())
  MiniTest.expect.equality(response, { outcome = { outcome = "selected", optionId = "allow-once" } })
  chat:dispose()
  nvim.fn.delete(path)
end

T["chat"]["schedules and resolves command and unknown permission requests"] = function()
  local first = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  local original_schedule = nvim.schedule
  local original_select = nvim.ui.select
  local scheduled = {}
  local selected = {}
  local responses = {}
  nvim.schedule = function(callback)
    scheduled[#scheduled + 1] = callback
  end

  first:emit({
    type = "permission_requested",
    session_id = "session-1",
    data = {
      operation = { kind = "command", command = { "git", "status" } },
      options = { { optionId = "allow-once", kind = "allow_once" }, { optionId = "deny", kind = "deny" } },
    },
    respond = function(result)
      responses[#responses + 1] = result
      return true
    end,
  })
  first:emit({
    type = "permission_requested",
    session_id = "session-1",
    data = { operation = { kind = "unknown" }, options = { "allow", "deny" } },
    respond = function(result)
      responses[#responses + 1] = result
      return true
    end,
  })

  MiniTest.expect.equality(#scheduled, 2)
  MiniTest.expect.equality(responses, {})
  nvim.ui.select = function(options, _, callback)
    selected[#selected + 1] = options
    callback(#selected == 1 and options[1] or nil)
  end
  scheduled[1]()
  scheduled[2]()
  nvim.schedule = original_schedule
  nvim.ui.select = original_select

  MiniTest.expect.equality(selected[1][1], { optionId = "allow-once", kind = "allow_once" })
  MiniTest.expect.equality(selected[2], { "allow", "deny" })
  MiniTest.expect.equality(responses, {
    { outcome = { outcome = "selected", optionId = "allow-once" } },
    { outcome = { outcome = "cancelled" } },
  })
  chat:dispose()
end

T["chat"]["ignores a queued permission choice after disposal"] = function()
  local first = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  local original_schedule = nvim.schedule
  local original_select = nvim.ui.select
  local scheduled = {}
  local choose
  local response
  nvim.schedule = function(callback)
    scheduled[#scheduled + 1] = callback
  end
  nvim.ui.select = function(_, _, callback)
    choose = callback
  end

  first:emit({
    type = "permission_requested",
    session_id = "session-1",
    data = { operation = { kind = "unknown" }, options = { "allow", "deny" } },
    respond = function(result)
      response = result
      return true
    end,
  })
  scheduled[1]()
  chat:dispose()
  choose("allow")

  nvim.schedule = original_schedule
  nvim.ui.select = original_select
  MiniTest.expect.equality(response, nil)
end

T["chat"]["queues context items as ACP text before the user prompt"] = function()
  local first = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  assert(chat:queue_context({ label = "file: init.lua", text = "Referenced file: init.lua" }))

  MiniTest.expect.equality(buffer_lines(chat:buffer()), {
    "# claude · session-1 · ready · Your turn",
    "",
    "> [context: file: init.lua] ",
  })
  assert(chat:submit("Review this"))

  MiniTest.expect.equality(first.prompts, {
    {
      { type = "text", text = "Referenced file: init.lua" },
      { type = "text", text = "Review this" },
    },
  })
  chat:dispose()
end

T["chat"]["renders state telemetry and reported-only usage"] = function()
  local first = fake_session("session-1", "claude")
  first.state.config_options = {
    { id = "model", name = "Model", category = "model", type = "select", current_value = "opus", options = {} },
    { id = "brave", name = "Brave", type = "boolean", current_value = true },
  }
  first.state.context = { used = 95, size = 100, percentage = 95, pressure = "critical", stale = false }
  first.state.cost = { amount = 1.5, currency = "USD" }
  local original_select = nvim.ui.select
  nvim.ui.select = function(_, _, callback)
    callback(nil)
  end
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  first.state.usage = { input_tokens = 12, cached_read_tokens = 3 }
  first:emit({ type = "turn_done", session_id = "session-1", data = { stopReason = "end_turn" } })
  nvim.wait(100, function()
    return #buffer_lines(chat:buffer()) == 4
  end, 1)
  nvim.ui.select = original_select

  MiniTest.expect.equality(buffer_lines(chat:buffer()), {
    "# claude · session-1 · ready · Model=opus · Brave=true · context=95/100 (95% critical) · cost=1.5 USD · Your turn",
    "",
    "[usage] input_tokens=12 · cached_read_tokens=3",
    "> ",
  })
  chat:dispose()
end

T["chat"]["shows whose turn it is in the session header"] = function()
  local first = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))

  MiniTest.expect.equality(buffer_lines(chat:buffer())[1], "# claude · session-1 · ready · Your turn")

  first.state.status = "prompting"
  first:emit({
    type = "state_changed",
    session_id = "session-1",
    data = { status = "prompting" },
  })
  nvim.wait(100, function()
    return buffer_lines(chat:buffer())[1] == "# claude · session-1 · prompting · Model responding"
  end, 1)

  MiniTest.expect.equality(buffer_lines(chat:buffer())[1], "# claude · session-1 · prompting · Model responding")
  chat:dispose()
end

T["chat"]["keeps the turn label visible in the window bar"] = function()
  local first = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))

  MiniTest.expect.equality(nvim.api.nvim_get_option_value("winbar", { win = 0 }), "Your turn")

  first.state.status = "prompting"
  first:emit({
    type = "state_changed",
    session_id = "session-1",
    data = { status = "prompting" },
  })
  nvim.wait(100, function()
    return nvim.api.nvim_get_option_value("winbar", { win = 0 }) == "Model responding"
  end, 1)

  MiniTest.expect.equality(nvim.api.nvim_get_option_value("winbar", { win = 0 }), "Model responding")
  chat:dispose()
end

T["chat"]["opens the setup overview and applies a selected option"] = function()
  local first = fake_session("session-1", "claude")
  first.state.config_options = {
    {
      id = "model",
      name = "Model",
      type = "select",
      current_value = "small",
      options = { { value = "small", name = "Small" }, { value = "large", name = "Large" } },
    },
    { id = "brave", name = "Brave", type = "boolean", current_value = false },
  }
  local original_select = nvim.ui.select
  local calls = {}
  nvim.ui.select = function(items, options, callback)
    calls[#calls + 1] = { items = items, options = options }
    if #calls == 1 then
      callback(items[1])
    elseif #calls == 2 then
      callback(items[2])
    else
      callback(nil)
    end
  end
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  nvim.wait(100, function()
    return #calls == 3
  end, 1)
  nvim.ui.select = original_select

  MiniTest.expect.equality(calls[1].options.prompt, "louiselm session options: ")
  MiniTest.expect.equality(calls[1].options.format_item(calls[1].items[1]), "Model: small")
  MiniTest.expect.equality(first.config_changes, { { id = "model", value = "large" } })
  chat:dispose()
end

T["chat"]["ignores a queued setup overview after disposal"] = function()
  local first = fake_session("session-1", "claude")
  first.state.config_options = {
    { id = "brave", name = "Brave", type = "boolean", current_value = false },
  }
  local original_schedule = nvim.schedule
  local original_select = nvim.ui.select
  local scheduled = {}
  local selects = 0
  nvim.schedule = function(callback)
    scheduled[#scheduled + 1] = callback
  end
  nvim.ui.select = function()
    selects = selects + 1
  end
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  chat:dispose()
  scheduled[1]()
  nvim.schedule = original_schedule
  nvim.ui.select = original_select

  MiniTest.expect.equality(selects, 0)
end

T["chat"]["switches with telemetry rows and closes only the selected session"] = function()
  local first = fake_session("session-1", "one")
  local second = fake_session("session-2", "two")
  second.state.status = "prompting"
  local api = fake_api()
  api.list_sessions = function()
    return { "session-1", "session-2" }
  end
  api.get_session = function(_, id)
    return id == "session-1" and first or second
  end
  local chat = assert(Chat.new(api))
  assert(chat:attach(first))
  assert(chat:attach(second))
  local original_select = nvim.ui.select
  local prompts = {}
  nvim.ui.select = function(items, options, callback)
    prompts[#prompts + 1] = options
    callback(items[1])
  end

  assert(chat:switch_session())
  MiniTest.expect.equality(chat.current_id, "session-1")
  assert(chat:switch("session-2"))
  assert(chat:close_session())
  nvim.ui.select = original_select

  MiniTest.expect.equality(prompts[1].prompt, "louiselm session: ")
  MiniTest.expect.equality(prompts[1].format_item(first), "one · session-1 · ready")
  MiniTest.expect.equality(prompts[2].prompt, "close active louiselm session? ")
  MiniTest.expect.equality(second.disposed, true)
  MiniTest.expect.equality(first.disposed, false)
  MiniTest.expect.equality(chat:buffer("session-2"), nil)
  chat:dispose()
end

T["chat"]["renames the current session and refreshes its header"] = function()
  local first = fake_session("session-1", "claude")
  first.state.name = "First"
  function first:set_name(name)
    self.state.name = name
    self:emit({ type = "state_changed", session_id = self.state.id, data = { status = self.state.status } })
    return true
  end
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))

  assert(chat:rename_session("Review"))
  nvim.wait(100, function()
    return nvim.api.nvim_buf_get_lines(chat:buffer(), 0, 1, false)[1]:find("# Review", 1, true) ~= nil
  end, 1)
  MiniTest.expect.equality(
    nvim.api.nvim_buf_get_lines(chat:buffer(), 0, 1, false)[1],
    "# Review · claude · session-1 · ready · Your turn"
  )
  chat:dispose()
end

T["chat"]["mentions the source buffer rather than the chat scratch buffer"] = function()
  local source = nvim.api.nvim_create_buf(false, true)
  nvim.api.nvim_buf_set_name(source, "/tmp/source.lua")
  nvim.api.nvim_set_current_buf(source)
  local first = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))

  assert(chat:mention_buffer())
  assert(chat:submit("Review"))

  MiniTest.expect.equality(first.prompts, {
    {
      { type = "text", text = "Current buffer: /tmp/source.lua" },
      { type = "text", text = "Review" },
    },
  })
  chat:dispose()
  nvim.api.nvim_buf_delete(source, { force = true })
end

T["chat"]["switches between attached session buffers"] = function()
  local first = fake_session("session-1", "one")
  local second = fake_session("session-2", "two")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  assert(chat:attach(second))

  assert(chat:switch("session-1"))
  MiniTest.expect.equality(nvim.api.nvim_get_current_buf(), chat:buffer("session-1"))
  assert(chat:switch("session-2"))
  MiniTest.expect.equality(nvim.api.nvim_get_current_buf(), chat:buffer("session-2"))

  chat:dispose()
end

T["chat"]["uses the agent picker for a new session"] = function()
  local created
  local session = fake_session("session-1", "two")
  local api = {
    create_session = function(_, agent_name)
      created = agent_name
      return session
    end,
  }
  local chat = assert(Chat.new(api, { agents = { "one", "two" } }))
  local original_select = nvim.ui.select
  nvim.ui.select = function(items, _, callback)
    MiniTest.expect.equality(items, { "one", "two" })
    callback("two")
  end

  local selected = chat:new_session()

  nvim.ui.select = original_select
  MiniTest.expect.equality(selected, nil)
  MiniTest.expect.equality(created, "two")
  MiniTest.expect.equality(chat:buffer("session-1") ~= nil, true)
  chat:dispose()
end

return T
