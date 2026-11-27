local MiniTest = require("mini.test")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local Config = require("louiselm.config")
local Schema = require("louiselm.schema")
local PhoneControl = require("louiselm.phone_control")

local previous_state
local T = MiniTest.new_set({
  hooks = {
    pre_case = function()
      previous_state = nvim.env.LOUISELM_CAPTURE_STATE_DIR
      nvim.env.LOUISELM_CAPTURE_STATE_DIR = nvim.fn.tempname()
    end,
    post_case = function()
      nvim.env.LOUISELM_CAPTURE_STATE_DIR = previous_state
    end,
  },
})

T["ordinary setup observes Sessions without granting phone authority"] = function()
  MiniTest.expect.equality(Config.enabled({}, "phone_control"), true)
  local owner = assert(PhoneControl.start({}))
  MiniTest.finally(function()
    owner:dispose()
  end)
  MiniTest.expect.equality(owner.controller.controller_id, nil)
  MiniTest.expect.equality(PhoneControl.start(nil), nil)
  MiniTest.expect.equality(Schema.validate(Config.schema, { phone_control = { enabled = true } }), {})
  MiniTest.expect.equality(Config.enabled({ phone_control = { enabled = true } }, "phone_control"), true)
end

T["capture-enabled stock setup connects unless phone control is explicitly disabled"] = function()
  local owner = assert(PhoneControl.start({ capture = { enabled = true } }))
  MiniTest.finally(function()
    owner:dispose()
  end)
  MiniTest.expect.equality(
    PhoneControl.start({ capture = { enabled = true }, phone_control = { enabled = false } }),
    nil
  )
end

T["repeated owners dispose safely while the service is unavailable"] = function()
  for _ = 1, 3 do
    local owner = assert(PhoneControl.start({ phone_control = { enabled = true } }))
    owner:dispose()
    owner:dispose()
  end
end

T["the channel follows the capture state directory override"] = function()
  local previous = nvim.env.LOUISELM_CAPTURE_STATE_DIR
  local directory = nvim.fn.tempname()
  nvim.env.LOUISELM_CAPTURE_STATE_DIR = directory
  local owner = assert(PhoneControl.start({ phone_control = { enabled = true } }))
  MiniTest.finally(function()
    owner:dispose()
    nvim.env.LOUISELM_CAPTURE_STATE_DIR = previous
  end)
  MiniTest.expect.equality(owner.transport.socket_path, directory .. "/louiselm/workflow/phone-control.sock")
end

T["Run registration belongs to the explicit phone owner"] = function()
  local owner = assert(PhoneControl.start({ phone_control = { enabled = true } }))
  MiniTest.finally(function()
    owner:dispose()
  end)
  local id = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"
  local executor = {
    run = { id = id, workers = {} },
    options = {
      envelope = { run_id = id },
      agent_id = "codex",
      bead_ids = { "b-1" },
      prepared = { registration_id = string.rep("1", 32), revision = 1, selection_sha256 = string.rep("2", 64) },
    },
    status = "new",
  }
  assert(owner:track_run(executor))
  MiniTest.expect.equality(owner:track_run(executor), false)
  owner:untrack_run(executor)
  assert(owner:track_run(executor))
  assert(owner:dispose())
  MiniTest.expect.equality(owner:track_run(executor), false)
end

return T
