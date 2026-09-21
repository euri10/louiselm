local MiniTest = require("mini.test")
local Buffer = require("louiselm.ui.chat.buffer")

---@diagnostic disable-next-line: undefined-global -- Neovim injects its runtime API.
local nvim = vim

local T = MiniTest.new_set()

local function buffer_lines(buffer)
  return nvim.api.nvim_buf_get_lines(buffer, 0, -1, false)
end

local function new_buffer(options)
  local state = { id = "buffer-test", agent = "claude", status = "ready", config_options = {} }
  local owner = Buffer.new(state, options or {
    markdown_highlighting = false,
    on_prompt_edit = function() end,
    prompt_prefix = function()
      return ""
    end,
    on_enter = function() end,
    submit = function() end,
  })
  MiniTest.finally(function()
    owner:dispose()
  end)
  owner:show(nvim.api.nvim_get_current_win(), false)
  return owner
end

T["prompt focus preserves the prefix while typing"] = MiniTest.new_set({
  parametrize = {
    { "enter", "", "" },
    { "enter", "", "draft\nsecond line" },
    { "enter", "[context] ", "" },
    { "enter", "[context] ", "draft" },
    { "show", "", "" },
    { "show", "", "draft" },
  },
})
T["prompt focus preserves the prefix while typing"]["with an attached UI"] = function(action, prefix, draft)
  local child = MiniTest.new_child_neovim()
  MiniTest.finally(function()
    child.stop()
  end)
  child.start({ "--noplugin", "-u", "NONE" })
  child.api.nvim_ui_attach(80, 24, { rgb = true })
  child.lua("vim.opt.runtimepath:prepend(...)", { nvim.fn.getcwd() })
  child.lua(
    [[
    local prefix, draft = ...
    local owner = require("louiselm.ui.chat.buffer").new({
      id = "prompt-focus-test", agent = "test", status = "ready", config_options = {},
    }, {
      markdown_highlighting = false,
      on_prompt_edit = function() end,
      prompt_prefix = function() return prefix end,
      on_enter = function() end,
      submit = function() error("typing must not submit") end,
    })
    _G.qa_prompt_owner = owner
    owner:replace_prompt(prefix .. draft)
    owner:append({ "previous reply", "" })
    owner:show(vim.api.nvim_get_current_win(), false)
  ]],
    { prefix, draft }
  )
  child.type_keys("gg")
  if action == "enter" then
    child.type_keys("<CR>")
  else
    child.lua([[
      vim.keymap.set("n", "<F5>", function()
        qa_prompt_owner:show(vim.api.nvim_get_current_win(), true)
      end)
    ]])
    child.type_keys("<F5>")
  end
  MiniTest.expect.equality(child.api.nvim_get_mode().mode, "i")
  child.type_keys("QA-CURSOR")
  MiniTest.expect.equality(child.lua_get("qa_prompt_owner:prompt_text()"), prefix .. "QA-CURSOR" .. draft)
  MiniTest.expect.equality(
    child.lua_get("vim.api.nvim_buf_get_lines(qa_prompt_owner.buffer, qa_prompt_owner.prompt_line, -1, false)[1]"),
    "> " .. prefix .. "QA-CURSOR" .. (draft:match("^[^\n]*") or "")
  )
  child.type_keys("<Esc>", "gg", "<CR>")
  MiniTest.expect.equality(child.api.nvim_get_mode().mode, "i")
  child.type_keys("again-")
  MiniTest.expect.equality(child.lua_get("qa_prompt_owner:prompt_text()"), prefix .. "again-QA-CURSOR" .. draft)
end

