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
    return #self.prompts
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
  nvim.api.nvim_buf_set_mark(buffer, "<", 1, 7, {})
  nvim.api.nvim_buf_set_mark(buffer, ">", 1, 9, {})
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

return T
