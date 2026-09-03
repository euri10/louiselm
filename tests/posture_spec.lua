local MiniTest = require("mini.test")
local Posture = require("louiselm.posture")

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim
local T = MiniTest.new_set()

local function fixture()
  local lines = nvim.fn.readfile("tests/fixtures/verified_posture_v1.json")
  return table.concat(lines, "\n")
end

T["decodes the shared Rust posture fixture for human presentation"] = function()
  local posture, decode_error = Posture.decode(fixture())
  MiniTest.expect.equality(decode_error, nil)
  assert(posture ~= nil)
  MiniTest.expect.equality(posture.state, "unverified")

  MiniTest.expect.equality(Posture.health_items(posture), {
    { level = "error", message = "Verified posture: unverified" },
    { level = "ok", message = "managed_supply: verified" },
    {
      level = "error",
      message = "native_supply: failed (native_supply_uncertain); next: mask_or_measure_native_supply",
    },
    { level = "ok", message = "runtime: verified" },
    { level = "ok", message = "isolation: verified" },
    {
      level = "warn",
      message = "network: waived (broker_unavailable); next: restore_control_broker",
    },
    { level = "ok", message = "provider_disclosure: verified" },
    {
      level = "info",
      message = "Plaintext intentionally sent to a cloud Provider is visible to that Provider despite local containment.",
    },
  })
end

T["rejects malformed or contradictory controller output"] = function()
  local function changed(pattern, replacement)
    local payload = fixture():gsub(pattern, replacement, 1)
    return payload
  end
  local cases = {
    changed("louiselm%.verified%-posture/1", "louiselm.verified-posture/99"),
    changed('"state": "unverified"', '"state": "fully_verified"'),
    changed('"managed_supply"', '"unknown_supply"'),
    changed('"kind": "skill_generation"', '"kind": "/home/operator/secret"'),
    changed("^%{", '{"unexpected":true,'),
    changed('"state": "verified",', '"state": "verified", "unexpected": true,'),
    changed('"detail": "No action is required%."', '"detail": "agent supplied text"'),
  }

  for _, payload in ipairs(cases) do
    local posture, decode_error = Posture.decode(payload)
    MiniTest.expect.equality(posture, nil)
    MiniTest.expect.equality(type(decode_error), "string")
  end
end

return T