T["patches compaction rows in place without splitting subsequent prose"] = function()
  local owner = new_buffer()
  owner:compaction({ id = "same", status = "in_progress" })
  owner:render({ type = "chunk", session_id = "buffer-test", data = { text = "after " } })
  owner:compaction({ id = "same", status = "completed", summary = { { type = "text", text = "summary" } } })
  owner:render({ type = "chunk", session_id = "buffer-test", data = { text = "boundary" } })
  local count = 0
  for line, text in ipairs(buffer_lines(owner.buffer)) do
    if text:find("[compaction]", 1, true) then
      count = count + 1
      MiniTest.expect.equality(text:find("completed", 1, true) ~= nil, true)
      nvim.api.nvim_win_set_cursor(owner.window, { line, 0 })
      MiniTest.expect.equality(owner:compaction_at_cursor(), "same")
      MiniTest.expect.equality(owner:tool_at_cursor(), nil)
    end
  end
  MiniTest.expect.equality(count, 1)
  MiniTest.expect.equality(table.concat(buffer_lines(owner.buffer), "\n"):find("after boundary", 1, true) ~= nil, true)
end

T["preserves multiline prompts and drafts while prose streams between tool and reasoning blocks"] = function()
  local owner = new_buffer()
  owner:replace_prompt("first\nsecond")
  owner:accept_prompt("first\nsecond", {}, "", false)
  owner:replace_prompt("draft\nnext line")
  owner:render({ type = "chunk", session_id = "buffer-test", data = { text = "hello\nwor" } })
  owner:render({ type = "chunk", session_id = "buffer-test", data = { text = "ld" } })
  owner:render({
    type = "tool_call_started",
    session_id = "buffer-test",
    data = { toolCallId = "read", title = "Read" },
  })
  owner:render({
    type = "tool_call_finished",
    session_id = "buffer-test",
    data = { toolCallId = "read", status = "completed" },
  })
  owner:render({ type = "thought_chunk", session_id = "buffer-test", data = { text = "consider\nthis" } })
  owner:render({ type = "chunk", session_id = "buffer-test", data = { text = "answer" } })
  owner:finish_turn()
  owner:reset_response()
  MiniTest.expect.equality(nvim.list_slice(buffer_lines(owner.buffer), 6), {
    "> first",
    "> second",
    "",
    "hello",
    "world",
    "",
    "[tool] read: Read (completed)",
    "[thinking]",
    "consider",
    "this",
    "",
    "answer",
    "",
    "> draft",
    "> next line",
  })
  MiniTest.expect.equality(owner:prompt_text(), "draft\nnext line")
  MiniTest.expect.equality(nvim.fn.foldclosed(14), 13)
  MiniTest.expect.equality(nvim.fn.foldclosedend(14), 15)
end

