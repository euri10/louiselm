local MiniTest = require("mini.test")
local Session = require("louiselm.session")
local Chat = require("louiselm.ui.chat")
local Decisions = require("louiselm.ui.chat.decisions")

---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local T = MiniTest.new_set()

local function wait_for(predicate)
  assert(nvim.wait(10000, predicate, 10), "queue transition did not finish")
end

local function fixture(directory, load_id, mode, interaction_file)
  directory = directory or nvim.fn.tempname()
  local root = nvim.fn.getcwd()
  local api = assert(Session.new({
    mock = {
      provider = "test",
      command = nvim.v.progpath,
      args = {
        "--headless",
        "--noplugin",
        "-u",
        root .. "/tests/mock/init.lua",
        "-c",
        "lua require('louiselm.dev.mock_agent').run()",
      },
      env = { LOUISELM_MOCK_MODE = mode or "permission", LOUISELM_MOCK_INTERACTION_FILE = interaction_file },
    },
  }, nil, { usage_directory = directory }))
  MiniTest.finally(function()
    for _, id in ipairs(api:list_sessions()) do
      local owned = api:get_session(id)
      if owned ~= nil and owned:queue_inspect().uncertain then
        assert(owned:abandon())
      end
    end
    local disposed, disposal_error
    assert(api:dispose(function(ok, err)
      disposed, disposal_error = ok, err
    end))
    wait_for(function()
      return disposed ~= nil
    end)
    assert(disposed, disposal_error)
    local done
    api:flush_recording(function()
      done = true
    end)
    wait_for(function()
      return done
    end)
    nvim.fn.delete(directory, "rf")
  end)
  local session = load_id and assert(api:load_session("mock", load_id, { cwd = root }))
    or assert(api:create_session("mock", { cwd = root }))
  wait_for(function()
    return session:inspect().status == "ready"
  end)
  return api, session
end

local function admit(session, prompt)
  local complete, id, failure
  assert(session:enqueue(prompt, function(accepted, err)
    complete, id, failure = true, accepted, err
  end))
  wait_for(function()
    return complete
  end)
  return id, failure
end

local function change(start)
  local complete, changed, failure
  assert(start(function(ok, err)
    complete, changed, failure = true, ok, err
  end))
  wait_for(function()
    return complete
  end)
  return changed, failure
end

local function crash_editor(directory, settled, graceful)
  local output = ""
  local process = nvim.system({
    "python3",
    "tests/fixtures/queue_crash.py",
    nvim.v.progpath,
    directory,
    settled and "1" or "0",
    graceful and "1" or "0",
  }, {
    stdin = true,
    text = true,
    stdout = function(_, data)
      output = output .. (data or "")
    end,
  })
  MiniTest.finally(function()
    if not process:is_closing() then
      process:write("crash\n")
      process:write(nil)
      process:wait(5000)
    end
  end)
  assert(
    nvim.wait(25000, function()
      return output:find("READY", 1, true) ~= nil
    end, 10),
    "old editor did not prepare its queue"
  )
  local acp_id = nvim.fn.readfile(directory .. "/ready")[1]
  if not graceful then
    process:write("crash\n")
  end
  process:write(nil)
  local result = process:wait(10000)
  assert(result.code == 0, result.stderr)
  return acp_id
end

