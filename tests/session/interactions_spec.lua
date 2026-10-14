local MiniTest = require("mini.test")
local Session = require("louiselm.session")
local Protocol = require("louiselm.acp.protocol")
local Fixtures = dofile("tests/fixtures/grok_interactions.lua")

---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local T = MiniTest.new_set()
local original_system = nvim.system

local function harness()
  -- mini.test finalizers run FIFO; repeated harnesses must restore the same
  -- process boundary, never a previous harness's mock.
  local original = original_system
  local process
  rawset(nvim, "system", function(command, options, on_exit)
    if command[1] == "sqlite3" then
      return original(command, options, on_exit)
    end
    process = { options = options, on_exit = on_exit, writes = {} }
    return {
      write = function(_, data)
        if data == nil then
          return
        end
        process.writes[#process.writes + 1] = assert(Protocol.decode(data:sub(1, -2)))
      end,
      kill = function() end,
      is_closing = function()
        return false
      end,
    }
  end)
  MiniTest.finally(function()
    nvim.system = original
  end)
  local directory = nvim.fn.tempname()
  local api = assert(Session.new({ grok = { command = "test-grok", provider = "test" } }, nil, {
    usage_directory = directory,
  }))
  MiniTest.finally(function()
    assert(api:dispose())
    local done = false
    api:flush_recording(function(err)
      assert(err == nil)
      done = true
    end)
    assert(nvim.wait(6000, function()
      return done
    end, 10))
    nvim.fn.delete(directory, "rf")
  end)
  local session = assert(api:create_session("grok", { cwd = "/tmp", schedule = function() end }))
  local function receive(message)
    process.options.stdout(nil, assert(Protocol.encode(message)) .. "\n")
  end
  receive(Protocol.response(1, { protocolVersion = 1, agentCapabilities = {} }))
  receive(Protocol.response(2, { sessionId = "grok-acp" }))
  assert(nvim.wait(6000, function()
    return session:inspect().status == "ready"
  end, 10))
  local function request(id, kind, wrapped, params)
    local method = "x.ai/" .. (kind == "question" and "ask_user_question" or "exit_plan_mode")
    params = nvim.deepcopy(params or Fixtures[kind])
    receive(
      assert(
        Protocol.request(
          id,
          wrapped and "_" .. method or method,
          wrapped and { method = method, params = params } or params
        )
      )
    )
  end
  local function replies(id)
    local result = {}
    for _, message in ipairs(process.writes) do
      if message.id == id and message.method == nil then
        result[#result + 1] = message
      end
    end
    return result
  end
  local function prompt()
    assert(session:prompt("test"))
    assert(nvim.wait(6000, function()
      return session:inspect().status == "prompting"
    end, 10))
    for _, message in ipairs(process.writes) do
      if message.method == "session/prompt" then
        return message.id
      end
    end
    error("missing prompt dispatch")
  end
  return session, process, request, replies, receive, prompt
end

T["publishes validated direct and wrapped interactions with exact response shapes"] = function()
  local session, _, request, replies = harness()
  local events = {}
  session:on(function(event)
    if event.type == "interaction_requested" then
      assert(event.claim())
      events[#events + 1] = event
    end
  end)
  for index, wrapped in ipairs({ false, true }) do
    request(index, "question", wrapped)
    MiniTest.expect.equality(#events, index)
    local event = events[index]
    MiniTest.expect.equality(event.data.kind, "question")
    MiniTest.expect.equality(event.data.questions[2].multi_select, true)
    local answer = {
      outcome = "accepted",
      answers = {
        ["Which database?"] = { "Redis" },
        ["Which features?"] = { "Fast, simple", "Durable" },
        ["Anything else?"] = { "Other" },
      },
      annotations = {
        ["Which database?"] = { preview = "  <div>redis preview</div>\n" },
        ["Anything else?"] = { notes = "Keep it small" },
      },
    }
    assert(event.respond(answer))
    MiniTest.expect.equality(replies(index)[1].result, answer)
    MiniTest.expect.equality(event.respond(answer), false)
    MiniTest.expect.equality(#replies(index), 1)
  end
end

T["headless and observational listeners cancel both kinds without approving a plan"] = function()
  local session, _, request, replies = harness()
  local seen = 0
  session:on(function(event)
    if event.type == "interaction_requested" then
      seen = seen + 1
    end
  end)
  for index, kind in ipairs({ "question", "plan_review" }) do
    request(index, kind, true)
    MiniTest.expect.equality(replies(index)[1].result, { outcome = "cancelled" })
  end
  MiniTest.expect.equality(seen, 2)
end

T["invalid request data is refused before publishing an event"] = function()
  local session, _, request, replies = harness()
  local events = 0
  session:on(function(event)
    if event.type == "interaction_requested" then
      events = events + 1
    end
  end)
  local mutations = {
    function(p)
      p.sessionId = "another-session"
    end,
    function(p)
      p.toolCallId = false
    end,
    function(p)
      p.mode = "unknown"
    end,
    function(p)
      p.questions = { [2] = p.questions[1], [5] = p.questions[2] }
    end,
    function(p)
      p.questions[1].multiSelect = "yes"
    end,
    function(p)
      p.questions[1].options[1].preview = {}
    end,
    function(p)
      p.questions[1].options[1].description = nil
    end,
  }
  for index, mutate in ipairs(mutations) do
    local params = nvim.deepcopy(Fixtures.question)
    mutate(params)
    request(index, "question", true, params)
    MiniTest.expect.equality(replies(index)[1].error.code, -32602)
  end
  request(20, "plan_review", false, { sessionId = "grok-acp", toolCallId = "plan", planContent = false })
  MiniTest.expect.equality(replies(20)[1].error.code, -32602)
  MiniTest.expect.equality(events, 0)
end

T["completion failure cancellation disposal and exit retire requests exactly once"] = function()
  for _, terminal in ipairs({ "complete", "failure", "cancel", "dispose", "exit" }) do
    local session, process, request, replies, receive, prompt = harness()
    local prompt_id = prompt()
    local pending, cancelled = {}, {}
    session:on(function(event)
      if event.type == "interaction_requested" then
        assert(event.claim())
        pending[#pending + 1] = event
      elseif event.type == "interaction_cancelled" then
        cancelled[#cancelled + 1] = event.data.request_ids
      end
    end)
    request(71, "question", false)
    request(72, "plan_review", true)
    if terminal == "complete" then
      receive(Protocol.response(prompt_id, { stopReason = "end_turn" }))
    elseif terminal == "failure" then
      receive({ jsonrpc = "2.0", id = prompt_id, error = { code = -32603, message = "turn failed" } })
    elseif terminal == "cancel" then
      assert(session:cancel())
    elseif terminal == "dispose" then
      assert(session:dispose())
    else
      process.on_exit({ code = 1, signal = 0 })
    end
    assert(nvim.wait(1000, function()
      return #cancelled > 0
    end, 10))
    local expected_writes = terminal == "exit" and 0 or 1
    if expected_writes == 1 then
      MiniTest.expect.equality(replies(71)[1].result, { outcome = "cancelled" })
      MiniTest.expect.equality(replies(72)[1].result, { outcome = "cancelled" })
    end
    MiniTest.expect.equality(cancelled, { { 71, 72 } })
    for _, event in ipairs(pending) do
      MiniTest.expect.equality(event.respond({ outcome = "accepted", answers = {} }), false)
      MiniTest.expect.equality(event.claim(), false)
    end
    session:dispose()
    MiniTest.expect.equality(#replies(71), expected_writes)
    MiniTest.expect.equality(#replies(72), expected_writes)
  end
end

T["a fast-event request queued before prompt completion cannot reopen a finished turn"] = function()
  local session, _, request, replies, receive, prompt = harness()
  local prompt_id = prompt()
  local seen = 0
  session:on(function(event)
    if event.type == "interaction_requested" then
      seen = seen + 1
      assert(event.claim())
    end
  end)
  local timer = assert(nvim.uv.new_timer())
  timer:start(0, 0, function()
    assert(nvim.in_fast_event())
    request(83, "plan_review", true)
    receive(Protocol.response(prompt_id, { stopReason = "end_turn" }))
    timer:close()
  end)
  assert(nvim.wait(1000, function()
    return #replies(83) > 0 or seen > 0
  end, 5))
  MiniTest.expect.equality(seen, 0)
  MiniTest.expect.equality(replies(83)[1].result, { outcome = "cancelled" })
  MiniTest.expect.equality(session:inspect().status, "ready")
end

T["disposing from cancellation delivery cannot revive the Session"] = function()
  for _, terminal in ipairs({ "complete", "cancel" }) do
    local session, _, request, replies, receive, prompt = harness()
    local prompt_id = prompt()
    session:on(function(event)
      if event.type == "interaction_requested" then
        assert(event.claim())
      elseif event.type == "interaction_cancelled" then
        assert(session:dispose())
      end
    end)
    request(84, "question", false)
    if terminal == "complete" then
      receive(Protocol.response(prompt_id, { stopReason = "end_turn" }))
    else
      assert(session:cancel())
    end
    MiniTest.expect.equality(session:inspect().status, "disposed")
    MiniTest.expect.equality(#replies(84), 1)
  end
end

T["synchronous answers work without claiming and invalid asynchronous answers stay pending"] = function()
  local session, _, request, replies = harness()
  local pending
  session:on(function(event)
    if event.type ~= "interaction_requested" then
      return
    end
    if event.data.kind == "plan_review" then
      assert(event.respond({ outcome = "approved" }))
    else
      assert(event.claim())
      MiniTest.expect.equality(event.claim(), false)
      pending = event
    end
  end)
  request(91, "plan_review", true)
  MiniTest.expect.equality(replies(91)[1].result, { outcome = "approved" })
  request(92, "question", false)
  local event = assert(pending)
  event.data.questions[1].options[1].preview = "changed by consumer"
  local sent, err = event.respond({
    outcome = "accepted",
    answers = { ["Which database?"] = { "Redis" } },
    annotations = { ["Which database?"] = { preview = "changed by consumer" } },
  })
  MiniTest.expect.equality(sent, false)
  MiniTest.expect.equality(err, "preview must match the selected single option verbatim")
  MiniTest.expect.equality(replies(92), {})
  assert(event.respond({ outcome = "cancelled" }))
  MiniTest.expect.equality(#replies(92), 1)
end

return T
