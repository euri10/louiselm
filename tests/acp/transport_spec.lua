local MiniTest = require("mini.test")
local Protocol = require("louiselm.acp.protocol")
local Transport = require("louiselm.acp.transport")

local T = MiniTest.new_set()

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

---@param system function
local function set_system(system)
  rawset(nvim, "system", system)
end

T["start"] = MiniTest.new_set()

T["start"]["frames messages across stdout chunks and writes JSON lines"] = function()
  local calls = {}
  local fake_handle = {
    is_closing = function()
      return false
    end,
    write = function(_, data)
      calls.written = data
    end,
  }
  local original_system = nvim.system
  set_system(function(command, options, on_exit)
    calls.command = command
    calls.options = options
    calls.on_exit = on_exit
    return fake_handle
  end)

  local messages = {}
  local transport, err = Transport.start({ command = "agent", args = { "--acp" } }, {
    on_message = function(message)
      messages[#messages + 1] = message
    end,
  })

  set_system(original_system)

  MiniTest.expect.equality(err, nil)
  assert(transport ~= nil)
  MiniTest.expect.equality(calls.command, { "agent", "--acp" })
  MiniTest.expect.equality(calls.options.stdin, true)
  MiniTest.expect.equality(calls.options.text, true)

  calls.options.stdout(nil, '{"jsonrpc":"2.0","method":"session/update",')
  calls.options.stdout(nil, '"params":{"sessionId":"s"}}\n')
  MiniTest.expect.equality(messages, {
    { jsonrpc = "2.0", method = "session/update", params = { sessionId = "s" } },
  })

  local sent, send_error = transport:send({ jsonrpc = "2.0", method = "session/cancel", params = {} })
  MiniTest.expect.equality(sent, true)
  MiniTest.expect.equality(send_error, nil)
  MiniTest.expect.equality(assert(Protocol.decode(calls.written:sub(1, -2))), {
    jsonrpc = "2.0",
    method = "session/cancel",
    params = {},
  })
end

T["start"]["merges Session environment over Agent configuration"] = function()
  local captured
  local original_system = nvim.system
  set_system(function(_, options)
    captured = options.env
    return {
      is_closing = function()
        return false
      end,
    }
  end)
  assert(Transport.start({ command = "agent", args = {}, env = { SHARED = "agent", AGENT = "yes" } }, {
    env = { SHARED = "run", RUN = "yes" },
  }))
  set_system(original_system)
  MiniTest.expect.equality(captured, { SHARED = "run", AGENT = "yes", RUN = "yes" })
end

T["start"]["surfaces malformed stdout and launch errors"] = function()
  local errors = {}
  local original_system = nvim.system
  set_system(function(_, options)
    options.stdout(nil, "not json\n")
    return {
      is_closing = function()
        return false
      end,
    }
  end)

  local transport, err = Transport.start({ command = "agent", args = {} }, {
    on_error = function(message)
      errors[#errors + 1] = message
    end,
  })
  set_system(original_system)

  MiniTest.expect.equality(err, nil)
  assert(transport ~= nil)
  MiniTest.expect.equality(#errors, 1)
  MiniTest.expect.equality(errors[1], "invalid JSON")

  original_system = nvim.system
  set_system(function()
    error("cannot spawn")
  end)
  local failed, launch_error = Transport.start({ command = "agent", args = {} })
  set_system(original_system)

  MiniTest.expect.equality(failed, nil)
  assert(launch_error ~= nil)
  MiniTest.expect.equality(launch_error:find("cannot spawn", 1, true) ~= nil, true)
end

return T