T["queue waits for a typed interaction without answering it"] = function()
  local _, session = fixture(nil, nil, "echo", nvim.fn.getcwd() .. "/tests/fixtures/grok_ask_user_question.json")
  local interactions, dispatched = {}, {}
  session:on(function(event)
    if event.type == "interaction_requested" then
      assert(event.claim())
      interactions[#interactions + 1] = event
    elseif event.type == "prompt_dispatched" then
      dispatched[#dispatched + 1] = event.data
    end
  end)
  assert(session:prompt("active"))
  wait_for(function()
    return #interactions == 1
  end)
  assert(admit(session, "after the answer"))
  nvim.wait(50)
  MiniTest.expect.equality(#dispatched, 1)
  MiniTest.expect.equality(#session:queue_inspect().entries, 1)
  assert(interactions[1].respond({ outcome = "cancelled" }))
  wait_for(function()
    return #dispatched == 2 and #interactions == 2
  end)
  MiniTest.expect.equality(dispatched[2].display_text, "after the answer")
end

T["Chat freezes distinct busy submissions while a typed permission waits"] = function()
  local api, session = fixture()
  local permissions, dispatched = {}, {}
  session:on(function(event)
    if event.type == "permission_requested" then
      permissions[#permissions + 1] = event
    elseif event.type == "prompt_dispatched" then
      dispatched[#dispatched + 1] = event.data
    end
  end)
  local decisions = Decisions.new({
    is_visible = function()
      return false
    end,
    is_live = function()
      return true
    end,
    report_error = function() end,
    resolved = function() end,
  })
  local chat = assert(Chat.new(api, {
    markdown_highlighting = false,
    start_insert_on_switch = false,
    decisions = decisions,
  }))
  MiniTest.finally(function()
    chat:dispose()
    decisions:dispose()
  end)
  assert(session:prompt("active"))
  wait_for(function()
    return #permissions == 1
  end)
  assert(chat:attach(session))
  assert(chat:queue_context({ label = "first", text = "first context" }))
  assert(chat:submit("first waiting"))
  wait_for(function()
    return #session:queue_inspect().entries == 1
  end)
  assert(chat:queue_context({ label = "second", text = "second context" }))
  assert(chat:submit("second waiting"))
  assert(
    nvim.wait(10000, function()
      return #session:queue_inspect().entries == 2
    end, 10),
    nvim.inspect(session:queue_inspect())
  )
  local view = chat.views[session:inspect().id]
  view.renderer:replace_prompt("private draft")
  MiniTest.expect.equality(#dispatched, 1)
  MiniTest.expect.equality({ session:prompt("direct while busy") }, { nil, "session is not ready" })

  assert(permissions[1].respond({ outcome = { outcome = "selected", optionId = "allow-once" } }))
  wait_for(function()
    return #permissions == 2 and #dispatched == 2
  end)
  MiniTest.expect.equality(dispatched[2].prompt, {
    { type = "text", text = "first context" },
    { type = "text", text = "first waiting" },
  })
  MiniTest.expect.equality(view.renderer:prompt_text(), "private draft")
  assert(permissions[2].respond({ outcome = { outcome = "selected", optionId = "allow-once" } }))
  wait_for(function()
    return #permissions == 3 and #dispatched == 3
  end)
  MiniTest.expect.equality(dispatched[3].prompt, {
    { type = "text", text = "second context" },
    { type = "text", text = "second waiting" },
  })
  MiniTest.expect.equality(view.renderer:prompt_text(), "private draft")
end

T["Chat retains its first accepted instruction while it is awaiting a decision"] = function()
  local api, session = fixture()
  local permissions = {}
  session:on(function(event)
    if event.type == "permission_requested" then
      permissions[#permissions + 1] = event
    end
  end)
  local decisions = Decisions.new({
    is_visible = function()
      return false
    end,
    is_live = function()
      return true
    end,
    report_error = function() end,
    resolved = function() end,
  })
  local chat =
    assert(Chat.new(api, { markdown_highlighting = false, start_insert_on_switch = false, decisions = decisions }))
  MiniTest.finally(function()
    chat:dispose()
    decisions:dispose()
  end)
  assert(chat:attach(session))
  assert(chat:submit("first instruction"))
  wait_for(function()
    return #permissions == 1
  end)
  local state = session:queue_inspect()
  MiniTest.expect.equality(state.active and state.active.display_text, "first instruction")
  assert(chat:submit("second instruction"))
  wait_for(function()
    return #session:queue_inspect().entries == 1
  end)
  MiniTest.expect.equality(session:queue_inspect().entries[1].display_text, "second instruction")
end

T["successive Sends during durable admission keep their order and leave a fresh draft"] = function()
  local api, session = fixture()
  local permission
  session:on(function(event)
    if event.type == "permission_requested" then
      permission = event
    end
  end)
  assert(session:prompt("active"))
  wait_for(function()
    return permission ~= nil
  end)
  local chat = assert(Chat.new(api, { markdown_highlighting = false, start_insert_on_switch = false }))
  MiniTest.finally(function()
    chat:dispose()
  end)
  assert(chat:attach(session))
  assert(chat:submit("first"))
  assert(chat:submit("second"))
  wait_for(function()
    return #session:queue_inspect().entries == 2
  end)
  local entries = session:queue_inspect().entries
  MiniTest.expect.equality({ entries[1].display_text, entries[2].display_text }, { "first", "second" })
  MiniTest.expect.equality(chat.views[session:inspect().id].renderer:prompt_text(), "")
end

T["an unacknowledged queue write stays uncertain and cannot be replayed"] = function()
  local api, session = fixture()
  session.owner.queue_store.save = function(_, _, _, _, callback)
    nvim.schedule(function()
      callback("queue storage outcome is uncertain; review before retrying")
    end)
  end
  local chat = assert(Chat.new(api, { markdown_highlighting = false, start_insert_on_switch = false }))
  MiniTest.finally(function()
    chat:dispose()
  end)
  assert(chat:attach(session))
  assert(chat:submit("unknown admission"))
  wait_for(function()
    return session:queue_inspect().uncertain
  end)
  local state = session:queue_inspect()
  MiniTest.expect.equality(#state.entries, 0)
  MiniTest.expect.equality(state.uncertain_proposal.entries[1].display_text, "unknown admission")
  MiniTest.expect.equality(chat.views[session:inspect().id].renderer:prompt_text(), "unknown admission")
  MiniTest.expect.equality(session:inspect().current_turn, 0)
  local started = session:enqueue("unknown admission", function() end)
  MiniTest.expect.equality(started, false)
end

T["transport loss preserves uncertain dispatched work and pauses later entries"] = function()
  local _, session = fixture(nil, nil, "crash")
  local first, second
  assert(session:enqueue("may have arrived", function(id, err)
    first = assert(id, err)
  end))
  assert(session:enqueue("must wait", function(id, err)
    second = assert(id, err)
  end))
  wait_for(function()
    return first ~= nil and second ~= nil
  end)
  wait_for(function()
    local queue = session:queue_inspect()
    return session:inspect().status == "error" and queue.uncertain
  end)
  local queue = session:queue_inspect()
  MiniTest.expect.equality(queue.active.id, first)
  MiniTest.expect.equality(queue.active_status, "uncertain")
  MiniTest.expect.equality(queue.entries[1].id, second)
  MiniTest.expect.equality(queue.paused, true)
end

T["a failed transport write remains uncertain when admission reports uncertainty"] = function()
  local _, session = fixture()
  local transport = session.client.transport
  local send = transport.send
  transport.send = function(self, message)
    if message.method == "session/prompt" then
      return false, "synthetic transport failure"
    end
    return send(self, message)
  end
  assert(admit(session, "delivery cannot be inferred"))
  wait_for(function()
    local queue = session:queue_inspect()
    return session:inspect().status == "error"
      and queue.paused
      and (queue.uncertain or queue.active_status == "not_sent")
  end)
  local queue = session:queue_inspect()
  MiniTest.expect.equality(queue.active_status, "uncertain")
  MiniTest.expect.equality(queue.uncertain, true)
end

T["Session Disposal completes only after its saved waiting work is fenced"] = function()
  local _, session = fixture()
  local permission
  session:on(function(event)
    if event.type == "permission_requested" then
      permission = event
    end
  end)
  assert(session:prompt("active"))
  wait_for(function()
    return permission ~= nil
  end)
  assert(admit(session, "waiting at disposal"))
  local store = session.owner.queue_store
  local save = store.save
  local release
  store.save = function(self, id, revision, snapshot, callback)
    if snapshot.state.disposed then
      release = function()
        save(self, id, revision, snapshot, callback)
      end
    else
      save(self, id, revision, snapshot, callback)
    end
  end
  local finished, failure
  assert(session:dispose(nil, function(ok, err)
    finished, failure = ok, err
  end))
  wait_for(function()
    return release ~= nil
  end)
  MiniTest.expect.equality(finished, nil)
  MiniTest.expect.equality(session:inspect().status, "disposing")
  MiniTest.expect.equality({ session:prompt("late") }, { nil, "session is not ready" })
  release()
  wait_for(function()
    return finished ~= nil
  end)
  MiniTest.expect.equality({ finished, failure }, { true })
  MiniTest.expect.equality(session:inspect().status, "disposed")
end

T["a failed Disposal fence is reported and never claims the queue was disposed"] = function()
  local _, session = fixture()
  local permission
  session:on(function(event)
    if event.type == "permission_requested" then
      permission = event
    end
  end)
  assert(session:prompt("active"))
  wait_for(function()
    return permission ~= nil
  end)
  assert(admit(session, "waiting"))
  local store = session.owner.queue_store
  local save = store.save
  store.save = function(self, id, revision, snapshot, callback)
    if snapshot.state.disposed then
      nvim.schedule(function()
        callback("synthetic uncertain fence")
      end)
    else
      save(self, id, revision, snapshot, callback)
    end
  end
  local done, disposed, failure
  assert(session:dispose(nil, function(ok, err)
    done, disposed, failure = true, ok, err
  end))
  wait_for(function()
    return done
  end)
  MiniTest.expect.equality({ disposed, failure }, { false, "synthetic uncertain fence" })
  MiniTest.expect.equality(session:inspect().status, "error")
  MiniTest.expect.equality(session:queue_inspect().uncertain, true)
  MiniTest.expect.equality(session:queue_inspect().entries[1].display_text, "waiting")
  assert(session:abandon())
end

T["Disposal during an unacknowledged admission preserves its uncertain payload"] = function()
  local api, session = fixture()
  local release
  session.owner.queue_store.save = function(_, _, _, _, callback)
    release = function()
      callback("admission acknowledgement lost")
    end
  end
  local chat = assert(Chat.new(api, { markdown_highlighting = false, start_insert_on_switch = false }))
  MiniTest.finally(function()
    chat:dispose()
  end)
  assert(chat:attach(session))
  assert(chat:submit("possibly accepted"))
  wait_for(function()
    return release ~= nil
  end)
  local disposed, failure
  assert(session:dispose(nil, function(ok, err)
    disposed, failure = ok, err
  end))
  release()
  wait_for(function()
    return disposed ~= nil
  end)
  MiniTest.expect.equality({ disposed, failure }, { false, "admission acknowledgement lost" })
  local queue = session:queue_inspect()
  MiniTest.expect.equality(queue.uncertain, true)
  MiniTest.expect.equality(queue.uncertain_proposal.entries[1].display_text, "possibly accepted")
  assert(session:abandon())
end

T["registry Disposal acknowledges only after all Session queue fences finish"] = function()
  local api, session = fixture()
  local permission
  session:on(function(event)
    if event.type == "permission_requested" then
      permission = event
    end
  end)
  assert(session:prompt("active"))
  wait_for(function()
    return permission ~= nil
  end)
  assert(admit(session, "waiting"))
  local store = session.owner.queue_store
  local save = store.save
  local release
  store.save = function(self, id, revision, snapshot, callback)
    if snapshot.state.disposed then
      release = function()
        save(self, id, revision, snapshot, callback)
      end
    else
      save(self, id, revision, snapshot, callback)
    end
  end
  local result
  assert(api:dispose(function(ok, err)
    result = { ok, err }
  end))
  wait_for(function()
    return release ~= nil
  end)
  MiniTest.expect.equality(result, nil)
  MiniTest.expect.equality(session:inspect().status, "disposing")
  release()
  wait_for(function()
    return result ~= nil
  end)
  MiniTest.expect.equality(result, { true })
  MiniTest.expect.equality(session:inspect().status, "disposed")
end

T["process-wide Disposal waits for queue fences before acknowledging completion"] = function()
  local _, session = fixture()
  local permission
  session:on(function(event)
    if event.type == "permission_requested" then
      permission = event
    end
  end)
  assert(session:prompt("active"))
  wait_for(function()
    return permission ~= nil
  end)
  assert(admit(session, "waiting"))
  local store = session.owner.queue_store
  local save = store.save
  local release
  store.save = function(self, id, revision, snapshot, callback)
    if snapshot.state.disposed then
      release = function()
        save(self, id, revision, snapshot, callback)
      end
    else
      save(self, id, revision, snapshot, callback)
    end
  end
  local result
  assert(Session.dispose_all(function(ok, err)
    result = { ok, err }
  end))
  wait_for(function()
    return release ~= nil
  end)
  MiniTest.expect.equality(result, nil)
  release()
  wait_for(function()
    return result ~= nil
  end)
  MiniTest.expect.equality(result, { true })
end

T["editor abandonment preserves saved waiting work instead of fencing it as disposed"] = function()
  local _, session = fixture()
  local permission
  session:on(function(event)
    if event.type == "permission_requested" then
      permission = event
    end
  end)
  assert(session:prompt("active"))
  wait_for(function()
    return permission ~= nil
  end)
  assert(admit(session, "review after editor exit"))
  local database = session.owner.queue_store.path
  assert(session:abandon())
  local query = nvim
    .system({
      "sqlite3",
      "-json",
      database,
      "SELECT COALESCE(json_extract(payload,'$.state.disposed'),0) AS disposed FROM queues;",
    }, { text = true })
    :wait()
  assert(query.code == 0, query.stderr)
  MiniTest.expect.equality(nvim.json.decode(query.stdout)[1].disposed, 0)
end

T["waiting work is bounded, editable by revision, and Stop pauses until Continue"] = function()
  local _, session = fixture()
  local permissions, dispatched = {}, {}
  session:on(function(event)
    if event.type == "permission_requested" then
      permissions[#permissions + 1] = event
    elseif event.type == "prompt_dispatched" then
      dispatched[#dispatched + 1] = event.data
    end
  end)
  assert(session:prompt("active"))
  wait_for(function()
    return #permissions == 1
  end)
  local ids = {}
  for index = 1, 20 do
    ids[index] = assert(admit(session, "waiting " .. index))
  end
  local id, full = admit(session, "one too many")
  MiniTest.expect.equality(id, nil)
  MiniTest.expect.equality(full, "Session queue is full (20 waiting instructions)")
  local initial = session:queue_inspect()
  MiniTest.expect.equality(#initial.entries, 20)
  assert(change(function(callback)
    return session:queue_move(ids[20], 1, initial.revision, callback)
  end))
  local stale, stale_error = change(function(callback)
    return session:queue_cancel(ids[1], initial.revision, callback)
  end)
  MiniTest.expect.equality(stale, false)
  MiniTest.expect.equality(stale_error, "queue changed; review it again")
  local reordered = session:queue_inspect()
  MiniTest.expect.equality(reordered.entries[1].id, ids[20])
  assert(change(function(callback)
    return session:queue_replace(ids[20], reordered.revision, "replacement", callback)
  end))
  local replaced = session:queue_inspect()
  MiniTest.expect.equality(replaced.entries[1].display_text, "replacement")
  assert(replaced.entries[1].id ~= ids[20])
  MiniTest.expect.equality(replaced.entries[2].id, ids[1])
  assert(change(function(callback)
    return session:queue_cancel(ids[1], replaced.revision, callback)
  end))
  assert(session:cancel())
  wait_for(function()
    return session:inspect().status == "ready" and session:queue_inspect().paused
  end)
  assert(admit(session, "added while paused"))
  nvim.wait(100)
  MiniTest.expect.equality(#dispatched, 1)
  local paused = session:queue_inspect()
  MiniTest.expect.equality(#paused.entries, 20)
  assert(change(function(callback)
    return session:queue_continue(paused.revision, callback)
  end))
  wait_for(function()
    return #dispatched == 2 and #permissions == 2
  end)
  MiniTest.expect.equality(dispatched[2].display_text, "replacement")
end

T["a full queue refuses Chat Send and preserves the private draft"] = function()
  local api, session = fixture()
  local permission
  session:on(function(event)
    if event.type == "permission_requested" then
      permission = event
    end
  end)
  assert(session:prompt("active"))
  wait_for(function()
    return permission ~= nil
  end)
  for index = 1, 20 do
    assert(admit(session, "waiting " .. index))
  end
  local chat = assert(Chat.new(api, { markdown_highlighting = false, start_insert_on_switch = false }))
  local notify, notifications = nvim.notify, {}
  rawset(nvim, "notify", function(message)
    notifications[#notifications + 1] = message
  end)
  MiniTest.finally(function()
    rawset(nvim, "notify", notify)
    chat:dispose()
  end)
  assert(chat:attach(session))
  assert(chat:queue_context({ label = "file", text = "private context" }))
  assert(chat:submit("keep this draft"))
  wait_for(function()
    return #notifications > 0
  end)
  local view = chat.views[session:inspect().id]
  MiniTest.expect.equality(view.renderer:prompt_text(), "[context: file] keep this draft")
  MiniTest.expect.equality(view.draft.contexts, { { label = "file", text = "private context" } })
  MiniTest.expect.equality(#session:queue_inspect().entries, 20)
  MiniTest.expect.equality(notifications[1]:find("queue is full", 1, true) ~= nil, true)
end

T["Stop also pauses a queue created after cancellation begins"] = function()
  local _, session = fixture()
  local permissions, dispatched = {}, {}
  session:on(function(event)
    if event.type == "permission_requested" then
      permissions[#permissions + 1] = event
    elseif event.type == "prompt_dispatched" then
      dispatched[#dispatched + 1] = event.data
    end
  end)
  assert(session:prompt("active"))
  wait_for(function()
    return #permissions == 1
  end)
  assert(session:cancel())
  wait_for(function()
    return session:inspect().status == "ready"
  end)
  assert(admit(session, "after Stop"))
  nvim.wait(100)
  MiniTest.expect.equality(#dispatched, 1)
  local paused = session:queue_inspect()
  MiniTest.expect.equality(paused.paused, true)
  MiniTest.expect.equality(#paused.entries, 1)
  assert(change(function(callback)
    return session:queue_continue(paused.revision, callback)
  end))
  wait_for(function()
    return #dispatched == 2
  end)
end

T["a crashed editor recovers its waiting payload paused without replaying an uncertain turn"] = function()
  local directory = nvim.fn.tempname()
  local acp_id = crash_editor(directory, false)
  local _, restored = fixture(directory, acp_id)
  local recovered = restored:queue_inspect()
  assert(#recovered.entries == 1, nvim.inspect(recovered))
  MiniTest.expect.equality(recovered.paused, true)
  MiniTest.expect.equality(recovered.uncertain, true)
  MiniTest.expect.equality(#recovered.entries, 1)
  MiniTest.expect.equality(recovered.entries[1].display_text, "saved for review")
  local resumed = restored:queue_continue(recovered.revision, function() end)
  MiniTest.expect.equality(resumed, false)
  nvim.wait(100)
  MiniTest.expect.equality(restored:inspect().current_turn, 0)
end

T["a fenced former owner and settled turn permit explicit continuation after restart"] = function()
  local directory = nvim.fn.tempname()
  local acp_id = crash_editor(directory, true)
  local _, restored = fixture(directory, acp_id)
  local queue = restored:queue_inspect()
  MiniTest.expect.equality(queue.paused, true)
  MiniTest.expect.equality(queue.uncertain, false)
  MiniTest.expect.equality(queue.entries[1].display_text, "saved for review")
  MiniTest.expect.equality(restored:inspect().current_turn, 0)
  assert(change(function(callback)
    return restored:queue_continue(queue.revision, callback)
  end))
  wait_for(function()
    return restored:inspect().current_turn == 1
  end)
end

T["VimLeavePre retains the saved queue for paused restart recovery"] = function()
  local directory = nvim.fn.tempname()
  local acp_id = crash_editor(directory, true, true)
  local _, restored = fixture(directory, acp_id)
  local queue = restored:queue_inspect()
  MiniTest.expect.equality(queue.entries[1].display_text, "saved for review")
  MiniTest.expect.equality(queue.paused, true)
  MiniTest.expect.equality(queue.uncertain, false)
  MiniTest.expect.equality(restored:inspect().current_turn, 0)
end

T["a live owner cannot be displaced and a reused local ID cannot adopt its queue"] = function()
  local api, original = fixture()
  local permission
  original:on(function(event)
    if event.type == "permission_requested" then
      permission = event
    end
  end)
  assert(original:prompt("active"))
  wait_for(function()
    return permission ~= nil
  end)
  assert(admit(original, "owned by original"))
  local resumed = assert(api:load_session("mock", original:inspect().acp_session_id, { cwd = nvim.fn.getcwd() }))
  wait_for(function()
    return resumed:inspect().status == "ready"
  end)
  local queue = resumed:queue_inspect()
  MiniTest.expect.equality(queue.uncertain, true)
  MiniTest.expect.equality(queue.entries[1].display_text, "owned by original")
  MiniTest.expect.equality(resumed:queue_continue(queue.revision, function() end), false)
  local disposed, disposal_error
  assert(resumed:dispose(nil, function(ok, err)
    disposed, disposal_error = ok, err
  end))
  wait_for(function()
    return disposed ~= nil
  end)
  MiniTest.expect.equality(disposed, false)
  MiniTest.expect.equality(disposal_error, "saved queue ownership must be reconciled before Disposal can be confirmed")
  MiniTest.expect.equality(original:queue_inspect().entries[1].display_text, "owned by original")
  local _, new_session = fixture()
  MiniTest.expect.equality(new_session:inspect().id, original:inspect().id)
  MiniTest.expect.equality(new_session:queue_inspect().entries, {})
end

return T
