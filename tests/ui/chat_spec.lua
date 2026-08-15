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

local function virtual_text(buffer)
  local marks = nvim.api.nvim_buf_get_extmarks(buffer, -1, 0, -1, { details = true })
  local text = {}
  for _, mark in ipairs(marks) do
    for _, chunk in ipairs(mark[4].virt_text or {}) do
      text[#text + 1] = chunk[1]
    end
  end
  return text
end

T["chat"] = MiniTest.new_set({
  hooks = {
    post_case = function()
      nvim.cmd.normal({ args = { "<Esc>" }, bang = true })
      for _, buffer in ipairs(nvim.api.nvim_list_bufs()) do
        if nvim.api.nvim_buf_is_valid(buffer) and nvim.api.nvim_buf_get_name(buffer):match("^louiselm://") then
          nvim.api.nvim_buf_delete(buffer, { force = true })
        end
      end
    end,
  },
})

T["chat"]["focuses the prompt"] = function()
  local first = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))

  MiniTest.expect.equality(nvim.api.nvim_win_get_cursor(0), { 3, 1 })

  chat:dispose()
end

T["chat"]["submits every line in a multiline prompt"] = function()
  local first = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))

  nvim.api.nvim_buf_set_lines(chat:buffer(), 2, -1, false, { "> first line", "> second line" })
  assert(chat:submit())

  MiniTest.expect.equality(first.prompts, { "first line\nsecond line" })
  MiniTest.expect.equality(buffer_lines(chat:buffer()), {
    "# claude · session-1 · ready · Your turn",
    "",
    "> first line",
    "> second line",
    "",
    "> ",
  })
  chat:dispose()
end

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

T["chat"]["shows skill status and keeps slash prompts when skills are off"] = function()
  local first = fake_session("session-1", "claude")
  first.state.skills_policy = "off"
  local chat = assert(Chat.new(fake_api(), {
    skills = {
      { name = "grill-me", description = "Stress test", path = "/skills/grill-me/SKILL.md", content = "skill" },
    },
  }))
  assert(chat:attach(first))

  assert(chat:submit("/compact"))
  local picked, pick_error = chat:pick_skill()

  MiniTest.expect.equality(first.prompts, { "/compact" })
  MiniTest.expect.equality(buffer_lines(chat:buffer())[1], "# claude · session-1 · ready · skills: off · Your turn")
  MiniTest.expect.equality(picked, false)
  MiniTest.expect.equality(pick_error, "skill picker is disabled for this session")
  chat:dispose()
end

T["chat"]["reports an empty picker catalog without blocking a native session"] = function()
  local first = fake_session("session-1", "codex")
  first.state.skills_policy = "native"
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))

  local picked, pick_error = chat:pick_skill()

  MiniTest.expect.equality(picked, false)
  MiniTest.expect.equality(pick_error, "no chat skills configured")
  MiniTest.expect.equality(buffer_lines(chat:buffer())[1], "# codex · session-1 · ready · skills: on · Your turn")
  chat:dispose()
end

T["chat"]["normalizes multiline tool activity in the session header"] = function()
  local first = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))

  first.state.status = "prompting"
  first.state.activity = "exec command\nwith another line"
  first:emit({ type = "state_changed", session_id = "session-1", data = { status = "prompting" } })
  nvim.wait(100, function()
    return buffer_lines(chat:buffer())[1] ~= "# claude · session-1 · ready · Your turn"
  end, 1)

  MiniTest.expect.equality(
    buffer_lines(chat:buffer())[1],
    "# claude · session-1 · prompting · activity=exec command with another line · Model responding"
  )
  chat:dispose()
end

T["chat"]["queues one prompt in every active turn state and releases it only on turn completion"] = function()
  for _, status in ipairs({ "prompting", "waiting_permission", "cancelling" }) do
    local first = fake_session("session-" .. status, "claude")
    first.state.status = status
    local chat = assert(Chat.new(fake_api()))
    assert(chat:attach(first))

    assert(chat:submit("/compact"))
    MiniTest.expect.equality(first.prompts, {})
    MiniTest.expect.equality(buffer_lines(chat:buffer()), {
      "# claude · session-"
        .. status
        .. " · "
        .. status
        .. " · "
        .. (
          status == "prompting" and "Model responding"
          or status == "waiting_permission" and "Waiting for permission"
          or "Stopping"
        ),
      "",
      "> /compact",
    })
    MiniTest.expect.equality(virtual_text(chat:buffer()), { "Queued for next turn" })

    first.state.status = "ready"
    first:emit({ type = "turn_done", session_id = first.state.id, data = { stopReason = "end_turn" } })
    nvim.wait(100, function()
      return #first.prompts == 1
    end, 1)

    MiniTest.expect.equality(first.prompts, { "/compact" })
    MiniTest.expect.equality(buffer_lines(chat:buffer()), {
      buffer_lines(chat:buffer())[1],
      "",
      "> /compact",
      "",
      "> ",
    })
    MiniTest.expect.equality(virtual_text(chat:buffer()), {})
    chat:dispose()
  end
