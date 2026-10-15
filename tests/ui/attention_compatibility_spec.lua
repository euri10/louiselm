local MiniTest = require("mini.test")
local Attention = require("louiselm.ui.attention")
local AttentionPaste = require("louiselm.ui.attention_paste")
local AttentionClient = require("louiselm.workflow.attention_client")
local Chat = require("louiselm.ui.chat")
local Session = require("louiselm.session")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local T = MiniTest.new_set()

local function wait_for(predicate)
  assert(nvim.wait(6000, predicate, 5), "Attention compatibility fixture did not settle")
end

local function companion()
  -- Pre-interface snapshot shape: 98b6f05^:tests/workflow/attention_client_spec.lua.
  -- The original louiselm-26rs6 editor notification/order was not captured;
  -- repeated input and Session actions below are a deterministic reproduction.
  local fixture = { pipes = {}, mutations = {}, errors = {}, pending = 0 }
  local original_connect = AttentionClient.connect
  local handles = {}
  local capability_path = nvim.fn.tempname()
  nvim.fn.writefile({ "11111111-2222-4333-8444-555555555555" }, capability_path, "b")
  fixture.capability_path = capability_path
  MiniTest.finally(function()
    AttentionClient.connect = original_connect
    for _, handle in ipairs(handles) do
      if not handle:is_closing() then
        handle:close()
      end
    end
    nvim.fn.delete(capability_path)
  end)
  local function deliver(callback)
    fixture.pending = fixture.pending + 1
    local handle
    handle = assert(nvim.uv.new_async(function()
      assert(nvim.in_fast_event())
      callback()
      fixture.pending = fixture.pending - 1
      handle:close()
    end))
    handles[#handles + 1] = handle
    handle:send()
  end
  ---@diagnostic disable-next-line: duplicate-set-field -- Inject only the documented pipe seam; retain protocol validation and scheduling.
  AttentionClient.connect = function(path, callback, options)
    options.pipe_factory = function()
      local pipe = { closing = false }
      fixture.pipes[#fixture.pipes + 1] = pipe
      function pipe:connect(_, connected)
        deliver(connected)
      end
      function pipe:read_start(read)
        self.read = read
        deliver(function()
          read(nil, nvim.json.encode({
            type = "snapshot",
            service = fixture.metadata,
            snapshot = { generation = 0, items = {} },
          }) .. "\n")
        end)
      end
      function pipe:read_stop() end
      function pipe:is_closing()
        return self.closing
      end
      function pipe:close()
        self.closing = true
      end
      function pipe:write(frame)
        local request = nvim.json.decode(frame)
        fixture.mutations[#fixture.mutations + 1] = request
        deliver(function()
          self.read(nil, nvim.json.encode({
            type = "mutation_result",
            request_id = request.request_id,
            snapshot = { generation = #fixture.mutations, items = {} },
          }) .. "\n")
        end)
      end
      return pipe
    end
    return original_connect(path, callback, options)
  end
  function fixture:observe(attention)
    attention.capability_path = self.capability_path
    attention.on_error = function(message)
      assert(not nvim.in_fast_event())
      self.errors[#self.errors + 1] = message
    end
  end
  function fixture:reject(attention)
    attention:activity()
    wait_for(function()
      local pipe = self.pipes[#self.pipes]
      return pipe ~= nil and pipe.closing and not attention.connecting and self.pending == 0
    end)
  end
  return fixture
end

T["one actionable incompatibility warning while ordinary chat turns remain usable"] = function()
  local f = companion()
  local directory = nvim.fn.tempname()
  local api = assert(Session.new({
    mock = {
      provider = "test-service",
      command = nvim.v.progpath,
      args = {
        "--headless",
        "--noplugin",
        "-u",
        nvim.fn.getcwd() .. "/tests/mock/init.lua",
        "-c",
        "lua require('louiselm.dev.mock_agent').run()",
      },
      env = { LOUISELM_MOCK_MODE = "static", LOUISELM_MOCK_RESPONSE = "chat still works" },
    },
  }, nil, { usage_directory = directory }))
  local paste = AttentionPaste.new()
  local chat
  MiniTest.finally(function()
    if chat ~= nil then
      chat:dispose()
    end
    paste:dispose()
    assert(api:dispose())
    local settled, failure = false, nil
    api:flush_recording(function(err)
      settled, failure = true, err
    end)
    wait_for(function()
      return settled
    end)
    assert(failure == nil, failure and failure.message)
    nvim.fn.delete(directory, "rf")
  end)
  chat = assert(Chat.new(api, { attention = true, attention_paste = paste, start_insert_on_switch = false }))
  f:observe(chat.attention)
  local session = assert(api:create_session("mock", { cwd = nvim.fn.getcwd() }))
  wait_for(function()
    return session:inspect().status == "ready"
  end)
  assert(chat:attach(session))
  f:reject(chat.attention)
  for turn = 1, 3 do
    assert(chat:submit("ordinary prompt " .. turn))
    wait_for(function()
      local state = session:inspect()
      return state.status == "ready" and state.current_turn == turn and not state.recording_pending
    end)
    nvim.api.nvim_exec_autocmds("FocusGained", {})
    f:reject(chat.attention)
  end
  local text = table.concat(nvim.api.nvim_buf_get_lines(chat:buffer(), 0, -1, false), "\n")
  assert(text:find("chat still works", 1, true))
  MiniTest.expect.equality(f.mutations, {})
  MiniTest.expect.equality(f.errors, {
    "unidentified capture service: incompatible attention interface; install a capture release supporting attention interface 1 (see docs/releases.md)",
  })
end

T["recovery replays queued conditions and clears then rearms warnings"] = function()
  local f = companion()
  local paste = AttentionPaste.new()
  local timers = {}
  local attention = assert(Attention.new({
    paste_dispatcher = paste,
    schedule = function(_, callback)
      timers[#timers + 1] = callback
    end,
  }))
  MiniTest.finally(function()
    attention:dispose()
    paste:dispose()
  end)
  f:observe(attention)
  f:reject(attention)
  attention:permission_required({ acp_session_id = "waiting", current_turn = 1 }, { request_id = 7 })
  attention:permission_resolved("waiting", 7)
  attention:turn_done({ agent = "mock", acp_session_id = "unseen", current_turn = 1, status = "ready" }, false)
  f:reject(attention)
  MiniTest.expect.equality(#f.errors, 1)
  MiniTest.expect.equality(f.mutations, {})

  f.metadata = { component = "capture", version = "0.9.2", interfaces = { attention = 1 } }
  attention:activity()
  wait_for(function()
    return #f.mutations == 5 and f.pending == 0 and #timers > 0
  end)
  local operations = nvim.tbl_map(function(request)
    return { request.type, request.attention and request.attention.kind or request.kind or request.key.kind }
  end, f.mutations)
  MiniTest.expect.equality(operations, {
    { "upsert", "permission_required" },
    { "clear", "permission_required" },
    { "clear_session_kind", "turn_ready" },
    { "clear_session_kind", "session_failed" },
    { "upsert", "turn_ready" },
  })
  for _, timer in ipairs(timers) do
    timer()
  end
  wait_for(function()
    return #f.mutations == 6 and f.pending == 0
  end)
  MiniTest.expect.equality(f.mutations[6].type, "set_eligible")
  MiniTest.expect.equality(f.mutations[6].key.subject_id, "unseen")

  -- The same refusal after successful communication must be actionable again.
  local pipe = f.pipes[#f.pipes]
  pipe.read(nil, nvim.json.encode({ type = "snapshot", snapshot = { generation = 6, items = {} } }) .. "\n")
  wait_for(function()
    return pipe.closing
  end)
  MiniTest.expect.equality(#f.errors, 2)
  f.metadata = nil
  f:reject(attention)
  f:reject(attention)
  MiniTest.expect.equality(#f.errors, 2)
  MiniTest.expect.equality(f.errors[2], f.errors[1])

  -- Different failures still reach the operator before another recovery.
  f.metadata = { component = "capture", version = "0.9.2", interfaces = { attention = 2 } }
  f:reject(attention)
  f:reject(attention)
  MiniTest.expect.equality(#f.errors, 3)
  MiniTest.expect.equality(
    f.errors[3],
    "louiselm-capture 0.9.2: incompatible attention interface; install a capture release supporting attention interface 1 (see docs/releases.md)"
  )
end

return T
