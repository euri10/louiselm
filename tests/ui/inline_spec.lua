local MiniTest = require("mini.test")
local Inline = require("louiselm.ui.inline")

local T = MiniTest.new_set()

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local function fake_session()
  local listeners = {}
  local session = { state = { id = "inline-1", agent = "claude", status = "ready" }, prompts = {}, disposed = false }

  function session:inspect()
    return self.state
  end

  function session:on(callback)
    listeners[#listeners + 1] = callback
    return function() end
  end

  function session:prompt(prompt)
    self.prompts[#self.prompts + 1] = prompt
    self.state.status = "prompting"
    return #self.prompts
  end

  function session:cancel()
    self.cancelled = true
    self.state.status = "cancelling"
    return true
  end

  function session:dispose()
    self.disposed = true
    return true
  end

  function session:emit(event)
    for _, listener in ipairs(listeners) do
      listener(event)
    end
  end

  return session
end

local function fake_api(session)
  return {
    create_session = function(_, _, _, ready_callback)
      if ready_callback ~= nil then
        ready_callback(session)
      end
      return session
    end,
  }
end

local function new_buffer(lines)
  local buffer = nvim.api.nvim_create_buf(false, true)
  nvim.api.nvim_buf_set_lines(buffer, 0, -1, false, lines)
  nvim.api.nvim_set_current_buf(buffer)
  return buffer
end

T["inline"] = MiniTest.new_set({
  hooks = {
    post_case = function()
      for _, buffer in ipairs(nvim.api.nvim_list_bufs()) do
        if nvim.api.nvim_buf_is_valid(buffer) and nvim.api.nvim_buf_get_name(buffer) == "" then
          nvim.api.nvim_buf_delete(buffer, { force = true })
        end
      end
    end,
  },
})

T["inline"]["replaces a visual selection with streamed output"] = function()
  local buffer = new_buffer({ "before old after" })
  nvim.api.nvim_feedkeys("gg0wve" .. nvim.keycode("<Esc>"), "nx!", false)
  local session = fake_session()
  local inline = assert(Inline.new(fake_api(session), { agents = { "claude" } }))

  assert(inline:run("rewrite"))
  nvim.wait(100, function()
    return #session.prompts == 1
  end, 1)
  MiniTest.expect.equality(session.prompts[1][1].text, "File: [No Name]\nSelected text:\nold")
  session:emit({ type = "chunk", session_id = "inline-1", data = { text = "new" } })
  session:emit({ type = "chunk", session_id = "inline-1", data = { text = " value" } })
  nvim.wait(100, function()
    return nvim.api.nvim_buf_get_lines(buffer, 0, -1, false)[1] == "before new value after"
  end, 1)
  MiniTest.expect.equality(nvim.api.nvim_buf_get_lines(buffer, 0, -1, false), { "before new value after" })
  inline:dispose()
end

T["inline"]["normalizes native Visual selections"] = MiniTest.new_set({
  parametrize = {
    {
      { "keep before", "first old", "second old", "keep after" },
      "ggjVj",
      "inclusive",
      "first old\nsecond old",
      { "keep before", "new", "value", "keep after" },
    },
    { { "old", "" }, "ggVj", "inclusive", "old\n", { "new", "value" } },
    { { "aéz!" }, "gg0lv", "inclusive", "é", { "anew", "valuez!" } },
    { { "aéz!" }, "gg0lvl", "inclusive", "éz", { "anew", "value!" } },
    { { "aéz!" }, "gg0lvl", "exclusive", "é", { "anew", "valuez!" } },
    { { "aéz!" }, "gg0llvh", "exclusive", "é", { "anew", "valuez!" } },
  },
})

T["inline"]["normalizes native Visual selections"]["streams into exactly the selected range"] = function(
  lines,
  keys,
  selection,
  context,
  expected
)
  local previous_selection = nvim.o.selection
  MiniTest.finally(function()
    nvim.o.selection = previous_selection
  end)
  nvim.o.selection = selection
  local buffer = new_buffer(lines)
  nvim.api.nvim_feedkeys(keys .. nvim.keycode("<Esc>"), "nx!", false)
  local session = fake_session()
  local inline = assert(Inline.new(fake_api(session), { agents = { "claude" } }))
  MiniTest.finally(function()
    inline:dispose()
  end)

  assert(inline:run("rewrite"))
  assert(nvim.wait(100, function()
    return #session.prompts == 1
  end, 1))
  MiniTest.expect.equality(session.prompts[1][1].text, "File: [No Name]\nSelected text:\n" .. context)
  session:emit({ type = "chunk", session_id = "inline-1", data = { text = "new" } })
  session:emit({ type = "chunk", session_id = "inline-1", data = { text = "\nvalue" } })
  assert(nvim.wait(100, function()
    return nvim.deep_equal(nvim.api.nvim_buf_get_lines(buffer, 0, -1, false), expected)
  end, 1))
  MiniTest.expect.equality(nvim.api.nvim_buf_get_lines(buffer, 0, -1, false), expected)
end

T["inline"]["refuses a blockwise selection before starting a Session"] = function()
  local lines = { "abc first", "def second" }
  local buffer = new_buffer(lines)
  nvim.api.nvim_feedkeys("gg0" .. nvim.keycode("<C-v>") .. "jl" .. nvim.keycode("<Esc>"), "nx!", false)
  local session = fake_session()
  local inline = assert(Inline.new(fake_api(session), { agents = { "claude" } }))
  MiniTest.finally(function()
    inline:dispose()
  end)

  MiniTest.expect.equality({ inline:run("rewrite") }, {
    nil,
    "inline does not support blockwise selections; use a characterwise or linewise selection",
  })
  MiniTest.expect.equality(inline.session, nil)
  MiniTest.expect.equality(session.prompts, {})
  MiniTest.expect.equality(nvim.api.nvim_buf_get_lines(buffer, 0, -1, false), lines)
end

T["inline"]["inserts at the cursor when no selection exists"] = function()
  local buffer = new_buffer({ "hello" })
  nvim.api.nvim_win_set_cursor(0, { 1, 5 })
  local session = fake_session()
  local inline = assert(Inline.new(fake_api(session), { agents = { "claude" } }))

  assert(inline:run("complete"))
  nvim.wait(100, function()
    return #session.prompts == 1
  end, 1)
  session:emit({ type = "chunk", session_id = "inline-1", data = { content = { text = " world" } } })
  nvim.wait(100, function()
    return nvim.api.nvim_buf_get_lines(buffer, 0, -1, false)[1] == "hell worldo"
  end, 1)
  MiniTest.expect.equality(nvim.api.nvim_buf_get_lines(buffer, 0, -1, false), { "hell worldo" })
  inline:dispose()
end

T["inline"]["cancels before scheduled submission and does not send the prompt"] = function()
  new_buffer({ "old" })
  local session = fake_session()
  local inline = assert(Inline.new(fake_api(session), { agents = { "claude" } }))
  MiniTest.finally(function()
    inline:dispose()
  end)
  assert(inline:run("rewrite"))
  assert(inline:cancel())
  local drained = false
  nvim.schedule(function()
    drained = true
  end)
  assert(nvim.wait(100, function()
    return drained
  end, 1))
  MiniTest.expect.equality(session.prompts, {})
  MiniTest.expect.equality(session.disposed, true)
  MiniTest.expect.equality(inline.running, false)
end

T["inline"]["cancellation drops late chunks and permits another invocation after completion"] = function()
  local buffer = new_buffer({ "old" })
  local sessions = {}
  local api = {
    create_session = function(_, _, _, callback)
      local session = fake_session()
      sessions[#sessions + 1] = session
      nvim.schedule(function()
        callback(session)
      end)
      return session
    end,
  }
  local inline = assert(Inline.new(api, { agents = { "claude" } }))
  MiniTest.finally(function()
    inline:dispose()
  end)
  assert(inline:run("first"))
  assert(nvim.wait(100, function()
    return #sessions[1].prompts == 1
  end, 1))
  sessions[1]:emit({ type = "chunk", session_id = "inline-1", data = { text = "new" } })
  assert(nvim.wait(100, function()
    return nvim.api.nvim_buf_get_lines(buffer, 0, -1, false)[1] == "newold"
  end, 1))
  assert(inline:cancel())
  sessions[1]:emit({ type = "chunk", session_id = "inline-1", data = { text = " discarded" } })
  sessions[1]:emit({ type = "turn_done", session_id = "inline-1", data = {} })
  assert(nvim.wait(100, function()
    return not inline.running
  end, 1))
  MiniTest.expect.equality(nvim.api.nvim_buf_get_lines(buffer, 0, -1, false), { "newold" })
  nvim.api.nvim_win_set_cursor(0, { 1, 0 })
  assert(inline:run("second"))
  assert(nvim.wait(100, function()
    return #sessions[2].prompts == 1
  end, 1))
  MiniTest.expect.equality(sessions[1].disposed, true)
  sessions[1]:emit({ type = "chunk", session_id = "inline-1", data = { text = "stale Session" } })
  sessions[2]:emit({ type = "chunk", session_id = "inline-1", data = { text = "next" } })
  assert(nvim.wait(100, function()
    return nvim.api.nvim_buf_get_lines(buffer, 0, -1, false)[1] == "nextnewold"
  end, 1))
end

T["inline"]["disposal cancels queued permissions and cannot revive the source buffer"] = function()
  local buffer = new_buffer({ "old" })
  local session = fake_session()
  local inline = assert(Inline.new(fake_api(session), { agents = { "claude" } }))
  MiniTest.finally(function()
    inline:dispose()
  end)
  assert(inline:run("rewrite"))
  local outcomes = {}
  session:emit({
    type = "permission_requested",
    session_id = "inline-1",
    data = {},
    respond = function(result)
      outcomes[#outcomes + 1] = result.outcome
      return true
    end,
  })
  session:emit({ type = "chunk", session_id = "inline-1", data = { text = "late" } })
  inline:dispose()
  assert(nvim.wait(100, function()
    return #outcomes == 1
  end, 1))
  MiniTest.expect.equality(outcomes, { { outcome = "cancelled" } })
  MiniTest.expect.equality(nvim.api.nvim_buf_get_lines(buffer, 0, -1, false), { "old" })
end

T["inline"]["rejects restored marks without a Visual mode before creating a Session"] = function()
  local buffer = new_buffer({ "old" })
  nvim.fn.visualmode(1)
  nvim.api.nvim_buf_set_mark(buffer, "<", 1, 0, {})
  nvim.api.nvim_buf_set_mark(buffer, ">", 1, 1, {})
  local inline = assert(Inline.new(fake_api(fake_session()), { agents = { "claude" } }))
  MiniTest.finally(function()
    inline:dispose()
  end)
  MiniTest.expect.equality({ inline:run("rewrite") }, {
    nil,
    "inline selection mode is unavailable; select the text again",
  })
  MiniTest.expect.equality(inline.session, nil)
end

T["inline"]["late startup failures"] = MiniTest.new_set({ parametrize = { { true }, { false } } })
T["inline"]["late startup failures"]["cannot change a replacement or disposed controller"] = function(replace)
  new_buffer({ "old" })
  local sessions, callbacks, messages = {}, {}, {}
  local original_notify = nvim.notify
  rawset(nvim, "notify", function(message)
    messages[#messages + 1] = message
  end)
  local api = {
    create_session = function(_, _, _, callback)
      local session = fake_session()
      session.state.status = "starting"
      sessions[#sessions + 1] = session
      callbacks[#callbacks + 1] = callback
      return session
    end,
  }
  local inline = assert(Inline.new(api, { agents = { "claude" } }))
  MiniTest.finally(function()
    inline:dispose()
    rawset(nvim, "notify", original_notify)
  end)
  assert(inline:run("first"))
  if replace then
    assert(inline:cancel())
    assert(inline:run("replacement"))
  else
    inline:dispose()
  end
  callbacks[1](nil, "late startup failure")
  local drained = false
  nvim.schedule(function()
    drained = true
  end)
  assert(nvim.wait(100, function()
    return drained
  end, 1))
  MiniTest.expect.equality(messages, {})
  if replace then
    MiniTest.expect.equality(inline.running, true)
    callbacks[2](sessions[2])
    assert(nvim.wait(100, function()
      return #sessions[2].prompts == 1
    end, 1))
  end
end

return T
