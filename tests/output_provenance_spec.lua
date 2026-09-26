local MiniTest = require("mini.test")
local Provenance = require("louiselm.output_provenance")

---@diagnostic disable-next-line: undefined-global -- Neovim runtime.
local nvim = vim
local T = MiniTest.new_set()

local function broker_record(code, digest, quarantined)
  return nvim.json.encode({
    record = { launch = { session_id = "broker-session" } },
    quarantined = quarantined,
    output_provenance = {
      schema = "louiselm.workspace.output-provenance/1",
      code = code,
      taint_digest = digest or nvim.NIL,
      clean_review_refs = {},
    },
  })
end

T["a broker-bound Session changes from clean to tainted without changing its binding"] = function()
  local binding = { kind = "broker", session_id = "broker-session" }
  local current = broker_record("untainted", nil, false)
  local observed = {}
  local fetch = function(id, callback)
    MiniTest.expect.equality(id, "broker-session")
    nvim.schedule(function()
      callback(current)
    end)
    return function() end
  end
  Provenance.read(binding, function(value)
    observed[#observed + 1] = value
  end, fetch)
  assert(nvim.wait(1000, function()
    return #observed == 1
  end))
  MiniTest.expect.equality(observed[1].code, "untainted")

  local digest = "sha256:" .. string.rep("a", 64)
  current = broker_record("session_output_tainted", digest, true)
  Provenance.read(binding, function(value)
    observed[#observed + 1] = value
  end, fetch)
  assert(nvim.wait(1000, function()
    return #observed == 2
  end))
  MiniTest.expect.equality(observed[2].code, "session_output_tainted")
  MiniTest.expect.equality(observed[2].taint_digest, digest)
end

T["unmanaged is explicit; missing or malformed broker metadata stays unknown"] = function()
  MiniTest.expect.equality(Provenance.initial({ kind = "not_managed" }).code, "not_managed")
  MiniTest.expect.equality(Provenance.initial(nil).code, "unknown")
  local bad = nvim.json.decode(broker_record("untainted", nil, true))
  MiniTest.expect.equality(Provenance.from_broker(bad, "broker-session").code, "unknown")
  bad.quarantined = false
  bad.record.launch.session_id = "another-session"
  MiniTest.expect.equality(Provenance.from_broker(bad, "broker-session").code, "unknown")
  bad.record.launch.session_id = "broker-session"
  bad.output_provenance.clean_review_refs = { "forged" }
  MiniTest.expect.equality(Provenance.from_broker(bad, "broker-session").code, "unknown")
end

T["detached or malformed Markdown metadata never reads as clean"] = function()
  local body = "# Session transcript\n\n## User\n\nunchanged words\n"
  local marked = Provenance.markdown(body, Provenance.initial({ kind = "not_managed" }))
  MiniTest.expect.equality(marked:sub(-#body), body)
  MiniTest.expect.equality(Provenance.inspect_markdown(marked).code, "not_managed")
  local clean = Provenance.markdown(
    body,
    Provenance.from_broker(nvim.json.decode(broker_record("untainted", nil, false)), "broker-session")
  )
  MiniTest.expect.equality(Provenance.inspect_markdown(clean).code, "unknown")
  MiniTest.expect.equality(Provenance.inspect_markdown(body).code, "unknown")
  MiniTest.expect.equality(Provenance.inspect_markdown("<!-- louiselm-provenance: {} -->\n" .. body).code, "unknown")
end

T["cancelled broker reads complete once with unknown provenance"] = function()
  local callbacks = {}
  local cancelled = false
  local values = {}
  local cancel = Provenance.read({ kind = "broker", session_id = "broker-session" }, function(value)
    values[#values + 1] = value
  end, function(_, callback)
    callbacks[#callbacks + 1] = callback
    return function()
      cancelled = true
    end
  end)
  cancel()
  callbacks[1](broker_record("untainted", nil, false))
  assert(nvim.wait(1000, function()
    return #values == 1
  end))
  MiniTest.expect.equality(cancelled, true)
  MiniTest.expect.equality(values[1].code, "unknown")
end

return T
