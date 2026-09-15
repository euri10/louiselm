local MiniTest = require("mini.test")
local SessionOverview = require("louiselm.ui.session_overview")
local Chat = require("louiselm.ui.chat")

local T = MiniTest.new_set()

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local chats = {}

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

local function new_chat()
  local instance = assert(Chat.new(fake_api(), { start_insert_on_switch = false }))
  chats[#chats + 1] = instance
  return instance
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

T["sidebar"] = MiniTest.new_set({
  hooks = {
    post_case = function()
      for _, chat in ipairs(chats) do
        chat:dispose()
      end
      chats = {}
    end,
  },
})

T["sidebar"]["completed Codex multi-file diffs populate and refresh the overview"] = function()
  -- Captured ordering/shape: proxy/sessions/01a0a0b7-147e-75e1-a37e-f3bf254b9764/log.jsonl:484-485
  -- under ~/.local/state/acp-llm-adapter/: two diffs, then a status-only completion.
  -- Paths and text are replaced; no rawInput was present in the captured call.
  local chat = new_chat()
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
  SessionOverview.refresh(chat)
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
  assert(SessionOverview.close(chat))
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
  SessionOverview.refresh(chat)
  MiniTest.expect.equality(text():find("2 modified (+3 -1)", 1, true) ~= nil, true)
  chat:dispose()
end

T["sidebar"]["opens one left sidebar for the invoking Session and reuses it"] = function()
  local chat = new_chat()

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

  local tabpage = nvim.api.nvim_get_current_tabpage()
  local chat_win = nvim.api.nvim_get_current_win()
  local chat_buf = nvim.api.nvim_get_current_buf()
  local tab_count = #nvim.api.nvim_list_tabpages()
  local window_count = #nvim.api.nvim_tabpage_list_wins(tabpage)
  local opened, open_error = SessionOverview.open(chat)
  MiniTest.expect.equality(open_error, nil)
  MiniTest.expect.equality(opened, true)
  MiniTest.expect.equality(SessionOverview.is_open(chat), true)

  MiniTest.expect.equality(nvim.api.nvim_get_current_tabpage(), tabpage)
  MiniTest.expect.equality(#nvim.api.nvim_list_tabpages(), tab_count)
  local wins = nvim.api.nvim_tabpage_list_wins(tabpage)
  MiniTest.expect.equality(#wins, window_count + 1)
  local sidebar_win = nvim.api.nvim_get_current_win()
  local sidebar_buf = nvim.api.nvim_get_current_buf()
  MiniTest.expect.equality(nvim.api.nvim_win_get_position(sidebar_win)[2], 0)
  MiniTest.expect.equality(nvim.api.nvim_win_get_buf(chat_win), chat_buf)

  local function text()
    return table.concat(nvim.api.nvim_buf_get_lines(sidebar_buf, 0, -1, false), "\n")
  end
  MiniTest.expect.equality(text():find("SESSION: session-5", 1, true) ~= nil, true)
  MiniTest.expect.equality(text():find("task_5.txt", 1, true) ~= nil, true)
  MiniTest.expect.equality(text():find("task_1.txt", 1, true), nil)
  assert(chat:session_overview())
  MiniTest.expect.equality(nvim.api.nvim_get_current_win(), sidebar_win)
  MiniTest.expect.equality(#nvim.api.nvim_tabpage_list_wins(tabpage), window_count + 1)

  nvim.api.nvim_set_current_win(chat_win)
  assert(chat:switch("session-1"))
  MiniTest.expect.equality(SessionOverview.is_open(chat), false)
  MiniTest.expect.equality(nvim.api.nvim_buf_is_valid(sidebar_buf), false)
  assert(chat:session_overview())
  sidebar_buf = nvim.api.nvim_get_current_buf()
  MiniTest.expect.equality(#nvim.api.nvim_tabpage_list_wins(tabpage), window_count + 1)
  MiniTest.expect.equality(text():find("task_1.txt", 1, true) ~= nil, true)
  MiniTest.expect.equality(text():find("task_5.txt", 1, true), nil)

  -- Close overview
  assert(SessionOverview.close(chat))
  MiniTest.expect.equality(SessionOverview.is_open(chat), false)
  chat:dispose()
end

T["sidebar"]["chat:session_overview opens and refreshes live on events"] = function()
  local chat = new_chat()
  local s1 = fake_session("s1", "claude")
  local s2 = fake_session("s2", "codex")
  assert(chat:attach(s1))
  assert(chat:attach(s2))
  assert(chat:switch("s1"))

  assert(chat:session_overview())
  MiniTest.expect.equality(SessionOverview.is_open(chat), true)

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
  MiniTest.expect.equality(SessionOverview.is_open(chat), false)
end

T["sidebar"]["jump keymap navigates to target file and line"] = function()
  local tmp_file = nvim.fn.tempname() .. ".lua"
  local content = {}
  for i = 1, 50 do
    content[#content + 1] = "line " .. i
  end
  nvim.fn.writefile(content, tmp_file)

  local chat = new_chat()
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
  local chat_win = nvim.api.nvim_get_current_win()
  local chat_buf = nvim.api.nvim_get_current_buf()
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
  MiniTest.expect.equality(SessionOverview.is_open(chat), true)
  MiniTest.expect.equality(nvim.api.nvim_win_get_buf(chat_win), chat_buf)

  SessionOverview.close(chat)
  chat:dispose()
  if nvim.api.nvim_buf_is_valid(current_buf) then
    nvim.api.nvim_buf_delete(current_buf, { force = true })
  end
  nvim.fn.delete(tmp_file)
end

T["sidebar"]["q keymap closes overview"] = function()
  local chat = new_chat()
  local s1 = fake_session("s1", "claude")
  assert(chat:attach(s1))
  assert(chat:session_overview())
  MiniTest.expect.equality(SessionOverview.is_open(chat), true)

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

  MiniTest.expect.equality(SessionOverview.is_open(chat), false)
  chat:dispose()
end

T["sidebar"]["uses the focused chat buffer even when another Session was last switched"] = function()
  local chat = new_chat()
  assert(chat:attach(fake_session("focused", "codex")))
  local first_window = nvim.api.nvim_get_current_win()
  nvim.cmd("vsplit")
  local second_window = nvim.api.nvim_get_current_win()
  assert(chat:attach(fake_session("last-switched", "codex")))
  nvim.api.nvim_set_current_win(first_window)
  assert(chat:session_overview())
  local sidebar_buf = nvim.api.nvim_get_current_buf()
  local function text()
    return table.concat(nvim.api.nvim_buf_get_lines(sidebar_buf, 0, -1, false), "\n")
  end
  MiniTest.expect.equality(text():find("SESSION: focused", 1, true) ~= nil, true)
  assert(chat:session_overview())
  assert(SessionOverview.refresh(chat))
  MiniTest.expect.equality(text():find("SESSION: focused", 1, true) ~= nil, true)
  SessionOverview.close(chat)
  nvim.api.nvim_win_close(second_window, true)
end

T["sidebar"]["switching from the sidebar closes it before selecting the destination window"] = function()
  local chat = new_chat()
  local first = fake_session("first", "codex")
  assert(chat:attach(first))
  assert(chat:attach(fake_session("second", "codex")))
  assert(chat:switch("first"))
  local host = nvim.api.nvim_get_current_win()
  assert(chat:session_overview())
  local sidebar = nvim.api.nvim_get_current_win()
  first:emit({ type = "turn_done", session_id = "first" })

  assert(chat:switch("second"))
  MiniTest.expect.equality(SessionOverview.is_open(chat), false)
  MiniTest.expect.equality(nvim.api.nvim_win_is_valid(sidebar), false)
  MiniTest.expect.equality(nvim.api.nvim_get_current_win(), host)
  MiniTest.expect.equality(nvim.api.nvim_get_current_buf(), chat:buffer("second"))
  nvim.wait(20)
  MiniTest.expect.equality(chat.overview, nil)
  assert(chat:switch("first"))
  MiniTest.expect.equality(chat.overview, nil)
end

T["sidebar"]["another visible chat closes the sidebar but own chat and file focus preserve it"] = function()
  local chat = new_chat()
  assert(chat:attach(fake_session("first", "codex")))
  local first_window = nvim.api.nvim_get_current_win()
  nvim.cmd("vsplit")
  local second_window = nvim.api.nvim_get_current_win()
  assert(chat:attach(fake_session("second", "codex")))
  nvim.api.nvim_set_current_win(first_window)
  assert(chat:session_overview())
  nvim.api.nvim_set_current_win(first_window)
  MiniTest.expect.equality(SessionOverview.is_open(chat), true)

  local file = nvim.api.nvim_create_buf(true, false)
  local file_window = nvim.api.nvim_open_win(file, true, { split = "right", win = first_window })
  MiniTest.expect.equality(SessionOverview.is_open(chat), true)
  nvim.api.nvim_set_current_win(first_window)
  nvim.api.nvim_win_close(file_window, true)
  nvim.api.nvim_buf_delete(file, { force = true })

  nvim.api.nvim_set_current_win(second_window)
  MiniTest.expect.equality(SessionOverview.is_open(chat), false)
  MiniTest.expect.equality(nvim.api.nvim_get_current_win(), second_window)
  nvim.api.nvim_win_close(second_window, true)
end

T["sidebar"]["manual window close releases observers and queued events cannot revive it"] = function()
  local chat = new_chat()
  local session = fake_session("cleanup", "codex")
  assert(chat:attach(session))
  assert(chat:session_overview())
  local window = nvim.api.nvim_get_current_win()
  local buffer = nvim.api.nvim_get_current_buf()
  local group = chat.overview.augroup
  session:emit({ type = "state_changed", session_id = "cleanup", data = {} })
  nvim.api.nvim_win_close(window, true)
  MiniTest.expect.equality(
    nvim.wait(200, function()
      return chat.overview == nil
    end),
    true
  )
  MiniTest.expect.equality(nvim.api.nvim_buf_is_valid(buffer), false)
  MiniTest.expect.equality(nvim.fn.exists("#LouiselmOverview" .. buffer), 0)
  MiniTest.expect.equality(SessionOverview.is_open(chat), false)
  assert(chat:session_overview())
  MiniTest.expect.equality(chat.overview.augroup ~= group, true)
  session:emit({ type = "state_changed", session_id = "cleanup", data = {} })
  chat:dispose()
  nvim.wait(20)
  MiniTest.expect.equality(SessionOverview.is_open(chat), false)
end

T["sidebar"]["fast events refresh only after scheduling and keep other Sessions out"] = function()
  local chat = new_chat()
  local session = fake_session("fast", "codex")
  assert(chat:attach(session))
  assert(chat:attach(fake_session("background", "codex")))
  assert(chat:switch("fast"))
  assert(chat:session_overview())
  local buffer = nvim.api.nvim_get_current_buf()
  local function text()
    return table.concat(nvim.api.nvim_buf_get_lines(buffer, 0, -1, false), "\n")
  end
  local timer = assert(nvim.uv.new_timer())
  local event_error
  timer:start(0, 0, function()
    timer:close()
    session.state.status = "prompting"
    local ok, err = pcall(session.emit, session, { type = "state_changed", session_id = "fast", data = {} })
    if not ok then
      event_error = err
    end
  end)
  MiniTest.expect.equality(
    nvim.wait(200, function()
      return event_error ~= nil or text():find("STATUS:  prompting", 1, true) ~= nil
    end),
    true
  )
  MiniTest.expect.equality(event_error, nil)
  MiniTest.expect.equality(text():find("SESSION: background", 1, true), nil)
end

T["sidebar"]["disposing one chat leaves another chat sidebar open"] = function()
  local first = new_chat()
  assert(first:attach(fake_session("owner-a", "codex")))
  local host = nvim.api.nvim_get_current_win()
  assert(first:session_overview())
  nvim.api.nvim_set_current_win(host)
  local second = new_chat()
  assert(second:attach(fake_session("owner-b", "codex")))
  assert(second:session_overview())
  first:dispose()
  MiniTest.expect.equality(SessionOverview.is_open(second), true)
end

T["sidebar"]["missing and disposed subjects never fall back to another Session"] = function()
  local chat = new_chat()
  MiniTest.expect.equality({ chat:session_overview() }, { false, "no chat session is attached" })
  local subject = fake_session("retired", "codex")
  assert(chat:attach(subject))
  assert(chat:attach(fake_session("survivor", "codex")))
  assert(chat:switch("retired"))
  assert(chat:session_overview())
  subject:dispose()
  MiniTest.expect.equality(SessionOverview.refresh(chat), false)
  MiniTest.expect.equality(SessionOverview.is_open(chat), false)
  MiniTest.expect.equality({ chat:session_overview() }, { false, "session is disposed" })
  chat:dispose()
  MiniTest.expect.equality({ chat:session_overview() }, { false, "chat UI is disposed" })
end

T["sidebar"]["diff preview preserves the sidebar and chat"] = function()
  local chat = new_chat()
  local session = fake_session("preview", "codex")
  session.edits = { { rawInput = { path = "/tmp/overview-preview.lua", diff = "@@ -1 +1 @@\n-old\n+new" } } }
  assert(chat:attach(session))
  local host = nvim.api.nvim_get_current_win()
  local chat_buf = nvim.api.nvim_get_current_buf()
  assert(chat:session_overview())
  local buffer = nvim.api.nvim_get_current_buf()
  for row, line in ipairs(nvim.api.nvim_buf_get_lines(buffer, 0, -1, false)) do
    if line:find("▾", 1, true) ~= nil then
      nvim.api.nvim_win_set_cursor(0, { row, 0 })
      break
    end
  end
  for _, map in ipairs(nvim.api.nvim_buf_get_keymap(buffer, "n")) do
    if map.lhs == "d" then
      map.callback()
      break
    end
  end
  MiniTest.expect.equality(nvim.api.nvim_win_get_config(0).relative, "editor")
  MiniTest.expect.equality(SessionOverview.is_open(chat), true)
  MiniTest.expect.equality(nvim.api.nvim_win_get_buf(host), chat_buf)
  local preview_buffer = nvim.api.nvim_get_current_buf()
  chat:dispose()
  MiniTest.expect.equality(nvim.api.nvim_buf_is_valid(preview_buffer), false)
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