end

T["chat"]["keeps an edited queued prompt as a draft until Enter recommits it"] = function()
  local first = fake_session("session-1", "claude")
  first.state.status = "prompting"
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  assert(chat:submit("original"))

  nvim.api.nvim_buf_set_lines(chat:buffer(), 2, 3, false, { "> revised" })
  first.state.status = "ready"
  first:emit({ type = "turn_done", session_id = "session-1", data = {} })
  nvim.wait(20)

  MiniTest.expect.equality(first.prompts, {})
  MiniTest.expect.equality(virtual_text(chat:buffer()), {})
  assert(chat:submit())
  MiniTest.expect.equality(first.prompts, { "revised" })
  chat:dispose()
end

T["chat"]["snapshots queued context and keeps it isolated with its session"] = function()
  local first = fake_session("session-1", "one")
  local second = fake_session("session-2", "two")
  first.state.status = "prompting"
  second.state.status = "prompting"
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  local item = { label = "file", text = "original context" }
  assert(chat:queue_context(item))
  assert(chat:submit("first prompt"))
  item.text = "changed context"

  assert(chat:attach(second))
  assert(chat:submit("second prompt"))
  assert(chat:switch("session-1"))
  first.state.status = "ready"
  first:emit({ type = "turn_done", session_id = "session-1", data = {} })
  nvim.wait(100, function()
    return #first.prompts == 1
  end, 1)

  MiniTest.expect.equality(first.prompts, {
    {
      { type = "text", text = "original context" },
      { type = "text", text = "first prompt" },
    },
  })
  MiniTest.expect.equality(second.prompts, {})
  MiniTest.expect.equality(virtual_text(chat:buffer("session-2")), { "Queued for next turn" })
  chat:dispose()
end

T["chat"]["preserves rejected and failed prompts outside transcript history"] = function()
  local original_notify = nvim.notify
  local notifications = {}
  rawset(nvim, "notify", function(message)
    notifications[#notifications + 1] = message
  end)

  local starting = fake_session("starting", "claude")
  starting.state.status = "starting"
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(starting))
  local request_id, start_error = chat:submit("draft")
  MiniTest.expect.equality({ request_id, start_error }, { nil, "session is not ready" })
  MiniTest.expect.equality(buffer_lines(chat:buffer()), {
    "# claude · starting · starting · Starting",
    "",
    "> draft",
  })

  starting.state.status = "ready"
  function starting:prompt()
    return nil, "write failed"
  end
  local failed_id, failed_error = chat:submit()
  rawset(nvim, "notify", original_notify)

  MiniTest.expect.equality({ failed_id, failed_error }, { nil, "write failed" })
  MiniTest.expect.equality(buffer_lines(chat:buffer()), {
    "# claude · starting · starting · Starting",
    "",
    "> draft",
  })
  MiniTest.expect.equality(notifications, { "louiselm: session is not ready", "louiselm: write failed" })
  chat:dispose()
end

