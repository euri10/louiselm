-- Two disposable real Sessions; record every ACP prompt at explicit checkpoints.
---@diagnostic disable-next-line: undefined-global -- Neovim supplies the fixture runtime.
local nvim = vim
local root = assert(nvim.env.LOUISELM_TEST_PROJECT)
local directory = assert(nvim.env.LOUISELM_TEST_RECEIPTS)
local sync = assert(nvim.env.LOUISELM_VOICE_SEND_SYNC)
nvim.opt.runtimepath:prepend(root)
local Louise = require("louiselm")
local Session = require("louiselm.session")
local Chat = require("louiselm.ui.chat")
local agent = {
  provider = "test-service",
  command = "/usr/bin/python3",
  args = {
    root .. "/tests/fixtures/phone_send_agent.py",
    nvim.v.progpath,
    "--headless",
    "--noplugin",
    "-u",
    root .. "/tests/mock/init.lua",
    "-c",
    "lua require('louiselm.dev.mock_agent').run()",
  },
}
assert(Louise.setup({
  capture = { service = { assert(nvim.env.LOUISELM_TEST_CAPTURE) } },
  agents = { codex = agent },
}))
local api = assert(Session.new({ codex = agent }, nil, { usage_directory = directory .. "/usage" }))
local processes = {}
local chat
local function create(label)
  local session = assert(api:create_session("codex", {
    cwd = root,
    name = label == "selected" and "Phone Send target" or "Second phone Session",
    env = { LOUISELM_FIXTURE_WIRE = directory .. "/" .. label },
  }))
  processes[#processes + 1] = session.client.transport.handle
  return session
end
local function report(value)
  io.stdout:write(nvim.json.encode(value), "\n")
  io.stdout:flush()
end
local function run()
  local selected, other = create("selected"), create("other")
  assert(
    nvim.wait(15000, function()
      return selected:inspect().status == "ready"
        and other:inspect().status == "ready"
        and Louise.phone_control_subject(selected) ~= nil
        and Louise.phone_control_subject(other) ~= nil
    end),
    "Sessions did not become ready"
  )
  assert(selected:prompt("Desktop seed for selected context"))
  assert(
    nvim.wait(15000, function()
      return selected:inspect().status == "ready"
    end),
    "desktop seed did not complete"
  )
  chat = assert(Chat.new(api, { agents = { "codex" }, start_insert_on_switch = false }))
  assert(chat:attach(selected))
  local renderer = chat.views[selected:inspect().id].renderer
  renderer:replace_prompt("Desktop draft still being edited")
  local identity = assert(Louise.phone_control_subject(selected))
  report({
    controller_id = identity.controller_id,
    subject_id = identity.subject_id,
    other_subject_id = assert(Louise.phone_control_subject(other)).subject_id,
  })
  local function received(label)
    local count = 0
    for _, line in ipairs(nvim.fn.readfile(directory .. "/" .. label)) do
      local message = nvim.json.decode(line)
      if message.method == "session/prompt" then
        count = count + 1
        if label == "selected" then
          local expected = count == 1 and "Desktop seed for selected context"
            or table.concat(nvim.fn.readfile(sync .. "/reviewed", "b"), "\n")
          assert(
            count <= 2 and nvim.deep_equal(message.params.prompt, { { type = "text", text = expected } }),
            "unexpected selected Session prompt"
          )
        else
          local expected = table.concat(nvim.fn.readfile(sync .. "/other-reviewed", "b"), "\n")
          assert(
            count == 1 and nvim.deep_equal(message.params.prompt, { { type = "text", text = expected } }),
            "unexpected other Session prompt"
          )
        end
      end
    end
    return count
  end
  local finished = false
  local input = assert(nvim.uv.new_pipe(false))
  assert(input:open(0) == 0)
  input:read_start(function(err, data)
    assert(not err, err)
    if data == nil or data:find("stop", 1, true) then
      finished = true
      return
    end
    local selected_count, other_count = data:match("^check (%d+) (%d+)")
    local expected = assert(tonumber(selected_count))
    local expected_other = assert(tonumber(other_count))
    nvim.schedule(function()
      assert(
        nvim.wait(15000, function()
          return received("selected") >= expected
            and received("other") >= expected_other
            and selected:inspect().status == "ready"
            and other:inspect().status == "ready"
        end),
        "expected prompt did not complete"
      )
      local current = assert(Louise.phone_control_subject(selected))
      local current_other = assert(Louise.phone_control_subject(other))
      report({
        selected = received("selected"),
        other = received("other"),
        controller_id = current.controller_id,
        subject_id = current.subject_id,
        other_controller_id = current_other.controller_id,
        other_subject_id = current_other.subject_id,
        draft_preserved = renderer:prompt_text() == "Desktop draft still being edited",
      })
    end)
  end)
  assert(
    nvim.wait(600000, function()
      return finished
    end),
    "Android fixture did not finish"
  )
  input:close()
end
local ok, failure = pcall(run)
local chat_ok, chat_error = true, nil
if chat ~= nil then
  chat_ok, chat_error = pcall(chat.dispose, chat)
end
local disposed, dispose_error
local started, start_error = api:dispose(function(result, err)
  disposed, dispose_error = result, err
end)
local cleaned = nvim.wait(10000, function()
  if disposed == nil then
    return false
  end
  for _, process in ipairs(processes) do
    if not process:is_closing() then
      return false
    end
  end
  return true
end)
if not ok then
  error(failure)
end
assert(chat_ok and chat_error, "chat disposal failed")
assert(started, start_error)
assert(cleaned and disposed, dispose_error or "echo Agents did not exit")
nvim.cmd("qa!")
