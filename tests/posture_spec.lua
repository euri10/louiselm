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
    {
      level = "info",
      message = "Instructions embedded in the measured executable are part of runtime trust, not admitted Skill supply.",
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
    changed('"embedded_instructions_notice": "[^"]+",', ""),
    changed("not admitted Skill supply", "verified managed skills"),
  }

  for _, payload in ipairs(cases) do
    local posture, decode_error = Posture.decode(payload)
    MiniTest.expect.equality(posture, nil)
    MiniTest.expect.equality(type(decode_error), "string")
  end
end

T["metadata disclosure matches shared Rust evidence and safe notice"] = function()
  local metadata =
    nvim.json.decode(table.concat(nvim.fn.readfile("tests/fixtures/provider_metadata_disclosure.json"), "\n"))
  local record = nvim.json.decode(fixture())
  record.provider_disclosure_notice = metadata.notice
  local evidence = record.dimensions.provider_disclosure.evidence
  evidence[#evidence + 1] = { kind = metadata.kind, id = metadata.id }
  local posture, err = Posture.decode(nvim.json.encode(record))
  MiniTest.expect.equality(err, nil)
  assert(posture)
  local items = assert(Posture.health_items(posture))
  MiniTest.expect.equality(items[#items - 1].message, metadata.notice)

  record.provider_disclosure_notice = metadata.notice .. " Anonymous."
  MiniTest.expect.equality(Posture.decode(nvim.json.encode(record)), nil)
  record.provider_disclosure_notice = metadata.notice
  evidence[#evidence].id = "sha256:" .. string.rep("a", 64)
  MiniTest.expect.equality(Posture.decode(nvim.json.encode(record)), nil)
  evidence[#evidence].id = metadata.id
  record.dimensions.provider_disclosure.evidence = { evidence[#evidence] }
  -- Metadata approval alone cannot establish full disclosure.
  MiniTest.expect.equality(Posture.decode(nvim.json.encode(record)), nil)
end

return T
