-- Isolated Session owner driven by the pinned receiver/Android settings test.
---@diagnostic disable-next-line: undefined-global -- Neovim provides the fixture runtime.
local nvim = vim
local root = assert(nvim.env.LOUISELM_TEST_PROJECT)
nvim.opt.runtimepath:prepend(root)
local Session = require("louiselm.session")
local Louise = require("louiselm")
local Chat = require("louiselm.ui.chat")
local options = {
  {
    id = "model",
    name = "Model",
    category = "model",
    type = "select",
    currentValue = "a",
    options = {
      { value = "a", name = "Model A" },
      { value = "b", name = "Model B" },
    },
  },
  {
    id = "effort",
    name = "Effort",
    category = "thought_level",
    type = "select",
    currentValue = "high",
    options = {
      { value = "high", name = "High" },
      { value = "medium", name = "Medium" },
    },
  },
  {
    id = "access",
    name = "Access",
    category = "mode",
    type = "select",
    currentValue = "full",
    options = {
      { value = "full", name = "Full access" },
      { value = "safe", name = "Ask first" },
    },
  },
  {
    id = "private",
    name = "Private",
    category = "other",
    type = "select",
    currentValue = "secret",
    options = {
      { value = "secret", name = "PRIVATE CONFIG" },
    },
  },
}
local missing = {
  {
    id = "model",
    name = "Model",
    category = "model",
    type = "select",
    currentValue = "c",
    options = {
      { value = "c", name = "Other model" },
    },
  },
}
local function definition(advertised, mode)
  return {
    provider = "test-service",
    command = nvim.v.progpath,
    args = {
      "--headless",
      "--noplugin",
      "-u",
      root .. "/tests/mock/init.lua",
      "-c",
      "lua require('louiselm.dev.mock_agent').run("
        .. nvim.inspect({ config_options = advertised, mode = mode })
        .. ")",
    },
  }
end
local agents = { codex = definition(options), missing = definition(missing), queue = definition({}, "hold") }
assert(Louise.setup({
  phone_control = { enabled = true },
  capture = { service = { assert(nvim.env.LOUISELM_TEST_CAPTURE) } },
  agents = agents,
}))
local api = assert(Session.new(agents, nil, { usage_directory = nvim.env.LOUISELM_TEST_USAGE }))
local session = assert(api:create_session("codex", { cwd = root, name = "Settings" }))
local other = assert(api:create_session("missing", { cwd = root, name = "Missing settings" }))
local queued = assert(api:create_session("queue", { cwd = root, name = "Desktop queue" }))
assert(nvim.wait(10000, function()
  return session:inspect().status == "ready"
    and other:inspect().status == "ready"
    and queued:inspect().status == "ready"
    and Louise.phone_control_subject(session) ~= nil
    and Louise.phone_control_subject(other) ~= nil
    and Louise.phone_control_subject(queued) ~= nil
end))
local chat = assert(Chat.new(api, { agents = { "queue" }, start_insert_on_switch = false }))
assert(chat:attach(queued))
assert(chat:submit("Desktop active"))
assert(nvim.wait(10000, function()
  return queued:queue_inspect().active_status == "dispatched"
end))
assert(chat:submit("Desktop first"))
assert(chat:submit("Desktop second"))
assert(nvim.wait(10000, function()
  return #queued:queue_inspect().entries == 2 and not queued:queue_inspect().pending
end))
assert(chat:queue_context({ label = "private", text = "PRIVATE QUEUE CONTEXT" }))
chat.views[queued:inspect().id].renderer:replace_prompt("PRIVATE QUEUE DRAFT")
local target = assert(Louise.phone_control_subject(session))
io.stdout:write(
  nvim.json.encode({
    controller_id = target.controller_id,
    subject_id = target.subject_id,
    other_subject_id = assert(Louise.phone_control_subject(other)).subject_id,
    queue_subject_id = assert(Louise.phone_control_subject(queued)).subject_id,
  }),
  "\n"
)
io.stdout:flush()
local closed = false
local input = assert(nvim.uv.new_pipe(false))
assert(input:open(0) == 0)
input:read_start(function(err, data)
  assert(not err, err)
  if data == nil or data:find("stop", 1, true) then
    closed = true
  elseif data:find("change", 1, true) then
    nvim.schedule(function()
      local changes = { { "model", "b" }, { "effort", "medium" }, { "access", "safe" } }
      local function apply(index)
        if index > #changes then
          local state = queued:queue_inspect()
          assert(queued:queue_move(state.entries[2].id, 1, state.revision, function(ok, failure)
            assert(ok, failure)
            assert(queued:cancel())
            local function settled()
              local current = queued:queue_inspect()
              if current.paused and not current.pending and queued:inspect().status == "ready" then
                io.stdout:write("acknowledged\n")
                io.stdout:flush()
              else
                nvim.defer_fn(settled, 10)
              end
            end
            settled()
          end))
          return
        end
        assert(session:set_config_option(changes[index][1], changes[index][2], function(_, failure)
          assert(failure == nil, failure)
          apply(index + 1)
        end))
      end
      apply(1)
    end)
  end
end)
assert(nvim.wait(300000, function()
  return closed
end))
input:close()
chat:dispose()
assert(api:dispose())
nvim.cmd("qa!")
