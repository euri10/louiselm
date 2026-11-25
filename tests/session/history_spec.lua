local MiniTest = require("mini.test")
local Session = require("louiselm.session")

---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim

local T = MiniTest.new_set()

T["pages the exact Session transcript beyond the phone cache"] = function()
  local directory = nvim.fn.tempname()
  local definitions = {
    mock = {
      provider = "test-service",
      command = nvim.v.progpath,
      args = {
        "--headless",
        "--noplugin",
        "-u",
        "tests/mock/init.lua",
        "-c",
        "luafile tests/fixtures/usage_replay.lua",
      },
      env = { LOUISELM_TEST_REPLAY_TURNS = "180" },
    },
  }
  local api = assert(Session.new(definitions, nil, { usage_directory = directory }))
  local replacement_api
  MiniTest.finally(function()
    if replacement_api then
      assert(replacement_api:dispose())
      local replacement_flushed = false
      replacement_api:flush_recording(function()
        replacement_flushed = true
      end)
      assert(nvim.wait(6000, function()
        return replacement_flushed
      end))
    end
    assert(api:dispose())
    local finished = false
    api:flush_recording(function()
      finished = true
    end)
    assert(nvim.wait(6000, function()
      return finished
    end))
    nvim.fn.delete(directory, "rf")
  end)
  local session = assert(api:load_session("mock", "same-acp"))
  assert(nvim.wait(6000, function()
    return session:inspect().status == "ready"
  end))

  local function read(subject, before, revision)
    local result, failure
    session:read_history({ subject = subject, before = before, limit = 50, revision = revision }, function(page, err)
      assert(not nvim.in_fast_event())
      result, failure = page, err
    end)
    assert(result == nil and failure == nil, "history must complete asynchronously")
    assert(nvim.wait(6000, function()
      return result ~= nil or failure ~= nil
    end))
    return result, failure
  end

  local page = assert(read(nil, 0, 0))
  local subject, revision = page.subject, page.revision
  MiniTest.expect.equality(page.source.replay, "unverified")
  MiniTest.expect.equality(page.source.archive, "unavailable")
  MiniTest.expect.equality(page.source.redaction, "unknown")
  local total = #page.entries
  while page.next_before ~= 0 do
    page = assert(read(subject, page.next_before, revision))
    assert(#page.entries <= 50)
    total = total + #page.entries
  end
  MiniTest.expect.equality(total, 540)
  MiniTest.expect.equality(page.entries[1].text, "prompt 1continued 1")
  MiniTest.expect.equality(page.entries[1].seq, 1)

  local overlap_page, overlap_error
  session:read_history({ subject = subject, before = 2, limit = 1, revision = revision }, function(result, err)
    overlap_page, overlap_error = result, err
  end)
  session.emitter:emit({
    type = "chunk",
    session_id = session:inspect().id,
    data = { content = { type = "text", text = "concurrent output" } },
  })
  assert(nvim.wait(6000, function()
    return overlap_error ~= nil
  end))
  MiniTest.expect.equality(overlap_page, nil)
  MiniTest.expect.equality(overlap_error, "stale_subject")

  local done = false
  assert(session:prompt("new live instruction", function(_, err)
    assert(err == nil, err)
    done = true
  end))
  assert(nvim.wait(6000, function()
    return done
  end))
  local _, stale = read(subject, 2, revision)
  MiniTest.expect.equality(stale, "stale_subject")
  local _, wrong = read(string.rep("0", #subject), 2, 0)
  MiniTest.expect.equality(wrong, "stale_subject")

  local disposed
  session:read_history({ subject = subject, before = 0, limit = 1, revision = 0 }, function(_, err)
    disposed = err
  end)
  assert(session:dispose())
  assert(nvim.wait(6000, function()
    return disposed ~= nil
  end))
  MiniTest.expect.equality(disposed, "stale_subject")

  replacement_api = assert(Session.new(definitions, nil, { usage_directory = directory .. "/replacement" }))
  local replacement = assert(replacement_api:load_session("mock", "same-acp"))
  assert(nvim.wait(6000, function()
    return replacement:inspect().status == "ready"
  end))
  MiniTest.expect.equality(replacement:inspect().id, session:inspect().id)
  local replacement_error
  replacement:read_history({ subject = subject, before = 2, limit = 1, revision = 0 }, function(_, err)
    replacement_error = err
  end)
  assert(nvim.wait(6000, function()
    return replacement_error ~= nil
  end))
  MiniTest.expect.equality(replacement_error, "stale_subject")
end

T["reports withheld, missing and oversized content without copying raw tool data"] = function()
  local directory = nvim.fn.tempname()
  local api = assert(Session.new({
    mock = {
      provider = "test-service",
      command = nvim.v.progpath,
      args = {
        "--headless",
        "--noplugin",
        "-u",
        "tests/mock/init.lua",
        "-c",
        "lua require('louiselm.dev.mock_agent').run()",
      },
    },
  }, nil, { usage_directory = directory }))
  MiniTest.finally(function()
    assert(api:dispose())
    local finished = false
    api:flush_recording(function()
      finished = true
    end)
    assert(nvim.wait(6000, function()
      return finished
    end))
    nvim.fn.delete(directory, "rf")
  end)
  local session = assert(api:create_session("mock"))
  assert(nvim.wait(6000, function()
    return session:inspect().status == "ready"
  end))
  local done = false
  assert(session:prompt({
    { type = "text", text = "VISIBLE" },
    { type = "resource", resource = { uri = "file:///private", text = "SECRET RESOURCE" } },
  }, function(_, err)
    assert(err == nil, err)
    done = true
  end))
  assert(nvim.wait(6000, function()
    return done
  end))
  session.emitter:emit({
    type = "user_chunk",
    session_id = session:inspect().id,
    data = { content = { type = "image" } },
  })
  session.emitter:emit({
    type = "tool_call_started",
    session_id = session:inspect().id,
    data = {
      toolCallId = "tool",
      title = "Inspect",
      status = "completed",
      rawInput = { command = "ls", private = "SECRET FIELD" },
      rawOutput = { stdout = string.rep("x", 9000) },
    },
  })
  session.emitter:emit({
    type = "chunk",
    session_id = session:inspect().id,
    data = { content = { type = "text", text = "before question" } },
  })
  session.emitter:emit({
    type = "interaction_requested",
    session_id = session:inspect().id,
    data = {
      kind = "question",
      request_id = 7,
      questions = {
        { question = "Choose?", options = { { label = "Yes", description = "Proceed" } }, multi_select = false },
      },
    },
  })
  session.emitter:emit({
    type = "chunk",
    session_id = session:inspect().id,
    data = { content = { type = "text", text = "after question" } },
  })
  for _, text in ipairs({ "[res", "ource] SECRET REPLAY RESOURCE" }) do
    session.emitter:emit({
      type = "user_chunk",
      session_id = session:inspect().id,
      data = { content = { type = "text", text = text } },
    })
  end
  local request = { before = 0, limit = 50, revision = 0 }
  local page
  session:read_history(request, function(result, err)
    assert(err == nil, err)
    page = result
  end)
  request.limit = 0
  assert(nvim.wait(6000, function()
    return page ~= nil
  end))
  MiniTest.expect.equality(page.complete, false)
  MiniTest.expect.equality(page.source, {
    replay = "not_applicable",
    archive = "unavailable",
    redaction = "present",
    missing = true,
    truncated = true,
  })
  local encoded = nvim.json.encode(page)
  MiniTest.expect.equality(encoded:find("SECRET", 1, true), nil)
  local kinds = {}
  for index, entry in ipairs(page.entries) do
    kinds[entry.kind] = true
    if entry.kind == "question" then
      MiniTest.expect.equality(page.entries[index - 1].text, "before question")
      MiniTest.expect.equality(page.entries[index + 1].text, "after question")
    end
  end
  MiniTest.expect.equality(kinds.tool, true)
  MiniTest.expect.equality(kinds.question, true)

  for index = 1, 10 do
    session.emitter:emit({
      type = "prompt_dispatched",
      session_id = session:inspect().id,
      data = { turn_id = "large-" .. index, prompt = { { type = "text", text = string.rep("\1", 8192) } } },
    })
  end
  local bounded
  session:read_history({ before = 0, limit = 50, revision = 0 }, function(result)
    bounded = result
  end)
  assert(nvim.wait(6000, function()
    return bounded ~= nil
  end))
  MiniTest.expect.equality(#nvim.json.encode(bounded) < 262144, true)
  MiniTest.expect.equality(#bounded.entries < 10, true)
  MiniTest.expect.equality(bounded.next_before > 0, true)

  local called, scheduled = false, false
  local cancel = session:read_history({ before = 0, limit = 1, revision = 0 }, function()
    called = true
  end)
  cancel()
  nvim.schedule(function()
    scheduled = true
  end)
  assert(nvim.wait(6000, function()
    return scheduled
  end))
  MiniTest.expect.equality(called, false)
end

T["reports replay gaps and exclusions delivered by a mock ACP peer"] = function()
  local directory = nvim.fn.tempname()
  local api = assert(Session.new({
    mock = {
      provider = "test-service",
      command = nvim.v.progpath,
      args = {
        "--headless",
        "--noplugin",
        "-u",
        "tests/mock/init.lua",
        "-c",
        "luafile tests/fixtures/history_replay.lua",
      },
    },
  }, nil, { usage_directory = directory }))
  MiniTest.finally(function()
    assert(api:dispose())
    local finished = false
    api:flush_recording(function()
      finished = true
    end)
    assert(nvim.wait(6000, function()
      return finished
    end))
    nvim.fn.delete(directory, "rf")
  end)
  local session = assert(api:load_session("mock", "replay-source"))
  assert(nvim.wait(6000, function()
    return session:inspect().status == "ready"
  end))
  local page
  session:read_history({ before = 0, limit = 50, revision = 0 }, function(result, err)
    assert(err == nil, err)
    page = result
  end)
  assert(nvim.wait(6000, function()
    return page ~= nil
  end))
  MiniTest.expect.equality(page.source, {
    replay = "unverified",
    archive = "unavailable",
    redaction = "present",
    missing = true,
    truncated = true,
  })
  MiniTest.expect.equality(page.complete, false)
  local json = nvim.json.encode(page)
  MiniTest.expect.equality(json:find("PRIVATE", 1, true), nil)
  MiniTest.expect.equality(json:find("Visible answer", 1, true) ~= nil, true)
end

return T
