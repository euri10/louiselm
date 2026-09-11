local MiniTest = require("mini.test")
local Preflight = require("louiselm.preflight")

---@diagnostic disable-next-line: undefined-global -- Neovim runtime.
local nvim = vim
local T = MiniTest.new_set()

local function fixture()
  return table.concat(nvim.fn.readfile("tests/fixtures/preflight_v1.json"), "\n")
end

T["consumes the producing Rust fixture and presents proposed inputs separately"] = function()
  local preview, err = Preflight.decode(fixture())
  MiniTest.expect.equality(err, nil)
  assert(preview ~= nil)
  local items = assert(Preflight.health_items(preview))
  local messages = {}
  for _, item in ipairs(items) do
    messages[#messages + 1] = item.message
  end
  local text = table.concat(messages, "\n")
  MiniTest.expect.equality(text:find("not authorization or live Session state", 1, true) ~= nil, true)
  MiniTest.expect.equality(text:find("proposed envelope_revision: 2", 1, true) ~= nil, true)
  MiniTest.expect.equality(text:find("envelope_revision: 1 -> 2", 1, true) ~= nil, true)
  MiniTest.expect.equality(text:find("network_scope: comparison unresolved", 1, true) ~= nil, true)
end

T["rejects unsafe identities and contradictory presentation claims"] = function()
  local mutations = {
    function(value)
      value.notice = "authorized to launch"
    end,
    function(value)
      value.schema = "louiselm.launch.preflight/99"
    end,
    function(value)
      value.proposed[1].value = "secret\npath"
    end,
    function(value)
      value.proposed[2].field = "agent"
    end,
    function(value)
      value.proposed[14].value = "denied"
    end,
    function(value)
      value.comparison.changes[1].after = "sha256:" .. string.rep("0", 64)
    end,
    function(value)
      value.comparison.state = "not_requested"
    end,
    function(value)
      value.posture.state = "fully_verified"
    end,
  }
  for _, mutate in ipairs(mutations) do
    local value = nvim.json.decode(fixture())
    mutate(value)
    local preview, err = Preflight.decode(nvim.json.encode(value))
    MiniTest.expect.equality(preview, nil)
    MiniTest.expect.equality(type(err), "string")
  end
  MiniTest.expect.equality(Preflight.decode(string.rep(" ", 65537)), nil)
end

local function with_process(callback)
  local original = nvim.system
  local processes = {}
  rawset(nvim, "system", function(command, options, done)
    local process = { command = command, options = options, done = done, killed = 0 }
    processes[#processes + 1] = process
    return {
      kill = function()
        process.killed = process.killed + 1
      end,
    }
  end)
  local ok, err = pcall(callback, processes)
  nvim.system = original
  if not ok then
    error(err)
  end
end

local function complete(process, payload, code)
  local timer = assert(nvim.uv.new_timer())
  timer:start(0, 0, function()
    assert(nvim.in_fast_event())
    process.options.stdout(nil, payload)
    process.done({ code = code or 2, signal = 0 })
    timer:stop()
    timer:close()
  end)
end

T["async reads schedule UI callbacks once and use argument arrays"] = function()
  with_process(function(processes)
    local calls = 0
    local handle =
      assert(Preflight.read({ request = "/request with spaces", manifest = "/manifest" }, function(preview, err)
        assert(not nvim.in_fast_event())
        assert(preview ~= nil, err)
        calls = calls + 1
      end))
    MiniTest.expect.equality(
      processes[1].command,
      { "louiselm-skills", "preflight", "--robot-json", "--request", "/request with spaces", "--manifest", "/manifest" }
    )
    complete(processes[1], fixture())
    assert(nvim.wait(1000, function()
      return calls == 1
    end))
    processes[1].done({ code = 2, signal = 0 })
    nvim.wait(20, function()
      return false
    end)
    MiniTest.expect.equality(calls, 1)
    handle.dispose()
  end)
end

T["disposal suppresses completion already queued for the main loop"] = function()
  with_process(function(processes)
    local calls = 0
    local handle = assert(Preflight.read({ request = "/request" }, function()
      calls = calls + 1
    end))
    processes[1].options.stdout(nil, fixture())
    processes[1].done({ code = 2, signal = 0 })
    handle.dispose()
    nvim.wait(20, function()
      return false
    end)
    MiniTest.expect.equality(calls, 0)
    local pending = assert(Preflight.read({ request = "/request" }, function()
      calls = calls + 1
    end))
    pending.dispose()
    complete(processes[2], fixture())
    nvim.wait(20, function()
      return false
    end)
    MiniTest.expect.equality(calls, 0)
    MiniTest.expect.equality(processes[2].killed, 1)
  end)
end

T["bounded process failures are fixed diagnostics without stderr payloads"] = function()
  with_process(function(processes)
    local calls = {}
    assert(Preflight.read({ request = "/request" }, function(preview, err)
      MiniTest.expect.equality(preview, nil)
      calls[#calls + 1] = err
    end))
    complete(processes[1], string.rep("private", 10000))
    assert(nvim.wait(1000, function()
      return #calls == 1
    end))
    MiniTest.expect.equality(calls[1], "preflight output exceeds size limit")
  end)
end

return T