T["chat"]["does not release queued work after session error or chat disposal"] = function()
  local failed = fake_session("failed", "claude")
  failed.state.status = "prompting"
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(failed))
  assert(chat:submit("keep me"))
  failed.state.status = "error"
  failed:emit({ type = "error", session_id = "failed", data = { message = "agent failed" } })
  nvim.wait(100, function()
    return virtual_text(chat:buffer())[1] == nil
  end, 1)
  MiniTest.expect.equality(failed.prompts, {})
  MiniTest.expect.equality(buffer_lines(chat:buffer())[4], "> keep me")

  local late = fake_session("late", "claude")
  late.state.status = "prompting"
  assert(chat:attach(late))
  assert(chat:submit("never send"))
  local original_schedule = nvim.schedule
  local scheduled = {}
  nvim.schedule = function(callback)
    scheduled[#scheduled + 1] = callback
  end
  late.state.status = "ready"
  late:emit({ type = "turn_done", session_id = "late", data = {} })
  chat:dispose()
  scheduled[1]()
  nvim.schedule = original_schedule

  MiniTest.expect.equality(late.prompts, {})
end

T["chat"]["warns that closing an active session discards its queued prompt"] = function()
  local first = fake_session("session-1", "claude")
  first.state.status = "prompting"
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  assert(chat:submit("discard me"))
  local original_select = nvim.ui.select
  local close_prompt
  nvim.ui.select = function(_, options, callback)
    close_prompt = options.prompt
    callback("Close")
  end

  assert(chat:close_session())
  nvim.ui.select = original_select

  MiniTest.expect.equality(close_prompt, "close active louiselm session and discard queued prompt? ")
  MiniTest.expect.equality(first.disposed, true)
  MiniTest.expect.equality(first.prompts, {})
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

T["chat"]["renders replayed assistant chunks before a new prompt"] = function()
  local restored = fake_session("session-1", "codex")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(restored))
  local original_schedule = nvim.schedule
  local scheduled = {}
  nvim.schedule = function(callback)
    scheduled[#scheduled + 1] = callback
  end

  restored:emit({
    type = "chunk",
    session_id = "session-1",
    data = { content = { type = "text", text = "replayed" } },
  })

  scheduled[1]()
  nvim.schedule = original_schedule

  MiniTest.expect.equality(buffer_lines(chat:buffer()), {
    "# codex · session-1 · ready · Your turn",
    "",
    "replayed",
    "> ",
  })
  chat:dispose()
end

T["chat"]["discovers and resumes into a separate scheduled chat view"] = function()
  local first = fake_session("session-1", "claude")
  local restored = fake_session("session-2", "codex")
  restored.state.status = "starting"
  local api = fake_api()
  local discovery_options
  local discovery_callback
  local load_call
  function api:discover_sessions(options, callback)
    discovery_options = options
    discovery_callback = callback
    return true
  end
  function api:load_session(agent, session_id, options, ready_callback)
    load_call = { agent = agent, session_id = session_id, options = options, ready_callback = ready_callback }
    return restored
  end
  function api:list_sessions()
    return { "session-1", "session-2" }
  end

  local chat = assert(Chat.new(api))
  assert(chat:attach(first))
  local first_buffer = chat:buffer("session-1")
  local original_schedule = nvim.schedule
  local original_select = nvim.ui.select
  local scheduled = {}
  local formatted
  nvim.schedule = function(callback)
    scheduled[#scheduled + 1] = callback
  end
  nvim.ui.select = function(items, options, callback)
    formatted = options.format_item(items[1])
    callback(items[1])
  end

  assert(chat:resume_session())
  MiniTest.expect.equality(discovery_options, { cwd = nvim.fn.getcwd() })
  discovery_callback({
    {
      agent = "codex",
      session_id = "prior-acp",
      cwd = "/tmp/project",
      title = "Previous work",
      updated_at = "2026-08-10T10:00:00Z",
    },
  }, {})
  MiniTest.expect.equality(load_call, nil)
  scheduled[1]()

  MiniTest.expect.equality(
    formatted,
    "codex/prior-acp · Previous work · cwd=/tmp/project · updated=2026-08-10T10:00:00Z"
  )
  MiniTest.expect.equality(load_call.agent, "codex")
  MiniTest.expect.equality(load_call.session_id, "prior-acp")
  MiniTest.expect.equality(load_call.options, { cwd = "/tmp/project", name = "Previous work" })
  MiniTest.expect.equality(nvim.api.nvim_buf_is_valid(first_buffer), true)
  MiniTest.expect.equality(first.disposed, false)
  MiniTest.expect.equality(chat:buffer(), chat:buffer("session-2"))

  restored:emit({
    type = "chunk",
    session_id = "session-2",
    data = { content = { type = "text", text = "replayed history" } },
  })
  restored.state.status = "ready"
  restored:emit({ type = "state_changed", session_id = "session-2", data = { status = "ready" } })
  scheduled[2]()
  scheduled[3]()
  load_call.ready_callback(restored)

  MiniTest.expect.equality(buffer_lines(chat:buffer("session-2")), {
    "# codex · session-2 · ready · Your turn",
    "",
    "replayed history",
    "> ",
  })

  nvim.schedule = original_schedule
  nvim.ui.select = original_select
  chat:dispose()
end

T["chat"]["reports stable ACP identities for new and resumed sessions"] = function()
  local created = fake_session("session-1", "claude")
  created.state.acp_session_id = "created-acp"
  local loaded = fake_session("session-2", "codex")
  loaded.state.acp_session_id = "loaded-acp"
  loaded.state.source = "loaded"
  local chat = assert(Chat.new(fake_api()))

  local report_id, missing_error = chat:session_id()
  MiniTest.expect.equality(report_id, nil)
  MiniTest.expect.equality(missing_error, "no chat session is open")

  assert(chat:attach(created))
  MiniTest.expect.equality(chat:session_id(), "claude/created-acp")
  assert(chat:attach(loaded))
  MiniTest.expect.equality(chat:session_id(), "codex/loaded-acp")
  chat:dispose()
end

T["chat"]["ignores scheduled discovery after disposal and supports all workspaces"] = function()
  local api = fake_api()
  local discovery_options
  local discovery_callback
  function api:discover_sessions(options, callback)
    discovery_options = options
    discovery_callback = callback
    return true
  end
  local chat = assert(Chat.new(api))
  local original_schedule = nvim.schedule
  local original_select = nvim.ui.select
  local scheduled = {}
  local selected = false
  nvim.schedule = function(callback)
    scheduled[#scheduled + 1] = callback
  end
  nvim.ui.select = function()
    selected = true
  end

  assert(chat:resume_session(true))
  MiniTest.expect.equality(discovery_options, {})
  discovery_callback({ { agent = "codex", session_id = "prior", cwd = "/tmp/project" } }, {})
  chat:dispose()
  scheduled[1]()

  nvim.schedule = original_schedule
  nvim.ui.select = original_select
  MiniTest.expect.equality(selected, false)
end

T["chat"]["reports a selected load failure without creating a replacement"] = function()
  local api = fake_api()
  local created = 0
  function api:create_session()
    created = created + 1
    return fake_session("replacement", "codex")
  end
  function api:discover_sessions(_, callback)
    callback({ { agent = "codex", session_id = "stale", cwd = "/tmp/project" } }, {})
    return true
  end
  function api:load_session()
    return nil, "ACP session/load failed: unknown session"
  end
  local original_select = nvim.ui.select
  local original_notify = nvim.notify
  local notifications = {}
  nvim.ui.select = function(items, _, callback)
    callback(items[1])
  end
  rawset(nvim, "notify", function(message)
    notifications[#notifications + 1] = message
  end)

  local chat = assert(Chat.new(api))
  assert(chat:resume_session())
  nvim.wait(100, function()
    return #notifications > 0
  end, 1)

  nvim.ui.select = original_select
  rawset(nvim, "notify", original_notify)
  MiniTest.expect.equality(notifications, { "louiselm: ACP session/load failed: unknown session" })
  MiniTest.expect.equality(created, 0)
  MiniTest.expect.equality(chat:buffer(), nil)
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

T["chat"]["preserves distinct permission option names with the same kind"] = function()
  local first = fake_session("session-1", "codex")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  local original_schedule = nvim.schedule
  local original_select = nvim.ui.select
  local scheduled = {}
  local labels
  local response
  nvim.schedule = function(callback)
    scheduled[#scheduled + 1] = callback
  end

  first:emit({
    type = "permission_requested",
    session_id = "session-1",
    data = {
      operation = { kind = "command" },
      options = {
        { optionId = "session", name = "Allow for This Session", kind = "allow_always" },
        { optionId = "always", name = "Allow and Don't Ask Again", kind = "allow_always" },
      },
    },
    respond = function(result)
      response = result
      return true
    end,
  })

  nvim.ui.select = function(options, select_options, callback)
    labels = {}
    for _, option in ipairs(options) do
      labels[#labels + 1] = select_options.format_item(option)
    end
    callback(options[2])
  end
  scheduled[1]()
  nvim.schedule = original_schedule
  nvim.ui.select = original_select

  MiniTest.expect.equality(labels, { "Allow for This Session", "Allow and Don't Ask Again" })
  MiniTest.expect.equality(response, { outcome = { outcome = "selected", optionId = "always" } })
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
  first.state.acp_session_id = "one-acp"
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
  MiniTest.expect.equality(prompts[1].format_item(first), "one/one-acp · session-1 · ready")
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
