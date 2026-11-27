-- Both Sessions start idle. Only the Android Activity supplies a prompt.
---@diagnostic disable-next-line: undefined-global -- Neovim supplies the fixture runtime.
local nvim = vim
local root = assert(nvim.env.LOUISELM_TEST_PROJECT)
local directory = assert(nvim.env.LOUISELM_TEST_RECEIPTS)
nvim.opt.runtimepath:prepend(root)
local Louise = require("louiselm")
local Session = require("louiselm.session")
local Chat = require("louiselm.ui.chat")
local agent = {
  provider = "test-service",
  command = "/bin/sh",
  args = {
    "-c",
    'tee "$LOUISELM_FIXTURE_WIRE" | "$@"',
    "phone-send-test-agent",
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
  phone_control = { enabled = true },
  capture = { service = { assert(nvim.env.LOUISELM_TEST_CAPTURE) } },
  agents = { codex = agent },
}))
local api = assert(Session.new({ codex = agent }, nil, { usage_directory = directory .. "/usage" }))
local function create(label)
  return assert(api:create_session("codex", {
    cwd = root,
    name = "Same name",
    env = { LOUISELM_FIXTURE_WIRE = directory .. "/" .. label },
  }))
end
local selected, other = create("selected"), create("other")
assert(
  nvim.wait(15000, function()
    return selected:inspect().status == "ready"
      and other:inspect().status == "ready"
      and Louise.phone_control_subject(selected) ~= nil
      and Louise.phone_control_subject(other) ~= nil
  end),
  "two ready Sessions must be discoverable"
)
local chat = assert(Chat.new(api, { agents = { "codex" }, start_insert_on_switch = false }))
assert(chat:attach(selected))
local renderer = chat.views[selected:inspect().id].renderer
local draft = "Desktop draft still being edited"
renderer:replace_prompt(draft)
local identity = assert(Louise.phone_control_subject(selected))
local function report(value)
  io.stdout:write(nvim.json.encode(value), "\n")
  io.stdout:flush()
end
report({
  controller_id = identity.controller_id,
  subject_id = identity.subject_id,
  other_subject_id = assert(Louise.phone_control_subject(other)).subject_id,
})

-- These pipes contain only traffic between the disposable test Sessions and
-- their echo Agents. Count the actual ACP messages separately for each Agent.
local expected = assert(nvim.env.LOUISELM_TEST_SEND_TEXT)
local function received(label)
  local count = 0
  for _, line in ipairs(nvim.fn.readfile(directory .. "/" .. label)) do
    local message = nvim.json.decode(line)
    if message.method == "session/prompt" then
      assert(nvim.deep_equal(message.params.prompt, { { type = "text", text = expected } }), "unexpected ACP prompt")
      count = count + 1
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
  end
end)
assert(
  nvim.wait(600000, function()
    return finished
  end),
  "Android gate did not finish"
)
input:close()
assert(
  nvim.wait(10000, function()
    return selected:inspect().status == "ready"
  end),
  "the selected Agent did not finish its reply"
)
local lines = nvim.api.nvim_buf_get_lines(renderer.buffer, 0, -1, false)
local mentions = 0
for _, line in ipairs(lines) do
  if line:find(expected, 1, true) then
    mentions = mentions + 1
  end
end
report({
  selected_prompts = received("selected"),
  other_prompts = received("other"),
  selected_turns = selected:inspect().current_turn,
  other_turns = other:inspect().current_turn,
  transcript_mentions = mentions,
  draft_preserved = renderer:prompt_text() == draft,
})
chat:dispose()
assert(api:dispose())
nvim.cmd("qa!")
