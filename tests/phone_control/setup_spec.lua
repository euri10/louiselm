local MiniTest = require("mini.test")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local Config = require("louiselm.config")
local Schema = require("louiselm.schema")
local PhoneControl = require("louiselm.phone_control")

local T = MiniTest.new_set()

T["phone control is disabled unless explicitly enabled"] = function()
  MiniTest.expect.equality(Config.enabled({}, "phone_control"), false)
  MiniTest.expect.equality(PhoneControl.start({}), nil)
  MiniTest.expect.equality(Schema.validate(Config.schema, { phone_control = { enabled = true } }), {})
  MiniTest.expect.equality(Config.enabled({ phone_control = { enabled = true } }, "phone_control"), true)
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

return T
