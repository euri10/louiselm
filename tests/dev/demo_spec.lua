local MiniTest = require("mini.test")
local Demo = require("louiselm.dev.demo")

local T = MiniTest.new_set()

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local function fixture()
  local root = nvim.fn.tempname()
  local path = root .. "/lua/calculator.lua"
  nvim.fn.mkdir(root .. "/lua", "p")
  nvim.fn.writefile({
    "local M = {}",
    "",
    "function M.add(left, right)",
    "  return left - right",
    "end",
    "",
    "return M",
  }, path)
  return root, path
end

local function scheduler()
  local queued = {}
  return queued, function(_, callback)
    queued[#queued + 1] = callback
  end
end

local function drain(queued)
  while #queued > 0 do
    table.remove(queued, 1)()
  end
end

T["demo API"] = MiniTest.new_set()

T["demo API"]["accepts any prompt through an asynchronous, explicitly scripted file-edit turn"] = function()
  local root, path = fixture()
  nvim.cmd.edit(nvim.fn.fnameescape(path))
  local buffer = nvim.api.nvim_get_current_buf()
  local queued, schedule = scheduler()
  local api = assert(Demo.new({ project_root = root, schedule = schedule }))
  local session = assert(api:create_session("your-codex-here", { cwd = root }))
  local events = {}
  local permission
  session:on(function(event)
    events[#events + 1] = event
    if event.type == "permission_requested" then
      permission = event
    end
  end)

  assert(session:prompt({
    { type = "resource_link", uri = "file://" .. path, name = "calculator.lua" },
    { type = "text", text = "surprise me with a fix" },
  }))
  MiniTest.expect.equality(session:inspect().status, "prompting")
  MiniTest.expect.equality(#events, 1)

  drain(queued)
  MiniTest.expect.equality(session:inspect().status, "waiting_permission")
  MiniTest.expect.equality(events[2].type, "chunk")
  MiniTest.expect.equality(events[2].data.content.text:find("scripted demo", 1, true) ~= nil, true)
  MiniTest.expect.equality(events[3].type, "tool_call_started")
  MiniTest.expect.equality(permission.data.operation, {
    kind = "file_edit",
    path = path,
    diff = "@@ -4 +4 @@\n-  return left - right\n+  return left + right\n",
  })

  assert(permission.respond({ outcome = { outcome = "selected", optionId = "allow-once" } }))
  MiniTest.expect.equality(nvim.fn.readfile(path)[4], "  return left - right")
  drain(queued)
  MiniTest.expect.equality(nvim.fn.readfile(path)[4], "  return left + right")
  MiniTest.expect.equality(nvim.api.nvim_buf_get_lines(buffer, 3, 4, false)[1], "  return left + right")
  MiniTest.expect.equality(session:inspect().status, "ready")
  MiniTest.expect.equality(events[#events - 1].type, "state_changed")
  MiniTest.expect.equality(events[#events].type, "turn_done")
  local responded, response_error = permission.respond({ outcome = { outcome = "cancelled" } })
  MiniTest.expect.equality({ responded, response_error }, { false, "permission request was already answered" })

  assert(api:dispose())
  nvim.api.nvim_buf_delete(buffer, { force = true })
  nvim.fn.delete(root, "rf")
end

T["demo API"]["rejects without editing and permits a later retry"] = function()
  local root, path = fixture()
  local queued, schedule = scheduler()
  local api = assert(Demo.new({ project_root = root, schedule = schedule }))
  local session = assert(api:create_session("your-codex-here"))
  local permissions = {}
  session:on(function(event)
    if event.type == "permission_requested" then
      permissions[#permissions + 1] = event
    end
  end)

  assert(session:prompt("fix it"))
  drain(queued)
  assert(permissions[1].respond({ outcome = { outcome = "selected", optionId = "reject-once" } }))
  drain(queued)
  MiniTest.expect.equality(nvim.fn.readfile(path)[4], "  return left - right")
  MiniTest.expect.equality(session:inspect().status, "ready")

  assert(session:prompt("try again"))
  drain(queued)
  MiniTest.expect.equality(#permissions, 2)

  assert(api:dispose())
  nvim.fn.delete(root, "rf")
end

T["demo API"]["drops scheduled Agent work after disposal"] = function()
  local root = fixture()
  local queued, schedule = scheduler()
  local api = assert(Demo.new({ project_root = root, schedule = schedule }))
  local session = assert(api:create_session("your-codex-here"))
  local events = {}
  session:on(function(event)
    events[#events + 1] = event.type
  end)

  assert(session:prompt("fix it"))
  assert(session:dispose())
  drain(queued)

  MiniTest.expect.equality(events, { "state_changed", "state_changed" })
  MiniTest.expect.equality(session:inspect().status, "disposed")
  assert(api:dispose())
  nvim.fn.delete(root, "rf")
end

T["demo API"]["discovers and asynchronously replays one seeded Session"] = function()
  local root = fixture()
  local queued, schedule = scheduler()
  local api = assert(Demo.new({ project_root = root, schedule = schedule }))
  local discovered
  assert(api:discover_sessions({ cwd = root }, function(sessions, errors)
    discovered = { sessions = sessions, errors = errors }
  end))
  MiniTest.expect.equality(discovered, nil)
  drain(queued)
  MiniTest.expect.equality(#discovered.sessions, 1)
  MiniTest.expect.equality(discovered.errors, {})
  MiniTest.expect.equality(discovered.sessions[1].agent, "your-codex-here")
  MiniTest.expect.equality(discovered.sessions[1].cwd, root)

  local ready
  local session = assert(
    api:load_session(
      discovered.sessions[1].agent,
      discovered.sessions[1].session_id,
      { cwd = root },
      function(value, error_message)
        ready = { session = value, error = error_message }
      end
    )
  )
  local events = {}
  session:on(function(event)
    events[#events + 1] = event
  end)
  MiniTest.expect.equality(session:inspect().source, "loaded")
  MiniTest.expect.equality(session:inspect().status, "starting")
  drain(queued)

  MiniTest.expect.equality(session:inspect().status, "ready")
  MiniTest.expect.equality(ready, { session = session, error = nil })
  MiniTest.expect.equality({ events[1].type, events[2].type, events[3].type, events[4].type }, {
    "user_chunk",
    "chunk",
    "user_chunk",
    "chunk",
  })
  MiniTest.expect.equality(events[2].data.content.text:find("scripted demo", 1, true) ~= nil, true)
  MiniTest.expect.equality(events[#events].type, "state_changed")

  assert(api:dispose())
  nvim.fn.delete(root, "rf")
end

T["demo API"]["publishes a simulated reached limit through the normal observer"] = function()
  local root = fixture()
  local queued, schedule = scheduler()
  local api = assert(Demo.new({ project_root = root, schedule = schedule }))
  local observed = {}
  api:on_agent_limits(function(state)
    observed[#observed + 1] = state
  end)

  MiniTest.expect.equality(api:inspect_agent_limits("your-codex-here").status, "not_observed")
  assert(api:simulate_limit("your-codex-here"))
  MiniTest.expect.equality(observed, {})
  drain(queued)

  local state = api:inspect_agent_limits("your-codex-here")
  MiniTest.expect.equality(state.status, "fresh")
  MiniTest.expect.equality(state.snapshot.buckets[1].reached_type, "rate_limit")
  MiniTest.expect.equality(observed, { state })

  assert(api:dispose())
  nvim.fn.delete(root, "rf")
end

T["demo API"]["answers reviewed Handoff content without proposing a file edit"] = function()
  local root = fixture()
  local queued, schedule = scheduler()
  local api = assert(Demo.new({ project_root = root, schedule = schedule }))
  local session = assert(api:create_session("your-claude-here"))
  local events = {}
  session:on(function(event)
    events[#events + 1] = event
  end)

  assert(session:prompt({
    { type = "text", text = "## Handoff\n\n- takeover task: verify the calculator fix" },
    {
      type = "resource",
      resource = { uri = "louiselm://handoff/source", mimeType = "text/markdown", text = "## Context" },
    },
  }))
  drain(queued)

  local types = {}
  for _, event in ipairs(events) do
    types[event.type] = true
  end
  MiniTest.expect.equality(types.permission_requested, nil)
  MiniTest.expect.equality(events[2].type, "chunk")
  MiniTest.expect.equality(events[2].data.content.text:find("Handoff received", 1, true) ~= nil, true)
  MiniTest.expect.equality(session:inspect().status, "ready")

  assert(api:dispose())
  nvim.fn.delete(root, "rf")
end

return T
