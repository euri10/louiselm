local MiniTest = require("mini.test")
local Chat = require("louiselm.ui.chat")

local T = MiniTest.new_set()

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local function fake_session(id, agent)
  local listeners = {}
  local session = {
    state = { id = id, agent = agent },
    prompts = {},
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
    return #buffer_lines(chat:buffer()) == 7
  end, 1)

  MiniTest.expect.equality(first.prompts, { "/compact" })
  MiniTest.expect.equality(buffer_lines(chat:buffer()), {
    "# claude · session-1",
    "",
    "> /compact",
    "hello **world**",
    "[tool started] tool-1: Read file",
    "[tool finished] tool-1 (completed)",
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
  MiniTest.expect.equality(buffer_lines(chat:buffer()), { "# claude · session-1", "", "> hello", "", "> " })
  scheduled[1]()
  nvim.schedule = original_schedule

  MiniTest.expect.equality(buffer_lines(chat:buffer()), {
    "# claude · session-1",
    "",
    "> hello",
    "scheduled",
    "> ",
  })
  chat:dispose()
end

T["chat"]["queues context items as ACP text before the user prompt"] = function()
  local first = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  assert(chat:queue_context({ label = "file: init.lua", text = "Referenced file: init.lua" }))

  MiniTest.expect.equality(buffer_lines(chat:buffer()), {
    "# claude · session-1",
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
