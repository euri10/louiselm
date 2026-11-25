local MiniTest = require("mini.test")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local Session = require("louiselm.session")
local Louise = require("louiselm")
local Chat = require("louiselm.ui.chat")
local T = MiniTest.new_set()

T["setup serves a real headless Session and fences disposed incarnations"] = function()
  local root = nvim.fn.getcwd()
  local directory = nvim.fn.tempname()
  local workflow = directory .. "/louiselm/workflow"
  assert(nvim.fn.mkdir(workflow, "p") == 1)
  local capability = "00112233-4455-6677-8899-aabbccddeeff"
  assert(nvim.fn.writefile({ capability }, workflow .. "/operator-capability", "b") == 0)
  local previous_state = nvim.env.XDG_STATE_HOME
  nvim.env.XDG_STATE_HOME = directory
  local listener = assert(nvim.uv.new_pipe(false))
  assert(listener:bind(workflow .. "/phone-control.sock") == 0)
  local peer ---@type table?
  local buffer, responses = "", {}
  assert(listener:listen(1, function(err)
    assert(not err)
    peer = assert(nvim.uv.new_pipe(false))
    assert(listener:accept(peer) == 0)
    peer:read_start(function(read_err, data)
      assert(not read_err)
      if data == nil then
        return
      end
      buffer = buffer .. data
      while buffer:find("\n", 1, true) do
        local ending = buffer:find("\n", 1, true)
        local message = nvim.json.decode(buffer:sub(1, ending - 1))
        buffer = buffer:sub(ending + 1)
        if message.type == "attach" then
          assert(message.capability == capability)
          peer:write(nvim.json.encode({ v = 1, type = "attached", controller_id = string.rep("1", 32) }) .. "\n")
        else
          responses[#responses + 1] = message
        end
      end
    end)
  end) == 0)
  local definition = {
    provider = "test-service",
    command = nvim.v.progpath,
    env = { LOUISELM_MOCK_MODE = "static" },
    args = {
      "--headless",
      "--noplugin",
      "-u",
      root .. "/tests/mock/init.lua",
      "-c",
      "lua require('louiselm.dev.mock_agent').run()",
    },
  }
  local api, replacement_api, chat
  MiniTest.finally(function()
    if chat ~= nil then
      chat:dispose()
    end
    if replacement_api ~= nil then
      assert(replacement_api:dispose())
      local finished = false
      replacement_api:flush_recording(function()
        finished = true
      end)
      assert(nvim.wait(6000, function()
        return finished
      end))
    end
    if api ~= nil then
      assert(api:dispose())
      local finished = false
      api:flush_recording(function()
        finished = true
      end)
      assert(nvim.wait(6000, function()
        return finished
      end))
    end
    assert(Louise.setup({}))
    if peer ~= nil and not peer:is_closing() then
      peer:close()
    end
    if not listener:is_closing() then
      listener:close()
    end
    nvim.env.XDG_STATE_HOME = previous_state
    nvim.fn.delete(directory, "rf")
  end)
  assert(Louise.setup({
    phone_control = { enabled = true },
    capture = { service = { root .. "/tests/fixtures/phone_control_verify_stub.sh" } },
    agents = { mock = definition },
  }))
  api = assert(Session.new({ mock = definition }, nil, { usage_directory = directory .. "/usage" }))
  local session = assert(api:create_session("mock", { cwd = root }))
  assert(nvim.wait(6000, function()
    return session:inspect().status == "ready" and Louise.phone_control_subject(session) ~= nil
  end))
  local identity = assert(Louise.phone_control_subject(session))
  local done = false
  assert(session:prompt("sent from headless", function(_, err)
    assert(err == nil, err)
    done = true
  end))
  assert(nvim.wait(6000, function()
    return done
  end))
  local view
  local ordering, dispatched_prompt = {}, nil
  session:on(function(event)
    if event.type == "state_changed" and event.data.status == "ready" and session:inspect().current_turn == 2 then
      -- The Session becomes ready before Chat handles its scheduled ready event.
      nvim.schedule(function()
        assert(chat:queue_context({ label = "private", text = "UNSENT CONTEXT" }))
        view.renderer:replace_prompt("UNSENT DRAFT")
        view.draft:queue("UNSENT QUEUED")
        ordering[#ordering + 1] = "queued"
      end)
    elseif event.type == "prompt_dispatched" and event.data.display_text == "UNSENT QUEUED" then
      ordering[#ordering + 1] = "dispatched"
      dispatched_prompt = event.data.prompt
    end
  end)
  chat = assert(Chat.new(api, { agents = { "mock" }, start_insert_on_switch = false }))
  assert(chat:attach(session))
  view = chat.views[session:inspect().id]
  assert(chat:queue_context({ label = "sent", text = "HIDDEN SENT CONTEXT" }))
  assert(chat:submit("visible typed message"))
  assert(nvim.wait(6000, function()
    return session:inspect().current_turn == 3 and session:inspect().status == "ready"
  end))
  local chat_idle = false
  nvim.schedule(function()
    chat_idle = true
  end)
  assert(nvim.wait(6000, function()
    return chat_idle
  end))
  assert(chat:queue_context({ label = "still private", text = "PRIVATE STAGED CONTEXT" }))
  assert(view.draft:select_skill({
    name = "private-native-skill",
    description = "private selection",
    explicit_only = false,
    path = "/not-read/SKILL.md",
    content = "PRIVATE NATIVE SKILL",
  }, true) == nil)
  view.renderer:replace_prompt("PRIVATE DESKTOP DRAFT")
  view.draft:queue("PRIVATE WAITING QUEUE")
  local request = {
    v = 1,
    type = "read_session",
    controller_id = identity.controller_id,
    grant = { projects = { assert(nvim.uv.fs_realpath(root)) }, agents = { "mock" }, groups = { "read" } },
    proof = {},
    period = {},
    request = {
      controller_id = identity.controller_id,
      subject_id = identity.subject_id,
      operation_id = string.rep("b", 32),
      action = 1,
      payload = "00000000000000000014",
      expected_revision = 0,
    },
  }
  assert(peer):write(nvim.json.encode(request) .. "\n")
  assert(nvim.wait(6000, function()
    return #responses == 1
  end))
  MiniTest.expect.equality(responses[1].type, "session_page")
  MiniTest.expect.equality(responses[1].entries[1].text, "sent from headless")
  MiniTest.expect.equality(responses[1].entries[2].role, "assistant")
  MiniTest.expect.equality(ordering, { "queued", "dispatched" })
  MiniTest.expect.no_equality(nvim.json.encode(dispatched_prompt):find("UNSENT CONTEXT", 1, true), nil)
  MiniTest.expect.equality(responses[1].entries[5].text, "UNSENT QUEUED")
  MiniTest.expect.equality(nvim.json.encode(responses[1]):find("UNSENT CONTEXT", 1, true), nil)
  MiniTest.expect.equality(nvim.json.encode(responses[1]):find("UNSENT DRAFT", 1, true), nil)
  MiniTest.expect.equality(nvim.json.encode(responses[1]):find("HIDDEN SENT CONTEXT", 1, true), nil)
  MiniTest.expect.equality(responses[1].entries[3].text, "visible typed message")
  local private = {
    "PRIVATE STAGED CONTEXT",
    "PRIVATE DESKTOP DRAFT",
    "PRIVATE WAITING QUEUE",
    "PRIVATE NATIVE SKILL",
    "private-native-skill",
  }
  for _, marker in ipairs(private) do
    MiniTest.expect.equality(nvim.json.encode(responses[1]):find(marker, 1, true), nil)
  end
  MiniTest.expect.equality(view.draft.contexts[1].text, private[1])
  MiniTest.expect.equality(view.renderer:prompt_text(), private[2])
  MiniTest.expect.equality(view.draft.queued_prompt, private[3])
  MiniTest.expect.equality(assert(view.draft.pending_skill).content, private[4])

  request.request.payload = "00000000000000000002"
  local pages, queued_visible = 0, false
  repeat
    local response_index = #responses + 1
    assert(peer):write(nvim.json.encode(request) .. "\n")
    assert(nvim.wait(6000, function()
      return #responses == response_index
    end))
    local page = responses[response_index]
    MiniTest.expect.equality(page.type, "session_page")
    for _, marker in ipairs(private) do
      MiniTest.expect.equality(nvim.json.encode(page):find(marker, 1, true), nil)
    end
    for _, entry in ipairs(page.entries) do
      queued_visible = queued_visible or entry.text == "UNSENT QUEUED"
    end
    pages = pages + 1
    request.request.expected_revision = page.revision
    request.request.payload = string.format("%016x%04x", page.next_before, 2)
  until page.next_before == 0
  MiniTest.expect.no_equality(pages, 1)
  MiniTest.expect.equality(queued_visible, true)
  MiniTest.expect.equality(session:inspect().current_turn, 3)
  MiniTest.expect.equality(view.draft.contexts[1].text, private[1])
  MiniTest.expect.equality(view.renderer:prompt_text(), private[2])
  MiniTest.expect.equality(view.draft.queued_prompt, private[3])
  MiniTest.expect.equality(assert(view.draft.pending_skill).content, private[4])
  assert(session:dispose())
  local response_index = #responses + 1
  assert(peer):write(nvim.json.encode(request) .. "\n")
  assert(nvim.wait(6000, function()
    return #responses == response_index
  end))
  MiniTest.expect.equality(responses[response_index].code, "stale_subject")
  replacement_api = assert(Session.new({ mock = definition }, nil, { usage_directory = directory .. "/usage2" }))
  local replacement = assert(replacement_api:create_session("mock", { cwd = root }))
  assert(nvim.wait(6000, function()
    return replacement:inspect().status == "ready"
  end))
  MiniTest.expect.equality(replacement:inspect().id, "session-1")
  MiniTest.expect.no_equality(assert(Louise.phone_control_subject(replacement)).subject_id, identity.subject_id)
end

return T