T["keeps context folds and prompt navigation behind the buffer API"] = function()
  local owner = new_buffer()
  owner:replace_prompt("question")
  owner:accept_prompt("question", { { label = "notes", text = "hidden\ncontext" } }, "", false)
  owner:render({ type = "chunk", session_id = "buffer-test", data = { text = "answer" } })
  owner:finish_turn()
  owner:reset_response()
  MiniTest.expect.equality(nvim.fn.foldclosed(7), 6)
  MiniTest.expect.equality(nvim.fn.foldclosedend(7), 9)
  local lines = buffer_lines(owner.buffer)
  nvim.api.nvim_win_set_cursor(0, { #lines, 0 })
  nvim.api.nvim_feedkeys("[u", "mx", false)
  MiniTest.expect.equality(nvim.api.nvim_get_current_line(), "> question")
  nvim.api.nvim_feedkeys("]r", "mx", false)
  MiniTest.expect.equality(nvim.api.nvim_get_current_line(), "answer")
end

T["updates one completed tool row across replay notifications and late payloads"] = function()
  local owner = new_buffer()
  -- louiselm-r141: proxy/sessions/01a07efc-1094-7873-9c89-91e31c3e6f52/log.jsonl
  -- lines 194-201: completed tool_call followed by titleless completed update.
  for _, id in ipairs({ "one", "two", "three" }) do
    owner:render({
      type = "tool_call_finished",
      session_id = "buffer-test",
      data = { toolCallId = id, title = "Read " .. id, status = "completed" },
    })
    owner:render({
      type = "tool_call_finished",
      session_id = "buffer-test",
      data = { toolCallId = id, status = "completed" },
    })
  end
  owner:finish_turn()
  local rows = {}
  for line, text in ipairs(buffer_lines(owner.buffer)) do
    if text:match("^%[tool%]") then
      rows[#rows + 1] = text
      nvim.api.nvim_win_set_cursor(0, { line, 0 })
      MiniTest.expect.equality(owner:tool_at_cursor(), ({ "one", "two", "three" })[#rows])
    end
  end
  MiniTest.expect.equality(rows, {
    "[tool] one: Read one (completed)",
    "[tool] two: Read two (completed)",
    "[tool] three: Read three (completed)",
  })
  local before = #buffer_lines(owner.buffer)
  owner:render({
    type = "tool_call_finished",
    session_id = "buffer-test",
    data = { toolCallId = "one", title = "Updated read", status = "failed" },
  })
  MiniTest.expect.equality(#buffer_lines(owner.buffer), before)
  MiniTest.expect.equality(
    table.concat(buffer_lines(owner.buffer), "\n"):find("Updated read (failed)", 1, true) ~= nil,
    true
  )
end

T["renders hidden buffers and deletes buffer-local resources on repeated disposal"] = function()
  local entered, submitted, edited = 0, 0, 0
  local owner = new_buffer({
    markdown_highlighting = false,
    on_prompt_edit = function()
      edited = edited + 1
    end,
    prompt_prefix = function()
      return ""
    end,
    on_enter = function()
      entered = entered + 1
    end,
    submit = function()
      submitted = submitted + 1
    end,
  })
  owner:replace_prompt("draft")
  MiniTest.expect.equality(edited > 0, true)
  for _, mapping in ipairs(nvim.api.nvim_buf_get_keymap(owner.buffer, "i")) do
    if mapping.lhs == "<CR>" then
      mapping.callback()
    end
  end
  MiniTest.expect.equality(submitted, 1)
  local other = nvim.api.nvim_create_buf(false, true)
  MiniTest.finally(function()
    if nvim.api.nvim_buf_is_valid(other) then
      nvim.api.nvim_buf_delete(other, { force = true })
    end
  end)
  nvim.api.nvim_set_current_buf(other)
  owner:render({ type = "chunk", session_id = "buffer-test", data = { text = "hidden response" } })
  MiniTest.expect.equality(nvim.api.nvim_get_current_buf(), other)
  MiniTest.expect.equality(owner:prompt_text(), "draft")
  owner:show(nvim.api.nvim_get_current_win(), false)
  MiniTest.expect.equality(entered >= 2, true)
  MiniTest.expect.equality(nvim.tbl_contains(buffer_lines(owner.buffer), "hidden response"), true)
  owner:dispose()
  owner:dispose()
  MiniTest.expect.equality(nvim.api.nvim_buf_is_valid(owner.buffer), false)
  for _, autocmd in ipairs(nvim.api.nvim_get_autocmds({ event = { "BufEnter", "WinEnter" } })) do
    MiniTest.expect.equality(autocmd.buffer == owner.buffer, false)
  end
end

T["coalesces repeated nonterminal tool_call_started frames into one row"] = function()
  -- Live-shaped regression for louiselm-66qs: a long-running tool call emits
  -- many nonterminal tool_call_update frames for the same toolCallId with no
  -- title before the terminal completion. Each one used to append a fresh
  -- row, leaving a stale "(started)" duplicate behind the completed line.
  local owner = new_buffer()

  owner:render({
    type = "tool_call_started",
    session_id = "session-1",
    data = { toolCallId = "exec-1", title = "Run tests" },
  })
  for _ = 1, 17 do
    owner:render({
      type = "tool_call_started",
      session_id = "session-1",
      data = { toolCallId = "exec-1" },
    })
  end
  owner:render({
    type = "tool_call_finished",
    session_id = "session-1",
    data = { toolCallId = "exec-1", status = "completed" },
  })

  local lines = buffer_lines(owner.buffer)
  local tool_lines = nvim.tbl_filter(function(line)
    return line:find("exec-1", 1, true) ~= nil
  end, lines)
  MiniTest.expect.equality(tool_lines, { "[tool] exec-1: Run tests (completed)" })
end

return T
