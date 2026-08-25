local MiniTest = require("mini.test")
local Protocol = require("louiselm.acp.protocol")
local LogView = require("louiselm.forensics.log_view")

local T = MiniTest.new_set()

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

---@param message table
---@return string
local function line(message)
  return assert(Protocol.encode(message))
end

T["summarize"] = MiniTest.new_set()

T["summarize"]["renders a request with its id and method"] = function()
  local request = assert(Protocol.request(3, "session/prompt", { sessionId = "s1" }))
  MiniTest.expect.equality(LogView.summarize(request), '→ #3 session/prompt {"sessionId":"s1"}')
end

T["summarize"]["renders a successful response"] = function()
  local response = Protocol.response(3, { ok = true })
  MiniTest.expect.equality(LogView.summarize(response), '← #3 {"ok":true}')
end

T["summarize"]["renders an error response"] = function()
  local response = Protocol.error_response(3, -32000, "boom")
  MiniTest.expect.equality(LogView.summarize(response), "← #3 ERROR -32000 boom")
end

T["summarize"]["renders an agent message chunk"] = function()
  local notification = assert(Protocol.notification("session/update", {
    sessionId = "s1",
    update = { sessionUpdate = "agent_message_chunk", content = { type = "text", text = "hello" } },
  }))
  MiniTest.expect.equality(LogView.summarize(notification), "» hello")
end

T["summarize"]["renders a tool call with its title and status"] = function()
  local notification = assert(Protocol.notification("session/update", {
    sessionId = "s1",
    update = { sessionUpdate = "tool_call", toolCallId = "t1", title = "ls -la", status = "in_progress" },
  }))
  MiniTest.expect.equality(LogView.summarize(notification), "⚙ ls -la [in_progress]")
end

T["summarize"]["collapses multi-line chunk text onto one line"] = function()
  local notification = assert(Protocol.notification("session/update", {
    sessionId = "s1",
    update = { sessionUpdate = "agent_thought_chunk", content = { type = "text", text = "line one\nline two" } },
  }))
  MiniTest.expect.equality(LogView.summarize(notification), "… line one line two")
end

T["summarize"]["truncates long payloads to a bounded width"] = function()
  local notification = assert(Protocol.notification("session/update", {
    sessionId = "s1",
    update = { sessionUpdate = "agent_message_chunk", content = { type = "text", text = string.rep("x", 500) } },
  }))
  local rendered = LogView.summarize(notification)
  MiniTest.expect.equality(nvim.fn.strchars(rendered) <= 190, true)
  MiniTest.expect.equality(rendered:sub(-#"…"), "…")
end

T["summarize"]["falls back to a compact preview for an unrecognized update kind"] = function()
  local notification = assert(Protocol.notification("session/update", {
    sessionId = "s1",
    update = { sessionUpdate = "some_future_kind" },
  }))
  MiniTest.expect.equality(LogView.summarize(notification), 'some_future_kind {"sessionUpdate":"some_future_kind"}')
end

T["render_line"] = MiniTest.new_set()

T["render_line"]["decodes and summarizes a raw JSON-RPC line"] = function()
  local response = Protocol.response(1, { ok = true })
  MiniTest.expect.equality(LogView.render_line(line(response)), '← #1 {"ok":true}')
end

T["render_line"]["returns a truncated preview with an error tag for undecodable input"] = function()
  local rendered = LogView.render_line("not json")
  MiniTest.expect.equality(rendered, "not json  [invalid JSON]")
end

T["render_line"]["passes an empty line through unchanged"] = function()
  MiniTest.expect.equality(LogView.render_line(""), "")
end

T["fold integration"] = MiniTest.new_set({
  hooks = {
    pre_case = function()
      nvim.cmd("new")
    end,
    post_case = function()
      nvim.cmd("bwipeout!")
    end,
  },
})

T["fold integration"]["folds every line closed and renders its summary as foldtext"] = function()
  local win = nvim.api.nvim_get_current_win()
  local buffer = nvim.api.nvim_get_current_buf()
  local response = Protocol.response(1, { ok = true })
  nvim.api.nvim_buf_set_lines(buffer, 0, -1, false, { line(response), line(Protocol.response(2, { ok = false })) })

  LogView.enable(win)

  MiniTest.expect.equality(nvim.fn.foldclosed(1), 1)
  MiniTest.expect.equality(nvim.fn.foldclosed(2), 2)
  MiniTest.expect.equality(nvim.fn.foldtextresult(1), '← #1 {"ok":true}')
  MiniTest.expect.equality(nvim.fn.foldtextresult(2), '← #2 {"ok":false}')
end

T["fold integration"]["toggle restores the window's prior fold settings"] = function()
  local win = nvim.api.nvim_get_current_win()
  local buffer = nvim.api.nvim_get_current_buf()
  nvim.api.nvim_buf_set_lines(buffer, 0, -1, false, { line(Protocol.response(1, {})) })
  nvim.wo[win].foldmethod = "manual"

  LogView.toggle(win)
  MiniTest.expect.equality(nvim.wo[win].foldmethod, "expr")
  MiniTest.expect.equality(LogView.is_enabled(win), true)

  LogView.toggle(win)
  MiniTest.expect.equality(nvim.wo[win].foldmethod, "manual")
  MiniTest.expect.equality(LogView.is_enabled(win), false)
end

T["fold integration"]["leaves buffer content untouched"] = function()
  local win = nvim.api.nvim_get_current_win()
  local buffer = nvim.api.nvim_get_current_buf()
  local raw = line(Protocol.response(1, { ok = true }))
  nvim.api.nvim_buf_set_lines(buffer, 0, -1, false, { raw })

  LogView.enable(win)

  MiniTest.expect.equality(nvim.api.nvim_buf_get_lines(buffer, 0, -1, false), { raw })
end

T["fold integration"]["reuses a cached summary instead of re-rendering an unchanged line"] = function()
  local win = nvim.api.nvim_get_current_win()
  local buffer = nvim.api.nvim_get_current_buf()
  nvim.api.nvim_buf_set_lines(buffer, 0, -1, false, { line(Protocol.response(1, { ok = true })) })
  LogView.enable(win)

  local calls = 0
  local original_render_line = LogView.render_line
  ---@diagnostic disable-next-line: duplicate-set-field -- spying on the module's own function to count calls.
  LogView.render_line = function(...)
    calls = calls + 1
    return original_render_line(...)
  end

  nvim.fn.foldtextresult(1)
  nvim.fn.foldtextresult(1)
  nvim.fn.foldtextresult(1)
  LogView.render_line = original_render_line

  MiniTest.expect.equality(calls, 1)
end

return T
