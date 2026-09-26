local MiniTest = require("mini.test")
local Chat = require("louiselm.ui.chat")
local Provenance = require("louiselm.output_provenance")

local T = MiniTest.new_set()

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local function fake_session(id, agent)
  local listeners = {}
  local session = {
    state = {
      id = id,
      agent = agent,
      acp_session_id = id .. "-acp",
      status = "ready",
      current_turn = 0,
      config_options = {},
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

  function session:prompt(prompt)
    self.prompts[#self.prompts + 1] = prompt
    return #self.prompts
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
    inspect_agent_limits = function(_, agent)
      return { agent = agent, status = "not_observed" }
    end,
    refresh_agent_limits = function(_, agent, callback)
      callback({ agent = agent, status = "not_observed" })
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

local function read_file(path)
  return table.concat(nvim.fn.readfile(path), "\n")
end

local function read_bytes(path)
  local file = assert(nvim.uv.fs_open(path, "r", 0))
  local stat = assert(nvim.uv.fs_fstat(file))
  local content = assert(nvim.uv.fs_read(file, stat.size, 0))
  assert(nvim.uv.fs_close(file))
  return content
end

local function export(chat, session_id, path, fetch)
  local done, written_path, write_error = false, nil, nil
  local cancel, error_message = chat:to_markdown(session_id, path, function(value, err)
    written_path, write_error, done = value, err, true
  end, fetch)
  if cancel == nil then
    return nil, error_message
  end
  assert(nvim.wait(1000, function()
    return done
  end))
  return written_path, write_error
end

T["to_markdown"] = MiniTest.new_set({
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

T["to_markdown"]["exports the current session's full transcript, in order, with untruncated tool output"] = function()
  local session = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(session))

  assert(chat:submit("run the tests"))
  session:emit({
    type = "tool_call_started",
    session_id = "session-1",
    data = {
      toolCallId = "tool-1",
      title = "Run tests",
      status = "in_progress",
      rawInput = { command = { "make", "test" } },
    },
  })
  local long_output = string.rep("ok\n", 200)
  session:emit({
    type = "tool_call_finished",
    session_id = "session-1",
    data = { toolCallId = "tool-1", status = "completed", rawOutput = { stdout = long_output } },
  })
  session:emit({
    type = "chunk",
    session_id = "session-1",
    data = { content = { type = "text", text = "All green." } },
  })

  local path = nvim.fn.tempname() .. ".md"
  local written_path, write_error = export(chat, nil, path)

  MiniTest.expect.equality(write_error, nil)
  MiniTest.expect.equality(written_path, path)
  local content = read_file(path)
  MiniTest.expect.equality(content:find("## User", 1, true) ~= nil, true)
  MiniTest.expect.equality(content:find("run the tests", 1, true) ~= nil, true)
  MiniTest.expect.equality(content:find("<sub>**Run tests** — completed</sub>", 1, true) ~= nil, true)
  MiniTest.expect.equality(content:find('"make"', 1, true) ~= nil, true)
  local ok_count = 0
  for _ in content:gmatch("ok") do
    ok_count = ok_count + 1
  end
  MiniTest.expect.equality(ok_count, 200)
  MiniTest.expect.equality(content:find("## Assistant", 1, true) ~= nil, true)
  MiniTest.expect.equality(content:find("All green.", 1, true) ~= nil, true)
  local user_pos = assert(content:find("## User", 1, true))
  local tool_pos = assert(content:find("## Tool", 1, true))
  local assistant_pos = assert(content:find("## Assistant", 1, true))
  MiniTest.expect.equality(user_pos < tool_pos, true)
  MiniTest.expect.equality(tool_pos < assistant_pos, true)

  nvim.fn.delete(path)
  chat:dispose()
end

T["to_markdown"]["exports a specific attached session by id, independent of the current session"] = function()
  local first = fake_session("session-1", "claude")
  local second = fake_session("session-2", "codex")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  assert(chat:submit("first message"))
  assert(chat:attach(second))
  assert(chat:submit("second message"))

  local path = nvim.fn.tempname() .. ".md"
  local written_path, write_error = export(chat, "session-1", path)

  MiniTest.expect.equality(write_error, nil)
  MiniTest.expect.equality(written_path, path)
  local content = read_file(path)
  MiniTest.expect.equality(content:find("first message", 1, true) ~= nil, true)
  MiniTest.expect.equality(content:find("second message", 1, true) ~= nil, false)
  MiniTest.expect.equality(content:find("session: session-1", 1, true) ~= nil, true)

  nvim.fn.delete(path)
  chat:dispose()
end

T["to_markdown"]["captures replayed user_chunk events from a resumed session alongside the agent's replies"] = function()
  local session = fake_session("session-1", "codex")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(session))

  session:emit({
    type = "user_chunk",
    session_id = "session-1",
    data = { content = { type = "text", text = "what did we decide last time" } },
  })
  session:emit({
    type = "chunk",
    session_id = "session-1",
    data = { content = { type = "text", text = "we decided to ship it" } },
  })

  local path = nvim.fn.tempname() .. ".md"
  assert(export(chat, nil, path))

  local content = read_file(path)
  local user_pos = assert(content:find("what did we decide last time", 1, true))
  local assistant_pos = assert(content:find("we decided to ship it", 1, true))
  MiniTest.expect.equality(user_pos < assistant_pos, true)

  nvim.fn.delete(path)
  chat:dispose()
end

T["to_markdown"]["defaults to a generated path in the current working directory when no path is given"] = function()
  local session = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(session))
  assert(chat:submit("hello"))

  local written_path, write_error = assert(export(chat))

  MiniTest.expect.equality(write_error, nil)
  MiniTest.expect.equality(written_path:sub(1, #nvim.fn.getcwd()), nvim.fn.getcwd())
  MiniTest.expect.equality(written_path:sub(-3), ".md")
  MiniTest.expect.equality(nvim.fn.filereadable(written_path), 1)

  nvim.fn.delete(written_path)
  chat:dispose()
end

T["to_markdown"]["reports an error when no chat session is open"] = function()
  local chat = assert(Chat.new(fake_api()))

  local written_path, write_error = export(chat)

  MiniTest.expect.equality(written_path, nil)
  MiniTest.expect.equality(write_error, "no chat session is open")
end

T["to_markdown"]["reports an error when the given session id is not attached"] = function()
  local session = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(session))

  local written_path, write_error = export(chat, "does-not-exist", nvim.fn.tempname() .. ".md")

  MiniTest.expect.equality(written_path, nil)
  MiniTest.expect.equality(write_error, "session is not attached")
  chat:dispose()
end

T["to_markdown"]["reports a clean error instead of crashing when the destination directory does not exist"] = function()
  local session = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(session))
  assert(chat:submit("hello"))

  local path = nvim.fn.tempname() .. "/nope/qa-export.md"
  local written_path, write_error = export(chat, nil, path)

  MiniTest.expect.equality(written_path, nil)
  MiniTest.expect.equality(write_error, "could not write markdown file: " .. path)
  chat:dispose()
end

T["to_markdown"]["reports an error once the chat UI is disposed"] = function()
  local session = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(session))
  chat:dispose()

  local written_path, write_error = export(chat)

  MiniTest.expect.equality(written_path, nil)
  MiniTest.expect.equality(write_error, "chat UI is disposed")
end

T["to_markdown"]["adds current broker taint metadata without changing transcript content"] = function()
  local session = fake_session("session-1", "codex")
  session.state.broker_session_id = "broker-session"
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(session))
  assert(chat:submit("secret original prompt"))
  local path = nvim.fn.tempname() .. ".md"
  local result, failure
  local cancel = assert(chat:to_markdown(nil, path, function(value, err)
    result, failure = value, err
  end, function(_, callback)
    nvim.schedule(function()
      callback(nvim.json.encode({
        record = { launch = { session_id = "broker-session" } },
        quarantined = true,
        output_provenance = {
          schema = "louiselm.workspace.output-provenance/1",
          code = "session_output_tainted",
          taint_digest = "sha256:" .. string.rep("e", 64),
          clean_review_refs = {},
        },
      }))
    end)
    return function() end
  end))
  assert(nvim.wait(1000, function()
    return result ~= nil or failure ~= nil
  end))
  MiniTest.expect.equality(type(cancel), "function")
  MiniTest.expect.equality(failure, nil)
  MiniTest.expect.equality(result, path)
  local marked = read_bytes(path)
  MiniTest.expect.equality(Provenance.inspect_markdown(marked).code, "session_output_tainted")
  MiniTest.expect.equality(marked:find("secret original prompt", 1, true) ~= nil, true)
  MiniTest.expect.equality(marked:find("broker-session", 1, true), nil)
  MiniTest.expect.equality(nvim.uv.fs_stat(path).mode % 512, 384)
  nvim.fn.delete(path)
  chat:dispose()
end

T["to_markdown"]["cancellation during broker inspection leaves no transcript file"] = function()
  local session = fake_session("session-1", "codex")
  session.state.broker_session_id = "broker-session"
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(session))
  local path = nvim.fn.tempname() .. ".md"
  local pending, called, cancelled = nil, 0, false
  local cancel = assert(chat:to_markdown(nil, path, function(value, err)
    called = called + 1
    MiniTest.expect.equality(value, nil)
    MiniTest.expect.equality(err, "transcript export cancelled")
  end, function(_, callback)
    pending = callback
    return function()
      cancelled = true
    end
  end))
  cancel()
  local reply = assert(pending)
  reply(nil)
  assert(nvim.wait(1000, function()
    return called == 1
  end))
  MiniTest.expect.equality(cancelled, true)
  MiniTest.expect.equality(nvim.uv.fs_stat(path), nil)
  chat:dispose()
end

return T
