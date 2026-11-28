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

local function fixture(directory, load_id, mode, interaction_file, permission_policy)
  directory = directory or nvim.fn.tempname()
  local root = nvim.fn.getcwd()
  local definition = {
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
  }
  local api = assert(Session.new({ mock = definition, other = nvim.deepcopy(definition) }, nil, {
    usage_directory = directory,
  }))
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
  local options = { cwd = root, permission_policy = permission_policy }
  local session = load_id and assert(api:load_session("mock", load_id, options))
    or assert(api:create_session("mock", options))
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

local function crash_editor(directory, settled, graceful, replacement_phase)
  local output = ""
  local process = nvim.system({
    "python3",
    "tests/fixtures/queue_crash.py",
    nvim.v.progpath,
    directory,
    settled and "1" or "0",
    graceful and "1" or "0",
    replacement_phase or "",
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

T["another Session can dispatch while the first queue waits"] = function()
  local api, first = fixture()
  local second = assert(api:create_session("other", { cwd = nvim.fn.getcwd() }))
  wait_for(function()
    return second:inspect().status == "ready"
  end)
  local first_wait, second_wait
  first:on(function(event)
    if event.type == "permission_requested" then
      first_wait = event
    end
  end)
  second:on(function(event)
    if event.type == "permission_requested" then
      second_wait = event
    end
  end)
  assert(first:prompt("first active"))
  wait_for(function()
    return first_wait ~= nil
  end)
  assert(admit(first, "first waiting"))
  assert(admit(second, "independent work"))
  wait_for(function()
    return second_wait ~= nil
  end)
  MiniTest.expect.equality(first:inspect().current_turn, 1)
  MiniTest.expect.equality(first:queue_inspect().entries[1].display_text, "first waiting")
  MiniTest.expect.equality(second:inspect().current_turn, 1)
  MiniTest.expect.equality(second:queue_inspect().active.display_text, "independent work")
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
  local release, released
  MiniTest.finally(function()
    if release ~= nil and not released then
      release()
    end
  end)
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
  store.save = function(self, id, revision, snapshot, callback)
    if snapshot.state.disposed then
      release = function()
        released = true
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
  local release, released
  MiniTest.finally(function()
    if release ~= nil and not released then
      release()
    end
  end)
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
      release = function()
        released = true
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
  local returned, rejected
  assert(session:enqueue("one too many", function(id, err)
    assert(returned, "queue-full callback ran before enqueue returned")
    rejected = { id, err }
  end))
  returned = true
  wait_for(function()
    return rejected ~= nil
  end)
  MiniTest.expect.equality(rejected[1], nil)
  MiniTest.expect.equality(rejected[2], "Session queue is full (20 waiting instructions)")
  local initial = session:queue_inspect()
  MiniTest.expect.equality(#initial.entries, 20)
  assert(change(function(callback)
    return session:queue_move(ids[20], 1, initial.revision, callback)
  end))
  returned, rejected = false, nil
  assert(session:queue_cancel(ids[1], initial.revision, function(changed, err)
    assert(returned, "stale-revision callback ran before queue_cancel returned")
    rejected = { changed, err }
  end))
  returned = true
  wait_for(function()
    return rejected ~= nil
  end)
  MiniTest.expect.equality(rejected, { false, "queue changed; review it again" })
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

T["Stop pauses an idle Session while a queued dispatch is committing"] = function()
  local release, released
  MiniTest.finally(function()
    if release ~= nil and not released then
      release()
    end
  end)
  local _, session = fixture(nil, nil, "echo")
  local store = session.owner.queue_store
  local save = store.save
  store.save = function(self, id, revision, snapshot, callback)
    if snapshot.state.active_status == "admitting" and release == nil then
      release = function()
        released = true
        save(self, id, revision, snapshot, callback)
      end
    else
      save(self, id, revision, snapshot, callback)
    end
  end
  assert(admit(session, "must stay paused"))
  wait_for(function()
    return release ~= nil
  end)
  MiniTest.expect.equality(session:inspect().status, "ready")
  assert(session:cancel())
  release()
  wait_for(function()
    local queue = session:queue_inspect()
    return queue.paused and not queue.pending
  end)
  MiniTest.expect.equality(session:inspect().current_turn, 0)
  MiniTest.expect.equality(session:queue_inspect().entries[1].display_text, "must stay paused")
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

T["Stop-and-send waits for cancellation and keeps ordinary work paused"] = function()
  local holding, release, released = true, nil, false
  MiniTest.finally(function()
    holding = false
    if release ~= nil and not released then
      release()
    end
  end)
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
  local target = assert(session:inspect().turn_id)
  assert(admit(session, "ordinary waiting"))
  local replacement = { { type = "text", text = "reviewed replacement" } }
  local accepted, failure
  assert(session:stop_and_send(target, replacement, function(id, err)
    accepted, failure = id, err
  end, nil, "reviewed replacement", {}))
  replacement[1].text = "changed after submission"
  wait_for(function()
    return accepted ~= nil or failure ~= nil
  end)
  assert(accepted, failure)
  wait_for(function()
    return #dispatched == 2 and #permissions == 2
  end)
  MiniTest.expect.equality(dispatched[2].prompt[1].text, "reviewed replacement")
  local queue = session:queue_inspect()
  MiniTest.expect.equality(queue.paused, true)
  MiniTest.expect.equality(queue.entries[1].display_text, "ordinary waiting")
  wait_for(function()
    local current = session:queue_inspect()
    return current.replacement ~= nil and current.replacement.status == "dispatched" and not current.pending
  end)
  local store = session.owner.queue_store
  local save = store.save
  -- Hold the real completion write's acknowledgement to order Continue behind it.
  store.save = function(self, id, revision, snapshot, callback)
    if snapshot.state.replacement == nil and not snapshot.state.disposed and release == nil then
      save(self, id, revision, snapshot, function(err)
        if not holding then
          callback(err)
          return
        end
        release = function()
          released = true
          nvim.schedule(function()
            callback(err)
          end)
        end
      end)
    else
      save(self, id, revision, snapshot, callback)
    end
  end
  assert(permissions[2].respond({ outcome = { outcome = "selected", optionId = "allow-once" } }))
  wait_for(function()
    return session:inspect().status == "ready" and release ~= nil
  end)
  MiniTest.expect.equality(#dispatched, 2)
  local stale = session:queue_inspect()
  MiniTest.expect.equality(stale.pending, true)
  MiniTest.expect.equality(stale.replacement.status, "dispatched")
  MiniTest.expect.equality(stale.replacement.cancellation, "acknowledged")
  local done, changed, err
  assert(session:queue_continue(stale.revision, function(ok, failure_message)
    done, changed, err = true, ok, failure_message
  end))
  assert(release)()
  wait_for(function()
    return done
  end)
  MiniTest.expect.equality({ changed, err }, { false, "queue changed; review it again" })
  wait_for(function()
    local current = session:queue_inspect()
    return current.replacement == nil and not current.pending
  end)
  local completed = session:queue_inspect()
  MiniTest.expect.equality(completed.revision, stale.revision + 1)
  MiniTest.expect.equality(completed.paused, true)
  MiniTest.expect.equality(completed.entries[1].display_text, "ordinary waiting")
  assert(change(function(callback)
    return session:queue_continue(completed.revision, callback)
  end))
  wait_for(function()
    return #dispatched == 3
  end)
  MiniTest.expect.equality(dispatched[3].display_text, "ordinary waiting")
end

T["a full queue reserves one replacement and old Stops cannot cancel its new turn"] = function()
  local _, session = fixture(nil, nil, "hold")
  local dispatched = {}
  session:on(function(event)
    if event.type == "prompt_dispatched" then
      dispatched[#dispatched + 1] = event.data
    end
  end)
  local target = assert(session:prompt("active"))
  wait_for(function()
    return #dispatched == 1
  end)
  for i = 1, 20 do
    assert(admit(session, "waiting " .. i))
  end
  local client = assert(session.client)
  local original_cancel, held = client.cancel, nil
  client.cancel = function(_, params)
    held = params
    return true
  end
  local accepted
  assert(session:stop_and_send(target, "replacement", function(id)
    accepted = id
  end))
  wait_for(function()
    return accepted ~= nil and held ~= nil
  end)
  MiniTest.expect.equality(#session:queue_inspect().entries, 20)
  MiniTest.expect.equality(#dispatched, 1)
  wait_for(function()
    return session:queue_inspect().replacement.cancellation == "requested"
  end)
  local second, second_error = session:stop_and_send(target, "competing", function() end)
  MiniTest.expect.equality(second, false)
  assert(second_error ~= nil)
  local continued, continue_error = change(function(callback)
    return session:queue_continue(session:queue_inspect().revision, callback)
  end)
  MiniTest.expect.equality({ continued, continue_error }, { false, "replacement requires review" })
  client.cancel = original_cancel
  assert(original_cancel(client, assert(held)))
  wait_for(function()
    return #dispatched == 2
  end)
  local stopped, stale_error = session:stop(target)
  MiniTest.expect.equality({ stopped, stale_error }, { false, "turn changed; review it again" })
  MiniTest.expect.equality(session:inspect().turn_id, dispatched[2].turn_id)
  MiniTest.expect.equality(session:queue_inspect().paused, true)
  MiniTest.expect.equality(#session:queue_inspect().entries, 20)
end

T["a queued active turn settles before its replacement is dispatched"] = function()
  local _, session = fixture()
  local permissions, dispatched = {}, {}
  session:on(function(event)
    if event.type == "permission_requested" then
      permissions[#permissions + 1] = event
    elseif event.type == "prompt_dispatched" then
      dispatched[#dispatched + 1] = event.data
    end
  end)
  assert(admit(session, "active queued entry"))
  wait_for(function()
    return #permissions == 1
  end)
  local target = assert(session:inspect().turn_id)
  assert(admit(session, "older waiting entry"))
  assert(session:stop_and_send(target, "replacement", function() end))
  wait_for(function()
    return #dispatched == 2 and #permissions == 2
  end)
  MiniTest.expect.equality(dispatched[2].display_text, "replacement")
  MiniTest.expect.equality(session:queue_inspect().entries[1].display_text, "older waiting entry")
  MiniTest.expect.equality(session:queue_inspect().paused, true)
end

T["replacement preserves the configured automatic permission policy"] = function()
  local evaluations = 0
  local policy = {
    name = "fixture-policy",
    evaluate = function()
      evaluations = evaluations + 1
      return evaluations == 1 and "ask" or "allow"
    end,
  }
  local _, session = fixture(nil, nil, "permission", nil, policy)
  local permissions, dispatched = 0, 0
  session:on(function(event)
    if event.type == "permission_requested" then
      permissions = permissions + 1
    elseif event.type == "prompt_dispatched" then
      dispatched = dispatched + 1
    end
  end)
  local target = assert(session:prompt("active"))
  wait_for(function()
    return permissions == 1
  end)
  assert(session:stop_and_send(target, "auto-approved replacement", function() end))
  wait_for(function()
    return dispatched == 2 and session:inspect().status == "ready"
  end)
  MiniTest.expect.equality(evaluations, 2)
  MiniTest.expect.equality(permissions, 1)
  MiniTest.expect.equality(session:queue_inspect().paused, true)
end

T["ordinary completion after a cancellation request leaves the replacement unsent"] = function()
  local _, session = fixture()
  local permission, dispatched
  session:on(function(event)
    if event.type == "permission_requested" then
      permission = event
    elseif event.type == "prompt_dispatched" then
      dispatched = (dispatched or 0) + 1
    end
  end)
  local target = assert(session:prompt("active"))
  wait_for(function()
    return permission ~= nil
  end)
  local client = assert(session.client)
  local original_cancel = client.cancel
  client.cancel = function()
    return true
  end
  local accepted
  assert(session:stop_and_send(target, "must remain unsent", function(id)
    accepted = id
  end))
  wait_for(function()
    local replacement = session:queue_inspect().replacement
    return accepted ~= nil and replacement ~= nil and replacement.status == "unsent"
  end)
  client.cancel = original_cancel
  MiniTest.expect.equality(dispatched, 1)
  MiniTest.expect.equality(session:queue_inspect().replacement.reason, "target did not acknowledge cancellation")
  MiniTest.expect.equality(session:queue_inspect().replacement.cancellation, "completed")
  MiniTest.expect.equality(session:queue_inspect().paused, true)
  local queue = session:queue_inspect()
  assert(change(function(callback)
    return session:queue_cancel(queue.replacement.entry.id, queue.revision, callback)
  end))
  assert(change(function(callback)
    return session:queue_continue(session:queue_inspect().revision, callback)
  end))
end

T["local cancellation before prompt dispatch is not an Agent acknowledgement"] = function()
  local _, session = fixture(nil, nil, "hold")
  local recording = session.owner.recording
  local flush, pending = recording.flush, nil
  recording.flush = function(_, callback)
    pending = callback
  end
  local dispatched = 0
  session:on(function(event)
    if event.type == "prompt_dispatched" then
      dispatched = dispatched + 1
    end
  end)
  local target = assert(session:prompt("never sent"))
  assert(session:stop_and_send(target, "replacement", function() end))
  wait_for(function()
    local replacement = session:queue_inspect().replacement
    return replacement ~= nil and replacement.status == "unsent"
  end)
  recording.flush = flush
  assert(pending)()
  nvim.wait(50)
  MiniTest.expect.equality(dispatched, 0)
  MiniTest.expect.equality(session:queue_inspect().replacement.cancellation, "not_sent")
  MiniTest.expect.equality(
    session:queue_inspect().replacement.reason,
    "target was not sent; cancellation was not acknowledged"
  )
end

T["lost Agent response leaves cancellation unknown and the replacement unsent"] = function()
  local _, session = fixture(nil, nil, "hold")
  local dispatched = 0
  session:on(function(event)
    if event.type == "prompt_dispatched" then
      dispatched = dispatched + 1
    end
  end)
  local target = assert(session:prompt("active"))
  wait_for(function()
    return dispatched == 1
  end)
  local client = assert(session.client)
  client.cancel = function()
    return true
  end
  assert(session:stop_and_send(target, "replacement", function() end))
  wait_for(function()
    local replacement = session:queue_inspect().replacement
    return replacement ~= nil and replacement.cancellation == "requested"
  end)
  client.transport.handle:kill("sigkill")
  wait_for(function()
    local replacement = session:queue_inspect().replacement
    return session:inspect().status == "error"
      and replacement ~= nil
      and replacement.status == "unsent"
      and replacement.cancellation == "unknown"
  end)
  MiniTest.expect.equality(dispatched, 1)
  MiniTest.expect.equality(session:queue_inspect().paused, true)
end

T["Disposal retains the reserved replacement as unsent with unknown cancellation"] = function()
  local _, session = fixture(nil, nil, "hold")
  local dispatched = 0
  session:on(function(event)
    if event.type == "prompt_dispatched" then
      dispatched = dispatched + 1
    end
  end)
  local target = assert(session:prompt("active"))
  wait_for(function()
    return dispatched == 1
  end)
  local client = assert(session.client)
  client.cancel = function()
    return true
  end
  assert(session:stop_and_send(target, "reserved replacement", function() end))
  wait_for(function()
    local replacement = session:queue_inspect().replacement
    return replacement ~= nil and replacement.cancellation == "requested" and not session:queue_inspect().pending
  end)
  local disposed, failure
  assert(session:dispose(nil, function(ok, err)
    disposed, failure = ok, err
  end))
  wait_for(function()
    return disposed ~= nil
  end)
  assert(disposed, failure)
  local queue = session:queue_inspect()
  MiniTest.expect.equality(queue.paused, true)
  MiniTest.expect.equality(queue.replacement.status, "unsent")
  MiniTest.expect.equality(queue.replacement.cancellation, "unknown")
  MiniTest.expect.equality(dispatched, 1)
end

T["Disposal keeps a dispatched replacement outcome unknown"] = function()
  local _, session = fixture(nil, nil, "hold")
  local dispatched = 0
  session:on(function(event)
    if event.type == "prompt_dispatched" then
      dispatched = dispatched + 1
    end
  end)
  local target = assert(session:prompt("active"))
  wait_for(function()
    return dispatched == 1
  end)
  assert(session:stop_and_send(target, "replacement", function() end))
  wait_for(function()
    local queue = session:queue_inspect()
    return dispatched == 2
      and queue.replacement ~= nil
      and queue.replacement.status == "dispatched"
      and not queue.pending
  end)
  local disposed, failure
  assert(session:dispose(nil, function(ok, err)
    disposed, failure = ok, err
  end))
  wait_for(function()
    return disposed ~= nil
  end)
  assert(disposed, failure)
  local queue = session:queue_inspect()
  MiniTest.expect.equality(queue.paused, true)
  MiniTest.expect.equality(queue.uncertain, true)
  MiniTest.expect.equality(queue.replacement.status, "uncertain")
  MiniTest.expect.equality(queue.replacement.cancellation, "acknowledged")
end

T["plain Stop wins while the replacement dispatch fence is awaiting storage"] = function()
  local _, session = fixture(nil, nil, "hold")
  local dispatched = 0
  session:on(function(event)
    if event.type == "prompt_dispatched" then
      dispatched = dispatched + 1
    end
  end)
  local target = assert(session:prompt("active"))
  wait_for(function()
    return dispatched == 1
  end)
  assert(admit(session, "ordinary waiting"))
  local store = session.owner.queue_store
  local save, release = store.save, nil
  store.save = function(owner, id, revision, snapshot, callback)
    if snapshot.state.replacement ~= nil and snapshot.state.replacement.status == "admitting" and release == nil then
      save(owner, id, revision, snapshot, function(err)
        release = function()
          callback(err)
        end
      end)
    else
      save(owner, id, revision, snapshot, callback)
    end
  end
  assert(session:stop_and_send(target, "replacement", function() end))
  wait_for(function()
    return release ~= nil
  end)
  assert(session:stop(target))
  assert(release)()
  wait_for(function()
    local replacement = session:queue_inspect().replacement
    return replacement ~= nil and replacement.status == "unsent" and not session:queue_inspect().pending
  end)
  MiniTest.expect.equality(dispatched, 1)
  MiniTest.expect.equality(session:queue_inspect().entries[1].display_text, "ordinary waiting")
  MiniTest.expect.equality(session:queue_inspect().paused, true)
end

T["plain Stop can veto after replacement admission but before its transport write"] = function()
  local _, session = fixture(nil, nil, "hold")
  local dispatched, target, stopped = 0, nil, false
  session:on(function(event)
    if event.type == "prompt_dispatched" then
      dispatched = dispatched + 1
    elseif
      event.type == "state_changed"
      and event.data.status == "preparing"
      and target ~= nil
      and session:inspect().turn_id ~= target
    then
      stopped = session:stop(target)
    end
  end)
  target = assert(session:prompt("active"))
  wait_for(function()
    return dispatched == 1
  end)
  assert(session:stop_and_send(target, "replacement", function() end))
  wait_for(function()
    local replacement = session:queue_inspect().replacement
    return stopped and replacement ~= nil and replacement.status == "unsent" and not session:queue_inspect().pending
  end)
  MiniTest.expect.equality(dispatched, 1)
  MiniTest.expect.equality(session:queue_inspect().paused, true)
end

T["lost reservation reply retains the proposed replacement without cancelling"] = function()
  local _, session = fixture(nil, nil, "hold")
  local dispatched = 0
  session:on(function(event)
    if event.type == "prompt_dispatched" then
      dispatched = dispatched + 1
    end
  end)
  local target = assert(session:prompt("active"))
  wait_for(function()
    return dispatched == 1
  end)
  assert(admit(session, "waiting"))
  local store, original = session.owner.queue_store, session.owner.queue_store.save
  store.save = function(owner, id, revision, snapshot, callback)
    if snapshot.state.replacement ~= nil then
      original(owner, id, revision, snapshot, function(err)
        assert(err == nil)
        callback("reservation acknowledgement lost")
      end)
    else
      original(owner, id, revision, snapshot, callback)
    end
  end
  local accepted, failure
  assert(session:stop_and_send(target, "retain this replacement", function(id, err)
    accepted, failure = id, err
  end))
  wait_for(function()
    return failure ~= nil
  end)
  local state = session:queue_inspect()
  MiniTest.expect.equality(accepted, nil)
  MiniTest.expect.equality(failure, "reservation acknowledgement lost")
  MiniTest.expect.equality(state.uncertain, true)
  MiniTest.expect.equality(state.uncertain_proposal.replacement.entry.display_text, "retain this replacement")
  MiniTest.expect.equality(session:inspect().status, "prompting")
  MiniTest.expect.equality(dispatched, 1)
end

T["an old Stop cannot retarget a newer turn after a reentrant veto"] = function()
  local _, session = fixture(nil, nil, "hold")
  local target, newer, late_error, attempted, dispatched = nil, nil, nil, false, 0
  session:on(function(event)
    if event.type == "prompt_dispatched" then
      dispatched = dispatched + 1
    elseif
      event.type == "state_changed"
      and event.data.status == "preparing"
      and target ~= nil
      and session:inspect().turn_id ~= target
      and not attempted
    then
      attempted = true
      assert(session:stop(target))
      newer = assert(session:prompt("independent new turn"))
      local stopped
      stopped, late_error = session:stop(target)
      MiniTest.expect.equality(stopped, false)
    end
  end)
  target = assert(session:prompt("active"))
  wait_for(function()
    return dispatched == 1
  end)
  assert(session:stop_and_send(target, "replacement", function() end))
  wait_for(function()
    return newer ~= nil and dispatched == 2
  end)
  MiniTest.expect.equality(late_error, "turn changed; review it again")
  MiniTest.expect.equality(session:inspect().turn_id, newer)
  MiniTest.expect.equality(session:inspect().status, "prompting")
end

T["crash never replays reserved or possibly dispatched replacements"] = function()
  for _, phase in ipairs({ "cancelling", "admitting", "dispatched" }) do
    local directory = nvim.fn.tempname()
    local acp_id = crash_editor(directory, false, false, phase)
    local _, restored = fixture(directory, acp_id)
    local queue = restored:queue_inspect()
    MiniTest.expect.equality(queue.paused, true)
    MiniTest.expect.equality(queue.uncertain, true)
    MiniTest.expect.equality(queue.entries[1].display_text, "saved for review")
    MiniTest.expect.equality(queue.replacement.entry.display_text, "saved replacement")
    MiniTest.expect.equality(queue.replacement.status, phase == "cancelling" and "unsent" or "uncertain")
    MiniTest.expect.equality(queue.replacement.cancellation, phase == "cancelling" and "unknown" or "acknowledged")
    MiniTest.expect.equality(restored:queue_continue(queue.revision, function() end), false)
    nvim.wait(50)
    MiniTest.expect.equality(restored:inspect().current_turn, 0)
  end
end

T["two desktop views keep private drafts when replacements compete"] = function()
  local api, session = fixture(nil, nil, "hold")
  local dispatched = {}
  session:on(function(event)
    if event.type == "prompt_dispatched" then
      dispatched[#dispatched + 1] = event.data
    end
  end)
  local first = assert(Chat.new(api, { markdown_highlighting = false, start_insert_on_switch = false }))
  local second = assert(Chat.new(api, { markdown_highlighting = false, start_insert_on_switch = false }))
  MiniTest.finally(function()
    first:dispose()
    second:dispose()
  end)
  assert(first:attach(session))
  assert(second:attach(session))
  assert(session:prompt("active"))
  wait_for(function()
    return #dispatched == 1
  end)
  local first_view, second_view = first.views[session:inspect().id], second.views[session:inspect().id]
  assert(first:queue_context({ label = "first", text = "first private context" }))
  assert(second:queue_context({ label = "second", text = "second private context" }))
  first_view.renderer:replace_prompt("reviewed first draft")
  second_view.renderer:replace_prompt("private second draft")
  local client = assert(session.client)
  local original_cancel, held = client.cancel, nil
  client.cancel = function(_, params)
    held = params
    return true
  end
  assert(first:stop_and_send())
  wait_for(function()
    return held ~= nil and session:queue_inspect().replacement ~= nil
  end)
  local accepted = second:stop_and_send()
  MiniTest.expect.equality(accepted, nil)
  MiniTest.expect.equality(second_view.renderer:prompt_text(), "private second draft")
  MiniTest.expect.equality(second_view.draft.contexts[1].text, "second private context")
  first_view.renderer:replace_prompt("new first draft")
  client.cancel = original_cancel
  assert(original_cancel(client, assert(held)))
  wait_for(function()
    return #dispatched == 2
  end)
  MiniTest.expect.equality(dispatched[2].prompt, {
    { type = "text", text = "first private context" },
    { type = "text", text = "reviewed first draft" },
  })
  MiniTest.expect.equality(first_view.renderer:prompt_text(), "new first draft")
  MiniTest.expect.equality(second_view.renderer:prompt_text(), "private second draft")
  first:dispose()
  MiniTest.expect.equality(nvim.api.nvim_buf_is_valid(second_view.renderer.buffer), true)
  MiniTest.expect.equality(session:inspect().status, "prompting")
  MiniTest.expect.equality(second_view.renderer:prompt_text(), "private second draft")
end

T["disposing one desktop view does not answer another view's permission"] = function()
  local api, session = fixture()
  local function decisions(visible)
    return Decisions.new({
      is_visible = function()
        return visible
      end,
      is_live = function()
        return true
      end,
      report_error = function() end,
      resolved = function() end,
    })
  end
  local first_decisions, second_decisions = decisions(false), decisions(false)
  local first = assert(
    Chat.new(api, { markdown_highlighting = false, start_insert_on_switch = false, decisions = first_decisions })
  )
  local second = assert(
    Chat.new(api, { markdown_highlighting = false, start_insert_on_switch = false, decisions = second_decisions })
  )
  MiniTest.finally(function()
    first:dispose()
    second:dispose()
    first_decisions:dispose()
    second_decisions:dispose()
  end)
  assert(first:attach(session))
  local permission
  session:on(function(event)
    if event.type == "permission_requested" then
      permission = event
    end
  end)
  assert(session:prompt("request a decision"))
  wait_for(function()
    return permission ~= nil
  end)
  assert(second:attach(session))
  local request_id = permission.data.request_id
  wait_for(function()
    return second_decisions:has_request(session, request_id, false)
  end)
  first:dispose()
  MiniTest.expect.equality(session:inspect().status, "waiting_permission")
  assert(permission.respond({ outcome = { outcome = "selected", optionId = "allow-once" } }))
  wait_for(function()
    return not second_decisions:has_request(session, request_id, false)
  end)
end

T["disposing one desktop view keeps a typed interaction in the other"] = function()
  local api, session = fixture(nil, nil, "echo", nvim.fn.getcwd() .. "/tests/fixtures/grok_ask_user_question.json")
  local function decisions(visible)
    return Decisions.new({
      is_visible = function()
        return visible
      end,
      is_live = function()
        return true
      end,
      report_error = function() end,
      resolved = function() end,
    })
  end
  local first_decisions, second_decisions = decisions(false), decisions(false)
  local first = assert(
    Chat.new(api, { markdown_highlighting = false, start_insert_on_switch = false, decisions = first_decisions })
  )
  local second = assert(
    Chat.new(api, { markdown_highlighting = false, start_insert_on_switch = false, decisions = second_decisions })
  )
  MiniTest.finally(function()
    first:dispose()
    second:dispose()
    first_decisions:dispose()
    second_decisions:dispose()
  end)
  assert(first:attach(session))
  local interaction
  session:on(function(event)
    if event.type == "interaction_requested" then
      interaction = event
    end
  end)
  assert(session:prompt("request an interaction"))
  wait_for(function()
    return interaction ~= nil
  end)
  assert(second:attach(session))
  local request_id = interaction.data.request_id
  wait_for(function()
    return second_decisions:has_request(session, request_id, true)
  end)
  first:dispose()
  MiniTest.expect.equality(#session.interactions, 1)
  assert(interaction.respond({ outcome = "cancelled" }))
  wait_for(function()
    return not second_decisions:has_request(session, request_id, true)
  end)
end

T["a desktop view cannot steal a headless interaction claim"] = function()
  local api, session = fixture(nil, nil, "echo", nvim.fn.getcwd() .. "/tests/fixtures/grok_ask_user_question.json")
  local interaction
  session:on(function(event)
    if event.type == "interaction_requested" then
      assert(event.claim())
      interaction = event
    end
  end)
  assert(session:prompt("request an interaction"))
  wait_for(function()
    return interaction ~= nil
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
  MiniTest.expect.equality(decisions:has_request(session, interaction.data.request_id, true), false)
  assert(interaction.respond({ outcome = "cancelled" }))
end

T["a shared decision controller survives its creating Chat view"] = function()
  local api, session = fixture()
  local first = assert(Chat.new(api, { markdown_highlighting = false, start_insert_on_switch = false }))
  local second = assert(
    Chat.new(api, { markdown_highlighting = false, start_insert_on_switch = false, decisions = first.decisions })
  )
  MiniTest.finally(function()
    first:dispose()
    second:dispose()
  end)
  assert(first:attach(session))
  assert(second:attach(session))
  nvim.api.nvim_set_current_buf(first.views[session:inspect().id].source_buffer)
  local permission
  session:on(function(event)
    if event.type == "permission_requested" then
      permission = event
    end
  end)
  assert(session:prompt("request a decision"))
  wait_for(function()
    return permission ~= nil
  end)
  local request_id = permission.data.request_id
  assert(first.decisions:has_request(session, request_id, false))
  first:dispose()
  nvim.wait(50)
  MiniTest.expect.equality(session:inspect().status, "waiting_permission")
  assert(second.decisions:has_request(session, request_id, false))
  assert(permission.respond({ outcome = { outcome = "selected", optionId = "allow-once" } }))
  MiniTest.expect.equality(second.decisions:has_request(session, request_id, false), false)
end

T["Stop retires permission authority in both desktop views"] = function()
  local api, session = fixture()
  local function decisions()
    return Decisions.new({
      is_visible = function()
        return false
      end,
      is_live = function()
        return true
      end,
      report_error = function() end,
      resolved = function() end,
    })
  end
  local first_decisions, second_decisions = decisions(), decisions()
  local first = assert(
    Chat.new(api, { markdown_highlighting = false, start_insert_on_switch = false, decisions = first_decisions })
  )
  local second = assert(
    Chat.new(api, { markdown_highlighting = false, start_insert_on_switch = false, decisions = second_decisions })
  )
  MiniTest.finally(function()
    first:dispose()
    second:dispose()
    first_decisions:dispose()
    second_decisions:dispose()
  end)
  assert(first:attach(session))
  assert(second:attach(session))
  local permission
  session:on(function(event)
    if event.type == "permission_requested" then
      permission = event
    end
  end)
  local target = assert(session:prompt("active"))
  wait_for(function()
    return permission ~= nil
  end)
  local request_id = permission.data.request_id
  assert(first_decisions:has_request(session, request_id, false))
  assert(second_decisions:has_request(session, request_id, false))
  assert(session:stop(target))
  MiniTest.expect.equality(first_decisions:has_request(session, request_id, false), false)
  MiniTest.expect.equality(second_decisions:has_request(session, request_id, false), false)
  local sent = permission.respond({ outcome = { outcome = "selected", optionId = "allow-once" } })
  MiniTest.expect.equality(sent, false)
  wait_for(function()
    return session:queue_inspect().paused
  end)
  MiniTest.expect.equality(session:queue_inspect().paused, true)
end

T["Session Disposal retires permission authority in every attached view"] = function()
  local api, session = fixture()
  local function decisions()
    return Decisions.new({
      is_visible = function()
        return false
      end,
      is_live = function()
        return true
      end,
      report_error = function() end,
      resolved = function() end,
    })
  end
  local first_decisions, second_decisions = decisions(), decisions()
  local first = assert(
    Chat.new(api, { markdown_highlighting = false, start_insert_on_switch = false, decisions = first_decisions })
  )
  local second = assert(
    Chat.new(api, { markdown_highlighting = false, start_insert_on_switch = false, decisions = second_decisions })
  )
  MiniTest.finally(function()
    first:dispose()
    second:dispose()
    first_decisions:dispose()
    second_decisions:dispose()
  end)
  assert(first:attach(session))
  assert(second:attach(session))
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
  local request_id = permission.data.request_id
  assert(first_decisions:has_request(session, request_id, false))
  assert(second_decisions:has_request(session, request_id, false))
  assert(session:dispose())
  MiniTest.expect.equality(first_decisions:has_request(session, request_id, false), false)
  MiniTest.expect.equality(second_decisions:has_request(session, request_id, false), false)
  local sent = permission.respond({ outcome = { outcome = "selected", optionId = "allow-once" } })
  MiniTest.expect.equality(sent, false)
end

-- A different well-formed boot id proves the old owner is gone (louiselm-sm35e).
T["an owner from a previous boot is dead, missing boot evidence is live"] = function()
  local QueueStore = require("louiselm.session.queue_store")
  local namespace = assert(nvim.uv.fs_readlink("/proc/self/ns/pid"))
  local boot = assert(io.open("/proc/sys/kernel/random/boot_id")):read("*l")
  local function dead(owner)
    local result
    QueueStore.new(nvim.fn.tempname()):owner_dead(owner, function(value)
      result = value
    end)
    wait_for(function()
      return result ~= nil
    end)
    return result
  end
  local previous = boot == "00000000-0000-0000-0000-000000000000" and "11111111-1111-1111-1111-111111111111"
    or "00000000-0000-0000-0000-000000000000"
  local pid = nvim.uv.os_getpid()
  MiniTest.expect.equality(dead({ pid = pid, boot = previous, namespace = namespace }), true)
  MiniTest.expect.equality(dead({ pid = pid, boot = boot, namespace = namespace }), false)
  MiniTest.expect.equality(dead({ pid = pid, namespace = namespace }), false)
  MiniTest.expect.equality(dead({ pid = pid, boot = "not-a-boot-id", namespace = namespace }), false)
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
