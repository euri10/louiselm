local MiniTest = require("mini.test")
local Transport = require("louiselm.acp.transport")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local T = MiniTest.new_set()

local function request()
  return {
    schema = "louiselm.launch.request/3",
    protocol_version = 1,
    request_id = "launch-1",
    authorization_id = "authorization-1",
    session_id = "worker-1",
    run_id = "run-1",
    agent_id = "codex",
    envelope_id = "envelope-1",
    envelope_revision = 1,
    skills = {
      mode = "enabled",
      generation_digest = "sha256:" .. string.rep("a", 64),
      view_digest = "sha256:" .. string.rep("c", 64),
    },
    session_input_manifest_id = "sha256:" .. string.rep("b", 64),
  }
end

T["launch document matches the Rust canonical field order"] = function()
  local Launch = require("louiselm.acp.launch")
  local bytes = assert(Launch.encode(request()))
  MiniTest.expect.equality(
    bytes,
    '{"schema":"louiselm.launch.request/3","protocol_version":1,"request_id":"launch-1","authorization_id":"authorization-1","session_id":"worker-1","run_id":"run-1","agent_id":"codex","envelope_id":"envelope-1","envelope_revision":1,"skills":{"mode":"enabled","generation_digest":"sha256:'
      .. string.rep("a", 64)
      .. '","view_digest":"sha256:'
      .. string.rep("c", 64)
      .. '"},"session_input_manifest_id":"sha256:'
      .. string.rep("b", 64)
      .. '"}'
  )
  local malformed = request()
  malformed.command = "injected"
  local value, err = Launch.encode(malformed)
  MiniTest.expect.equality(value, nil)
  MiniTest.expect.equality(err, "unknown launch request field: command")
  for _, field in ipairs({ "session_id", "run_id", "envelope_revision", "skills" }) do
    malformed = request()
    malformed[field] = "../invalid"
    value, err = Launch.encode(malformed)
    MiniTest.expect.equality(value, nil)
    MiniTest.expect.equality(err, "invalid launch request field: " .. field)
  end
end

T["skills off is explicit and cannot carry Generation authority"] = function()
  local Launch = require("louiselm.acp.launch")
  local selected = request()
  selected.skills = { mode = "off", view_digest = "sha256:" .. string.rep("c", 64) }
  local bytes = assert(Launch.encode(selected))
  MiniTest.expect.equality(nvim.json.decode(bytes).skills.mode, "off")
  MiniTest.expect.equality(nvim.json.decode(bytes).skills.generation_digest, nil)
  for _, skills in ipairs({
    { view_digest = selected.skills.view_digest },
    { mode = "off", view_digest = selected.skills.view_digest, generation_digest = "sha256:" .. string.rep("a", 64) },
    { mode = "enabled", view_digest = selected.skills.view_digest },
    { mode = "unknown", view_digest = selected.skills.view_digest },
  }) do
    selected.skills = skills
    local value, err = Launch.encode(selected)
    MiniTest.expect.equality(value, nil)
    MiniTest.expect.equality(err, "invalid launch request field: skills")
  end
end

T["contained transport sends the launch first and leaves cleanup to its supervisor"] = function()
  local writes, kills = {}, {}
  local command, options
  local transport = assert(Transport.start({ provider = "openai", command = "must-not-run", args = {} }, {
    cwd = "/workspace",
    launch_request = request(),
  }, function(argv, opts)
    command, options = argv, opts
    return {
      write = function(_, data)
        writes[#writes + 1] = data or "EOF"
      end,
      kill = function(_, signal)
        kills[#kills + 1] = signal
      end,
      is_closing = function()
        return false
      end,
    }
  end))
  MiniTest.expect.equality(
    command,
    { "/usr/bin/sudo", "-n", "/usr/local/lib/louiselm/current/bin/louiselm-launch", "run" }
  )
  MiniTest.expect.equality(options.cwd, "/")
  MiniTest.expect.equality(options.clear_env, true)
  MiniTest.expect.equality(nvim.json.decode(writes[1]).session_id, "worker-1")
  assert(transport:send({ jsonrpc = "2.0", id = 1, method = "initialize" }))
  MiniTest.expect.equality(nvim.json.decode(writes[2]).method, "initialize")
  transport.options.launch_request = nil -- Caller mutation cannot change cleanup ownership.
  assert(transport:close())
  assert(transport:close())
  MiniTest.expect.equality(writes[3], "EOF")
  MiniTest.expect.equality(#writes, 3)
  MiniTest.expect.equality(kills, {})
end

T["invalid launch never starts a process; failed prefix closes its input"] = function()
  local starts, closes = 0, 0
  local function system()
    starts = starts + 1
    return {
      write = function(_, data)
        if data then
          error("broken pipe")
        end
        closes = closes + 1
      end,
      kill = function()
        error("must not kill supervisor")
      end,
    }
  end
  local malformed = request()
  malformed.extra = true
  local transport, err = Transport.start(
    { provider = "openai", command = "unused", args = {} },
    { launch_request = malformed },
    system
  )
  MiniTest.expect.equality(transport, nil)
  MiniTest.expect.equality(err, "unknown launch request field: extra")
  MiniTest.expect.equality(starts, 0)
  transport, err = Transport.start(
    { provider = "openai", command = "unused", args = {} },
    { launch_request = request() },
    system
  )
  MiniTest.expect.equality(transport, nil)
  MiniTest.expect.equality(err, "could not write launch request")
  MiniTest.expect.equality(closes, 1)
end

return T
