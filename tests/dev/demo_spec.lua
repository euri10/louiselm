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

local function wait_until(queued, predicate)
  assert(nvim.wait(1000, function()
    drain(queued)
    return predicate()
  end))
end

T["demo API"] = MiniTest.new_set()

local function start_demo(schedule)
  local root = fixture()
  local cwd = nvim.fn.getcwd()
  local commands = nvim.api.nvim_get_commands({ builtin = false })
  local runtime
  MiniTest.finally(function()
    if runtime ~= nil then
      runtime.chat:dispose()
      runtime.api:dispose()
    end
    for name in pairs(nvim.api.nvim_get_commands({ builtin = false })) do
      if commands[name] == nil then
        nvim.api.nvim_del_user_command(name)
      end
    end
    -- Demo startup replaces the production commands installed by minimal_init.
    require("louiselm.ui.chat.command").register()
    nvim.api.nvim_set_current_dir(cwd)
    for _, buffer in ipairs(nvim.api.nvim_list_bufs()) do
      if nvim.api.nvim_buf_get_name(buffer):sub(1, #root) == root then
        nvim.api.nvim_buf_delete(buffer, { force = true })
      end
    end
    nvim.fn.delete(root, "rf")
  end)
  runtime = assert(Demo.start({ project_root = root, schedule = schedule }))
  return runtime
end

T["demo API"]["startup opens an idle chat before any prompt or diff"] = function()
  local runtime = start_demo()
  MiniTest.expect.equality(runtime.session:inspect().status, "ready")
  MiniTest.expect.equality(nvim.api.nvim_buf_get_name(0):match("^louiselm://demo%-") ~= nil, true)
  local content = table.concat(nvim.api.nvim_buf_get_lines(0, 0, -1, false), "\n")
  MiniTest.expect.equality(content:find("calculator.lua", 1, true) ~= nil, true)
  assert(runtime.chat:submit("fix the calculator"))
  local view = runtime.chat.views[runtime.session:inspect().id]
  assert(nvim.wait(1000, function()
    return #view.transcript:snapshot() > 0
  end))
  MiniTest.expect.equality(view.transcript:snapshot()[1], { kind = "user", text = "fix the calculator" })
end

T["demo API"]["real Chat keeps busy submissions and drops queued UI work after disposal"] = function()
  local queued, schedule = scheduler()
  local runtime = start_demo(schedule)
  local session, chat = runtime.session, runtime.chat
  local permissions = {}
  session:on(function(event)
    if event.type == "permission_requested" then
      permissions[#permissions + 1] = event
    end
  end)
  assert(chat:submit("first turn"))
  assert(nvim.wait(1000, function()
    drain(queued)
    return #permissions == 1
  end))
  assert(chat:submit("waiting one"))
  assert(nvim.wait(1000, function()
    drain(queued)
    return #session:queue_inspect().entries == 1 and not session:queue_inspect().pending
  end))
  assert(chat:submit("waiting two"))
  assert(nvim.wait(1000, function()
    drain(queued)
    return #session:queue_inspect().entries == 2 and not session:queue_inspect().pending
  end))
  local entries = session:queue_inspect().entries
  MiniTest.expect.equality({ entries[1].display_text, entries[2].display_text }, { "waiting one", "waiting two" })
  assert(permissions[1].respond({ outcome = { outcome = "selected", optionId = "reject-once" } }))
  assert(nvim.wait(1000, function()
    drain(queued)
    return #permissions == 2
  end))
  local view = chat.views[session:inspect().id]
  assert(nvim.wait(1000, function()
    local content = table.concat(nvim.api.nvim_buf_get_lines(view.renderer.buffer, 0, -1, false), "\n")
    return content:find("waiting one", 1, true) ~= nil
  end))
  MiniTest.expect.equality(session:queue_inspect().entries[1].display_text, "waiting two")
  assert(permissions[2].respond({ outcome = { outcome = "selected", optionId = "reject-once" } }))
  chat:dispose()
  runtime.api:dispose()
  drain(queued)
  nvim.wait(20)
  MiniTest.expect.equality(session:inspect().status, "disposed")
end

T["demo API"]["full queue rejection completes asynchronously"] = function()
  local root = fixture()
  local queued, schedule = scheduler()
  local api = assert(Demo.new({ project_root = root, schedule = schedule }))
  MiniTest.finally(function()
    api:dispose()
    nvim.fn.delete(root, "rf")
  end)
  local session = assert(api:create_session("your-codex-here"))
  assert(session:prompt("active"))
  drain(queued)
  for index = 1, 20 do
    local accepted
    assert(session:enqueue("waiting " .. index, function(id, err)
      accepted = { id = id, error = err }
    end))
    wait_until(queued, function()
      return accepted ~= nil
    end)
    assert(accepted.id, accepted.error)
  end
  local result
  assert(session:enqueue("overflow", function(id, err)
    result = { id = id, error = err }
  end))
  MiniTest.expect.equality(result, nil)
  wait_until(queued, function()
    return result ~= nil
  end)
  MiniTest.expect.equality(result, { error = "Session queue is full (20 waiting instructions)" })
end

T["demo API"]["does not dispatch after an admission listener disposes the Session"] = function()
  local root = fixture()
  local queued, schedule = scheduler()
  local api = assert(Demo.new({ project_root = root, schedule = schedule }))
  MiniTest.finally(function()
    api:dispose()
    nvim.fn.delete(root, "rf")
  end)
  local session = assert(api:create_session("your-codex-here"))
  local events, completed = {}, false
  session:on(function(event)
    events[#events + 1] = event.type
    if event.type == "admission_settled" then
      session:dispose()
    end
  end)
  assert(session:prompt("fix", function()
    completed = true
  end))
  drain(queued)
  MiniTest.expect.equality(events, { "state_changed", "admission_settled", "state_changed" })
  MiniTest.expect.equality(completed, false)
end

T["demo API"]["disposal during a scripted response does not revive the Session"] = function()
  local root = fixture()
  local queued, schedule = scheduler()
  local api = assert(Demo.new({ project_root = root, schedule = schedule }))
  MiniTest.finally(function()
    api:dispose()
    nvim.fn.delete(root, "rf")
  end)
  local session = assert(api:create_session("your-codex-here"))
  local events = {}
  session:on(function(event)
    events[#events + 1] = event.type
    if event.type == "chunk" then
      session:dispose()
    end
  end)
  assert(session:prompt("fix"))
  drain(queued)
  MiniTest.expect.equality(
    events,
    { "state_changed", "admission_settled", "prompt_dispatched", "chunk", "state_changed" }
  )
  MiniTest.expect.equality(session:inspect().status, "disposed")
end

T["demo API"]["disposal during seeded replay does not revive the Session"] = function()
  local root = fixture()
  local queued, schedule = scheduler()
  local api = assert(Demo.new({ project_root = root, schedule = schedule }))
  MiniTest.finally(function()
    api:dispose()
    nvim.fn.delete(root, "rf")
  end)
  local ready = false
  local session = assert(api:load_session("your-codex-here", "scripted-resume-1", nil, function()
    ready = true
  end))
  session:on(function(event)
    if event.type == "user_chunk" then
      session:dispose()
    end
  end)
  drain(queued)
  MiniTest.expect.equality(session:inspect().status, "disposed")
  MiniTest.expect.equality(ready, false)
end

T["demo API"]["cancellation during a scripted response completes the turn once"] = function()
  local root = fixture()
  local queued, schedule = scheduler()
  local api = assert(Demo.new({ project_root = root, schedule = schedule }))
  MiniTest.finally(function()
    api:dispose()
    nvim.fn.delete(root, "rf")
  end)
  local session = assert(api:create_session("your-codex-here"))
  local outcomes, completions, requested, tools = {}, {}, false, 0
  session:on(function(event)
    if event.type == "chunk" and not requested then
      requested = true
      assert(session:cancel())
    elseif event.type == "turn_done" then
      outcomes[#outcomes + 1] = event.data.stopReason
    elseif event.type == "tool_call_finished" then
      tools = tools + 1
    end
  end)
  assert(session:prompt("## Handoff", function(result)
    completions[#completions + 1] = result.stopReason
  end))
  drain(queued)
  MiniTest.expect.equality(outcomes, { "cancelled" })
  MiniTest.expect.equality(completions, { "cancelled" })
  MiniTest.expect.equality(tools, 0)
end

T["demo API"]["cancellation pauses waiting work until reviewed and continued"] = function()
  local root = fixture()
  local queued, schedule = scheduler()
  local api = assert(Demo.new({ project_root = root, schedule = schedule }))
  MiniTest.finally(function()
    api:dispose()
    nvim.fn.delete(root, "rf")
  end)
  local session = assert(api:create_session("your-codex-here"))
  local permissions, result = {}, nil
  session:on(function(event)
    if event.type == "permission_requested" then
      permissions[#permissions + 1] = event
    end
  end)
  assert(session:prompt("active", function(value, err)
    result = { value, err }
  end))
  drain(queued)
  local accepted
  assert(session:enqueue("waiting", function(id, err)
    accepted = { id = id, error = err }
  end))
  wait_until(queued, function()
    return accepted ~= nil
  end)
  assert(accepted.id, accepted.error)
  assert(session:cancel())
  wait_until(queued, function()
    return result ~= nil and session:queue_inspect().paused and not session:queue_inspect().pending
  end)
  local state = session:queue_inspect()
  MiniTest.expect.equality({ state.paused, #state.entries, #permissions }, { true, 1, 1 })
  MiniTest.expect.equality(result, { { stopReason = "cancelled" } })
  local continued
  assert(session:queue_continue(state.revision, function(ok, err)
    continued = { ok, err }
  end))
  MiniTest.expect.equality(continued, nil)
  assert(nvim.wait(1000, function()
    drain(queued)
    return #permissions == 2
  end))
  MiniTest.expect.equality(continued, { true })
  MiniTest.expect.equality(session:queue_inspect().paused, false)
end

T["demo API"]["Stop fences an admission while the Session is still ready"] = function()
  local root = fixture()
  local queued, schedule = scheduler()
  local api = assert(Demo.new({ project_root = root, schedule = schedule }))
  MiniTest.finally(function()
    api:dispose()
    nvim.fn.delete(root, "rf")
  end)
  local session = assert(api:create_session("your-codex-here"))
  local admitted
  assert(session:enqueue("waiting", function(id, err)
    admitted = { id = id, error = err }
  end))
  MiniTest.expect.equality(session:inspect().status, "ready")
  assert(session:cancel())
  wait_until(queued, function()
    local state = session:queue_inspect()
    return admitted ~= nil and state.paused and not state.pending
  end)
  assert(admitted.id, admitted.error)
  MiniTest.expect.equality(#session:queue_inspect().entries, 1)
  MiniTest.expect.equality(session:inspect().current_turn, 0)
end

T["demo API"]["cancelling before dispatch settles admission without a tool event"] = function()
  local root = fixture()
  local queued, schedule = scheduler()
  local api = assert(Demo.new({ project_root = root, schedule = schedule }))
  MiniTest.finally(function()
    api:dispose()
    nvim.fn.delete(root, "rf")
  end)
  local session = assert(api:create_session("your-codex-here"))
  local events, result = {}, nil
  session:on(function(event)
    events[#events + 1] = event
  end)
  local id = assert(session:prompt("cancel before dispatch", function(value, err)
    result = { value, err }
  end))
  assert(session:cancel())
  drain(queued)
  local types = {}
  for _, event in ipairs(events) do
    types[#types + 1] = event.type
  end
  MiniTest.expect.equality(types, { "state_changed", "admission_settled", "state_changed", "turn_done" })
  MiniTest.expect.equality({ events[2].data.turn_id, events[2].data.result }, { id, "cancelled" })
  MiniTest.expect.equality(result, { { stopReason = "cancelled" } })
end

T["demo API"]["retains, edits, and dispatches independent busy submissions"] = function()
  local root = fixture()
  local queued, schedule = scheduler()
  local api = assert(Demo.new({ project_root = root, schedule = schedule }))
  MiniTest.finally(function()
    api:dispose()
    nvim.fn.delete(root, "rf")
  end)
  local session = assert(api:create_session("your-codex-here"))
  local permissions, dispatched, changes = {}, {}, {}
  session:on(function(event)
    if event.type == "permission_requested" then
      permissions[#permissions + 1] = event
    elseif event.type == "prompt_dispatched" then
      dispatched[#dispatched + 1] = event.data
    elseif event.type == "queue_changed" then
      changes[#changes + 1] = event.data
    end
  end)
  assert(session:prompt("active"))
  drain(queued)
  MiniTest.expect.equality(#permissions, 1)

  local contexts = { { label = "file", text = "original" } }
  local admissions = {}
  assert(session:enqueue("first", function(id, err)
    admissions[#admissions + 1] = { id = id, error = err }
  end, nil, "first visible", contexts))
  assert(session:enqueue("second", function(id, err)
    admissions[#admissions + 1] = { id = id, error = err }
  end))
  contexts[1].text = "changed later"
  MiniTest.expect.equality(admissions, {})
  wait_until(queued, function()
    return #admissions == 2
  end)
  local state = session:queue_inspect()
  MiniTest.expect.equality(#state.entries, 2)
  MiniTest.expect.equality(state.entries[1].contexts[1].text, "original")
  MiniTest.expect.equality({ admissions[1].id, admissions[2].id }, { state.entries[1].id, state.entries[2].id })
  MiniTest.expect.equality(#changes > 0, true)

  local moved
  assert(session:queue_move(state.entries[2].id, 1, state.revision, function(ok, err)
    moved = { ok, err }
  end))
  MiniTest.expect.equality(moved, nil)
  wait_until(queued, function()
    return moved ~= nil
  end)
  MiniTest.expect.equality(moved, { true })
  state = session:queue_inspect()
  MiniTest.expect.equality(state.entries[1].display_text, "second")

  local replaced
  assert(session:queue_replace(state.entries[1].id, state.revision, "replacement", function(ok, err)
    replaced = { ok, err }
  end))
  wait_until(queued, function()
    return replaced ~= nil
  end)
  MiniTest.expect.equality(replaced, { true })
  state = session:queue_inspect()
  local replacement_id = state.entries[1].id
  MiniTest.expect.equality(state.entries[1].display_text, "replacement")
  MiniTest.expect.equality(replacement_id ~= admissions[2].id, true)

  local cancelled
  assert(session:queue_cancel(state.entries[2].id, state.revision, function(ok, err)
    cancelled = { ok, err }
  end))
  wait_until(queued, function()
    return cancelled ~= nil
  end)
  MiniTest.expect.equality(cancelled, { true })
  assert(permissions[1].respond({ outcome = { outcome = "selected", optionId = "reject-once" } }))
  assert(nvim.wait(1000, function()
    drain(queued)
    return #permissions == 2
  end))
  MiniTest.expect.equality(dispatched[2].queue_entry.id, replacement_id)
  MiniTest.expect.equality(dispatched[2].queue_entry.display_text, "replacement")
  MiniTest.expect.equality(session:queue_inspect().active_status, "dispatched")
end

T["demo API"]["drops unacknowledged queue work and late callbacks on disposal"] = function()
  local root = fixture()
  local queued, schedule = scheduler()
  local api = assert(Demo.new({ project_root = root, schedule = schedule }))
  MiniTest.finally(function()
    api:dispose()
    nvim.fn.delete(root, "rf")
  end)
  local session = assert(api:create_session("your-codex-here"))
  local events, result = {}, nil
  session:on(function(event)
    events[#events + 1] = event.type
  end)
  assert(session:enqueue("queued", function(id, err)
    result = { id = id, error = err }
  end))
  assert(session:dispose())
  local count = #events
  local disposal_result = nvim.deepcopy(result)
  drain(queued)
  nvim.wait(20)
  MiniTest.expect.equality(#events, count)
  MiniTest.expect.equality(result, disposal_result)
  if result ~= nil then
    MiniTest.expect.equality(result, { error = "Session Disposal stopped queue admission" })
  end
  MiniTest.expect.equality(session:inspect().status, "disposed")
  MiniTest.expect.equality(session:inspect().current_turn, 0)
end

T["demo API"]["a failed scripted turn reports its error and pauses waiting work"] = function()
  local root, path = fixture()
  local queued, schedule = scheduler()
  local api = assert(Demo.new({ project_root = root, schedule = schedule }))
  MiniTest.finally(function()
    api:dispose()
    nvim.fn.delete(root, "rf")
  end)
  local session = assert(api:create_session("your-codex-here"))
  local completion, admission
  assert(session:prompt("active", function(result, err)
    completion = { result = result, error = err }
  end))
  assert(session:enqueue("waiting", function(id, err)
    admission = { id = id, error = err }
  end))
  assert(nvim.fn.delete(path) == 0)
  wait_until(queued, function()
    return completion ~= nil and admission ~= nil and not session:queue_inspect().pending
  end)
  MiniTest.expect.equality(completion, { error = "demo project is missing " .. path })
  assert(admission.id, admission.error)
  local state = session:queue_inspect()
  MiniTest.expect.equality({ state.paused, #state.entries, session:inspect().current_turn }, { true, 1, 1 })
end

T["demo API"]["reloading the seeded Session does not invent queue persistence"] = function()
  local root = fixture()
  local queued, schedule = scheduler()
  local api = assert(Demo.new({ project_root = root, schedule = schedule }))
  MiniTest.finally(function()
    api:dispose()
    nvim.fn.delete(root, "rf")
  end)
  local session = assert(api:load_session("your-codex-here", "scripted-resume-1"))
  drain(queued)
  assert(session:prompt("active"))
  drain(queued)
  local admitted
  assert(session:enqueue("waiting", function(id, err)
    admitted = { id = id, error = err }
  end))
  wait_until(queued, function()
    return admitted ~= nil
  end)
  assert(admitted.id, admitted.error)
  MiniTest.expect.equality(#session:queue_inspect().entries, 1)
  assert(session:dispose())
  drain(queued)
  local restored = assert(api:load_session("your-codex-here", "scripted-resume-1"))
  drain(queued)
  MiniTest.expect.equality({ restored:inspect().status, #restored:queue_inspect().entries }, { "ready", 0 })
end

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
  MiniTest.expect.equality({ events[2].type, events[3].type }, { "admission_settled", "prompt_dispatched" })
  MiniTest.expect.equality(events[4].type, "chunk")
  MiniTest.expect.equality(events[4].data.content.text:find("scripted demo", 1, true) ~= nil, true)
  MiniTest.expect.equality(events[5].type, "tool_call_started")
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

T["demo API"]["localizes scripted responses for an existing Session"] = function()
  local root = fixture()
  local queued, schedule = scheduler()
  local api = assert(Demo.new({ project_root = root, schedule = schedule }))
  local session = assert(api:create_session("your-codex-here"))
  local messages = {}
  local permission
  session:on(function(event)
    if event.type == "chunk" then
      messages[#messages + 1] = event.data.content.text
    elseif event.type == "permission_requested" then
      permission = event
    end
  end)

  MiniTest.expect.equality({ api:set_language("fr") }, { false, "unknown demo language 'fr'" })
  assert(api:set_language("zh-CN"))
  assert(session:prompt("修复计算器"))
  drain(queued)
  MiniTest.expect.equality(messages[1]:find("我检查了已附加的计算器", 1, true) ~= nil, true)

  assert(permission.respond({ outcome = { outcome = "selected", optionId = "reject-once" } }))
  drain(queued)
  MiniTest.expect.equality(messages[2]:find("文件没有变化", 1, true) ~= nil, true)

  assert(api:dispose())
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
  MiniTest.expect.equality(events[4].type, "chunk")
  MiniTest.expect.equality(events[4].data.content.text:find("Handoff received", 1, true) ~= nil, true)
  MiniTest.expect.equality(session:inspect().status, "ready")

  assert(api:dispose())
  nvim.fn.delete(root, "rf")
end

return T
