local MiniTest = require("mini.test")
---@diagnostic disable-next-line: undefined-global -- Neovim runtime.
local nvim = vim
local T = MiniTest.new_set()

for _, interface in ipairs({ "run", "attention" }) do
  T[interface .. " refuses missing, malformed or unsupported interfaces before mutations"] = function()
    for _, metadata in ipairs({
      false,
      {},
      { component = "capture", version = "0.9.2", interfaces = { [interface] = 2 } },
    }) do
      local pipe = { closing = false, writes = {} }
      function pipe:connect(_, callback)
        callback()
      end
      function pipe:read_start(callback)
        self.read = callback
      end
      function pipe:read_stop() end
      function pipe:close()
        self.closing = true
      end
      function pipe:is_closing()
        return self.closing
      end
      function pipe:write(data)
        self.writes[#self.writes + 1] = data
      end
      local errors = {}
      local client = assert(require("louiselm.workflow." .. interface .. "_client").connect("fixture", function()
        error("incompatible snapshot must not reach consumer")
      end, {
        pipe_factory = function()
          return pipe
        end,
        operator_capability = "fixture",
        on_error = function(err)
          errors[#errors + 1] = err
        end,
      }))
      MiniTest.finally(function()
        client:dispose()
      end)
      local function mutate()
        if interface == "run" then
          return client:raise("run", 1, 2, function() end)
        end
        return client:clear_session("session", function() end)
      end
      MiniTest.expect.equality(mutate(), false)
      pipe.read(
        nil,
        nvim.json.encode({ type = "snapshot", service = metadata, runs = {}, snapshot = { generation = 0, items = {} } })
          .. "\n"
      )
      assert(nvim.wait(1000, function()
        return #errors > 0
      end))
      MiniTest.expect.equality(errors[1]:find("install", 1, true) ~= nil, true)
      MiniTest.expect.equality(pipe.closing, true)
      MiniTest.expect.equality(mutate(), false)
      MiniTest.expect.equality(pipe.writes, {})
    end
  end
end

return T
