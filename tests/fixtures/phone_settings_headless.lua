-- Isolated Session owner driven by the pinned receiver/Android settings test.
---@diagnostic disable-next-line: undefined-global -- Neovim provides the fixture runtime.
local nvim = vim
local root = assert(nvim.env.LOUISELM_TEST_PROJECT)
nvim.opt.runtimepath:prepend(root)
local Session = require("louiselm.session")
local Louise = require("louiselm")
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
local function definition(advertised)
  return {
    provider = "test-service",
    command = nvim.v.progpath,
    args = {
      "--headless",
      "--noplugin",
      "-u",
      root .. "/tests/mock/init.lua",
      "-c",
      "lua require('louiselm.dev.mock_agent').run(" .. nvim.inspect({ config_options = advertised }) .. ")",
    },
  }
end
local agents = { codex = definition(options), missing = definition(missing) }
assert(Louise.setup({
  phone_control = { enabled = true },
  capture = { service = { assert(nvim.env.LOUISELM_TEST_CAPTURE) } },
  agents = agents,
}))
local api = assert(Session.new(agents, nil, { usage_directory = nvim.env.LOUISELM_TEST_USAGE }))
local session = assert(api:create_session("codex", { cwd = root, name = "Settings" }))
local other = assert(api:create_session("missing", { cwd = root, name = "Missing settings" }))
assert(nvim.wait(10000, function()
  return session:inspect().status == "ready"
    and other:inspect().status == "ready"
    and Louise.phone_control_subject(session) ~= nil
    and Louise.phone_control_subject(other) ~= nil
end))
local target = assert(Louise.phone_control_subject(session))
io.stdout:write(
  nvim.json.encode({
    controller_id = target.controller_id,
    subject_id = target.subject_id,
    other_subject_id = assert(Louise.phone_control_subject(other)).subject_id,
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
          io.stdout:write("acknowledged\n")
          io.stdout:flush()
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
assert(api:dispose())
nvim.cmd("qa!")
