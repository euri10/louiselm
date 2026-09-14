local MiniTest = require("mini.test")
local SessionOverview = require("louiselm.ui.session_overview")
local Chat = require("louiselm.ui.chat")

local T = MiniTest.new_set()

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local function fake_session(id, agent, working_dir)
  local listeners = {}
  local session = {
    state = {
      id = id,
      agent = agent,
      status = "ready",
      current_turn = 0,
      config_options = {},
      working_dir = working_dir or nvim.fn.getcwd(),
    },
    prompts = {},
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

  function session:usage_history(callback)
    nvim.schedule(function()
      callback({})
    end)
  end

  function session:option_usage(_, callback)
    nvim.schedule(function()
      callback({})
    end)
  end

  function session:prompt(prompt)
    self.prompts[#self.prompts + 1] = prompt
    return #self.prompts
  end

  function session:set_config_option(_, _, callback)
    if callback ~= nil then
      callback(self.state.config_options)
    end
    return 1
  end

  function session:cancel()
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
    limits = {},
    create_session = function()
      return nil, "not implemented"
    end,
    get_session = function()
      return nil
    end,
    list_sessions = function()
      return {}
    end,
    inspect_agent_limits = function(_, agent)
      return { agent = agent, status = "not_observed" }
    end,
    refresh_agent_limits = function(self, agent, callback)
      callback(self:inspect_agent_limits(agent))
      return true
    end,
    on_agent_limits = function()
      return function() end
    end,
    dispose = function()
      return true
    end,
  }
end

T["diff_parser"] = MiniTest.new_set()

T["diff_parser"]["parses single and multiple hunks with exact line numbers and stats"] = function()
  local diff = table.concat({
    "--- a/src/test.lua",
    "+++ b/src/test.lua",
    "@@ -10,4 +10,6 @@",
    " context",
    "-old line",
    "+new line 1",
    "+new line 2",
    "+new line 3",
    " context",
    "@@ -50 +52,2 @@",
    "-replaced",
    "+line A",
    "+line B",
  }, "\n")

  local hunks, added, deleted = SessionOverview.parse_diff_hunks(diff)
  MiniTest.expect.equality(#hunks, 2)
  MiniTest.expect.equality(hunks[1].start_line, 10)
  MiniTest.expect.equality(hunks[1].end_line, 15)
  MiniTest.expect.equality(hunks[1].added, 3)
  MiniTest.expect.equality(hunks[1].deleted, 1)

  MiniTest.expect.equality(hunks[2].start_line, 52)
  MiniTest.expect.equality(hunks[2].end_line, 53)
  MiniTest.expect.equality(hunks[2].added, 2)
  MiniTest.expect.equality(hunks[2].deleted, 1)

  MiniTest.expect.equality(added, 5)
  MiniTest.expect.equality(deleted, 2)
end

T["diff_parser"]["handles empty or malformed diff cleanly"] = function()
  local hunks, added, deleted = SessionOverview.parse_diff_hunks("")
  MiniTest.expect.equality(#hunks, 0)
  MiniTest.expect.equality(added, 0)
  MiniTest.expect.equality(deleted, 0)
end

T["extract_file_edit"] = MiniTest.new_set()

T["extract_file_edit"]["extracts from diff payload"] = function()
  local raw = {
    title = "Edit file",
    rawInput = {
      path = "lua/louiselm/test.lua",
      diff = "@@ -20,3 +20,5 @@\n context\n+add1\n+add2\n context",
    },
  }
  local edit = SessionOverview.extract_file_edit(raw)
  MiniTest.expect.equality(edit ~= nil, true)
  assert(edit ~= nil)
  MiniTest.expect.equality(edit.display_path, "lua/louiselm/test.lua")
  MiniTest.expect.equality(#edit.hunks, 1)
  MiniTest.expect.equality(edit.hunks[1].start_line, 20)
  MiniTest.expect.equality(edit.hunks[1].end_line, 24)
  MiniTest.expect.equality(edit.total_added, 2)
  MiniTest.expect.equality(edit.total_deleted, 0)
end

T["extract_file_edit"]["extracts from explicit line numbers"] = function()
  local raw = {
    title = "Edit file",
    rawInput = {
      path = "src/main.rs",
      start_line = 42,
      end_line = 50,
    },
  }
  local edit = SessionOverview.extract_file_edit(raw)
  MiniTest.expect.equality(edit ~= nil, true)
  assert(edit ~= nil)
  MiniTest.expect.equality(edit.hunks[1].start_line, 42)
  MiniTest.expect.equality(edit.hunks[1].end_line, 50)
end

T["extract_file_edit"]["extracts from new file content"] = function()
  local raw = {
    title = "Create new module",
    rawInput = {
      path = "/nonexistent/path/new_module.lua",
      content = "local M = {}\n\nfunction M.hello()\n  return 'world'\nend\n\nreturn M\n",
    },
  }
  local edit = SessionOverview.extract_file_edit(raw)
  MiniTest.expect.equality(edit ~= nil, true)
  assert(edit ~= nil)
  MiniTest.expect.equality(edit.is_new, true)
  MiniTest.expect.equality(edit.hunks[1].start_line, 1)
  MiniTest.expect.equality(edit.hunks[1].end_line, 8)
  MiniTest.expect.equality(edit.total_added, 8)
end

T["conflicts"] = MiniTest.new_set()

T["conflicts"]["identifies when multiple sessions modify the same file"] = function()
  local summary1 = {
    session_id = "session-1",
    agent = "claude",
    name = "session-1",
    status = "ready",
    working_dir = "/app",
    files = {
      {
        path = "/app/src/shared.lua",
        display_path = "src/shared.lua",
        hunks = { { start_line = 10, end_line = 20, added = 10, deleted = 2 } },
        total_added = 10,
        total_deleted = 2,
        is_new = false,
      },
    },
    total_files = 1,
    total_added = 10,
    total_deleted = 2,
  }

  local summary2 = {
    session_id = "session-2",
    agent = "deepseek",
    name = "session-2",
    status = "prompting",
    working_dir = "/app",
    files = {
      {
        path = "/app/src/shared.lua",
        display_path = "src/shared.lua",
        hunks = { { start_line = 15, end_line = 25, added = 5, deleted = 5 } },
        total_added = 5,
        total_deleted = 5,
        is_new = false,
      },
      {
        path = "/app/src/other.lua",
        display_path = "src/other.lua",
        hunks = { { start_line = 1, end_line = 5, added = 5, deleted = 0 } },
        total_added = 5,
        total_deleted = 0,
        is_new = false,
      },
    },
    total_files = 2,
    total_added = 10,
    total_deleted = 5,
  }

  SessionOverview.detect_conflicts({ summary1, summary2 })

  MiniTest.expect.equality(summary1.files[1].conflicts, { "session-2" })
  MiniTest.expect.equality(summary2.files[1].conflicts, { "session-1" })
  MiniTest.expect.equality(summary2.files[2].conflicts, nil)
end

T["rendering"] = MiniTest.new_set()

T["rendering"]["renders buffer lines and line jump targets"] = function()
  local summary = {
    session_id = "session-1",
    agent = "claude",
    name = "Task A",
    status = "ready",
    working_dir = "/app",
    files = {
      {
        path = "/app/src/auth.lua",
        display_path = "src/auth.lua",
        hunks = {
          { start_line = 42, end_line = 55, added = 14, deleted = 2 },
          { start_line = 100, end_line = 100, added = 1, deleted = 0 },
        },
        total_added = 15,
        total_deleted = 2,
        is_new = false,
        conflicts = { "session-2" },
      },
    },
    total_files = 1,
    total_added = 15,
    total_deleted = 2,
  }

  local lines, targets = SessionOverview.render_session_buffer(summary)
  local joined = table.concat(lines, "\n")
  MiniTest.expect.equality(joined:find("SESSION: Task A %(claude%)") ~= nil, true)
  MiniTest.expect.equality(joined:find("STATUS:  ready") ~= nil, true)
  MiniTest.expect.equality(joined:find("▾ src/auth.lua %(%+15 %-2%)") ~= nil, true)
  MiniTest.expect.equality(joined:find("⚠️  CONFLICT: also modified in: session%-2") ~= nil, true)
  MiniTest.expect.equality(joined:find("• L42%-55 %(%+14 %-2%)") ~= nil, true)
  MiniTest.expect.equality(joined:find("• L100 %(%+1 %-0%)") ~= nil, true)

  -- Targets point to lines
  local has_target_42 = false
  for _, target in pairs(targets) do
    if target.path == "/app/src/auth.lua" and target.line == 42 then
      has_target_42 = true
    end
  end
  MiniTest.expect.equality(has_target_42, true)
end

T["multi_window_overview"] = MiniTest.new_set()

T["multi_window_overview"]["completed Codex multi-file diffs populate and refresh the overview"] = function()
  -- Captured ordering/shape: proxy/sessions/01a0a0b7-147e-75e1-a37e-f3bf254b9764/log.jsonl:484-485
  -- under ~/.local/state/acp-llm-adapter/: two diffs, then a status-only completion.
  -- Paths and text are replaced; no rawInput was present in the captured call.
  local chat = assert(Chat.new(fake_api()))
  local session = fake_session("codex-edits", "codex", "/workspace")
  assert(chat:attach(session))
  assert(chat:session_overview())
  local buf = nvim.api.nvim_get_current_buf()
  local function text()
    return table.concat(nvim.api.nvim_buf_get_lines(buf, 0, -1, false), "\n")
  end
  MiniTest.expect.equality(text():find("(no files modified yet)", 1, true) ~= nil, true)
  local started = {
    toolCallId = "edit-1",
    kind = "edit",
    status = "in_progress",
    content = {
      { type = "diff", path = "/workspace/existing.lua", oldText = "same\nold\n", newText = "same\nnew\nextra\n" },
      { type = "diff", path = "/workspace/new.lua", oldText = nvim.NIL, newText = "created\n" },
    },
  }
  session:emit({ type = "tool_call_started", session_id = "codex-edits", data = started })
  SessionOverview.refresh()
  MiniTest.expect.equality(text():find("(no files modified yet)", 1, true) ~= nil, true)
  session:emit({
    type = "tool_call_finished",
    session_id = "codex-edits",
    data = { toolCallId = "edit-1", status = "completed" },
  })
  MiniTest.expect.equality(
    nvim.wait(200, function()
      return text():find("2 modified (+3 -1)", 1, true) ~= nil
    end),
    true
  )
  MiniTest.expect.equality(text():find("existing.lua", 1, true) ~= nil, true)
  MiniTest.expect.equality(text():find("new.lua", 1, true) ~= nil, true)
  assert(SessionOverview.close())
  assert(chat:session_overview())
  buf = nvim.api.nvim_get_current_buf()
  MiniTest.expect.equality(text():find("2 modified (+3 -1)", 1, true) ~= nil, true)
  -- Failed calls, malformed blocks and unchanged text must not create file entries.
  for index, block in ipairs({
    { type = "diff", path = "/workspace/failed.lua", oldText = "old\n", newText = "new\n" },
    { type = "diff", path = "/workspace/bad.lua", oldText = 42, newText = "new\n" },
    { type = "diff", path = "/workspace/same.lua", oldText = "same\n", newText = "same\n" },
  }) do
    session:emit({
      type = "tool_call_finished",
      session_id = "codex-edits",
      data = { toolCallId = "ignored-" .. index, status = index == 1 and "failed" or "completed", content = { block } },
    })
  end
  SessionOverview.refresh()
  MiniTest.expect.equality(text():find("2 modified (+3 -1)", 1, true) ~= nil, true)
  chat:dispose()
end

T["multi_window_overview"]["opens 5 vertical windows for 5 sessions"] = function()
  local chat = assert(Chat.new(fake_api()))

  -- Create 5 sessions with distinct edits
  local sessions = {}
  for i = 1, 5 do
    local s = fake_session("session-" .. i, "agent-" .. i)
    s.edits = {
      {
        rawInput = {
          path = "/tmp/task_" .. i .. ".txt",
          diff = string.format("@@ -%d,5 +%d,8 @@\n context\n+edit%d\n context", i * 10, i * 10, i),
        },
      },
    }
    assert(chat:attach(s))
    sessions[i] = s
  end

  local opened, open_error = SessionOverview.open(chat)
  MiniTest.expect.equality(open_error, nil)
  MiniTest.expect.equality(opened, true)
  MiniTest.expect.equality(SessionOverview.is_open(), true)

  local tabpage = nvim.api.nvim_get_current_tabpage()
  local wins = nvim.api.nvim_tabpage_list_wins(tabpage)
  MiniTest.expect.equality(#wins, 5)

  for i, win in ipairs(wins) do
    local buf = nvim.api.nvim_win_get_buf(win)
    local lines = nvim.api.nvim_buf_get_lines(buf, 0, -1, false)
    local text = table.concat(lines, "\n")
    MiniTest.expect.equality(text:find("SESSION: session%-" .. i) ~= nil, true)
    MiniTest.expect.equality(text:find("task_" .. i .. "%.txt") ~= nil, true)
  end

  -- Close overview
  assert(SessionOverview.close())
  MiniTest.expect.equality(SessionOverview.is_open(), false)
  chat:dispose()
end

T["multi_window_overview"]["chat:session_overview opens and refreshes live on events"] = function()
  local chat = assert(Chat.new(fake_api()))
  local s1 = fake_session("s1", "claude")
  local s2 = fake_session("s2", "codex")
  assert(chat:attach(s1))
  assert(chat:attach(s2))

  assert(chat:session_overview())
  MiniTest.expect.equality(SessionOverview.is_open(), true)

  local wins = nvim.api.nvim_tabpage_list_wins(0)
  MiniTest.expect.equality(#wins, 2)

  -- Initial state: no files modified
  local buf1 = nvim.api.nvim_win_get_buf(wins[1])
  local text1 = table.concat(nvim.api.nvim_buf_get_lines(buf1, 0, -1, false), "\n")
  MiniTest.expect.equality(text1:find("%(no files modified yet%)") ~= nil, true)

  -- Emit tool call finished modifying a file on s1
  s1.edits = {
    {
      rawInput = {
        path = "lua/app.lua",
        start_line = 10,
        end_line = 25,
      },
    },
  }
  s1:emit({
    type = "tool_call_finished",
    session_id = "s1",
    data = { status = "completed" },
  })

  nvim.wait(50)

  text1 = table.concat(nvim.api.nvim_buf_get_lines(buf1, 0, -1, false), "\n")
  MiniTest.expect.equality(text1:find("app%.lua") ~= nil, true)
  MiniTest.expect.equality(text1:find("L10%-25") ~= nil, true)

  chat:dispose()
  MiniTest.expect.equality(SessionOverview.is_open(), false)
end

T["multi_window_overview"]["jump keymap navigates to target file and line"] = function()
  local tmp_file = nvim.fn.tempname() .. ".lua"
  local content = {}
  for i = 1, 50 do
    content[#content + 1] = "line " .. i
  end
  nvim.fn.writefile(content, tmp_file)

  local chat = assert(Chat.new(fake_api()))
  local s1 = fake_session("s1", "claude")
  s1.edits = {
    {
      rawInput = {
        path = tmp_file,
        start_line = 35,
        end_line = 40,
      },
    },
  }
  assert(chat:attach(s1))
  assert(chat:session_overview())

  local buf = nvim.api.nvim_get_current_buf()
  local lines = nvim.api.nvim_buf_get_lines(buf, 0, -1, false)
  local target_row = nil
  for row, line in ipairs(lines) do
    if line:find("L35%-40") ~= nil then
      target_row = row
      break
    end
  end
  assert(target_row ~= nil)

  nvim.api.nvim_win_set_cursor(0, { target_row, 0 })

  -- Trigger <CR> keymap
  local map = nil
  for _, m in ipairs(nvim.api.nvim_buf_get_keymap(buf, "n")) do
    if m.lhs == "<CR>" then
      map = m.callback
      break
    end
  end
  assert(type(map) == "function")
  map()

  local current_buf = nvim.api.nvim_get_current_buf()
  local current_path = nvim.fs.normalize(nvim.api.nvim_buf_get_name(current_buf))
  local expected_path = nvim.fs.normalize(tmp_file)
  MiniTest.expect.equality(current_path, expected_path)
  local cursor = nvim.api.nvim_win_get_cursor(0)
  MiniTest.expect.equality(cursor[1], 35)

  SessionOverview.close()
  chat:dispose()
  if nvim.api.nvim_buf_is_valid(current_buf) then
    nvim.api.nvim_buf_delete(current_buf, { force = true })
  end
  nvim.fn.delete(tmp_file)
end

T["multi_window_overview"]["q keymap closes overview"] = function()
  local chat = assert(Chat.new(fake_api()))
  local s1 = fake_session("s1", "claude")
  assert(chat:attach(s1))
  assert(chat:session_overview())
  MiniTest.expect.equality(SessionOverview.is_open(), true)

  local buf = nvim.api.nvim_get_current_buf()
  local close_map = nil
  for _, m in ipairs(nvim.api.nvim_buf_get_keymap(buf, "n")) do
    if m.lhs == "q" then
      close_map = m.callback
      break
    end
  end
  assert(type(close_map) == "function")
  close_map()

  MiniTest.expect.equality(SessionOverview.is_open(), false)
  chat:dispose()
end

T["extract_file_edit"]["extracts line range for text replacement"] = function()
  local tmp_file = nvim.fn.tempname() .. ".lua"
  local content = { "line 1", "line 2", "target to replace", "line 4", "line 5" }
  nvim.fn.writefile(content, tmp_file)

  local raw = {
    title = "Replace target line",
    rawInput = {
      path = tmp_file,
      old_str = "target to replace",
      new_str = "new line A\nnew line B",
    },
  }
  local edit = SessionOverview.extract_file_edit(raw)
  MiniTest.expect.equality(edit ~= nil, true)
  assert(edit ~= nil)
  MiniTest.expect.equality(edit.hunks[1].start_line, 3)
  MiniTest.expect.equality(edit.hunks[1].end_line, 4)
  MiniTest.expect.equality(edit.total_added, 2)
  MiniTest.expect.equality(edit.total_deleted, 1)

  nvim.fn.delete(tmp_file)
end

return T
