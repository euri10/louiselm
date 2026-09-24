local MiniTest = require("mini.test")
local Client = require("louiselm.workflow.attention_client")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local T = MiniTest.new_set()

T["renders durable conditions read-only and ignores queued frames after close"] = function()
  local original = Client.connect
  local pipe = { closing = false, writes = {} }
  function pipe:connect(_, callback)
    callback()
  end
  function pipe:read_start(callback)
    self.read = callback
  end
  function pipe:read_stop() end
  function pipe:is_closing()
    return self.closing
  end
  function pipe:close()
    self.closing = true
  end
  function pipe:write(frame)
    self.writes[#self.writes + 1] = frame
  end
  ---@diagnostic disable-next-line: duplicate-set-field
  Client.connect = function(path, callback, options)
    MiniTest.expect.equality(options.operator_capability, nil)
    options.pipe_factory = function()
      return pipe
    end
    return original(path, callback, options)
  end
  MiniTest.finally(function()
    Client.connect = original
  end)
  local view = assert(require("louiselm.ui.attention_view").open("/tmp/attention.sock"))
  MiniTest.finally(function()
    view:dispose()
  end)
  local item = {
    subject_kind = "session",
    subject_id = "broker-session",
    kind = "skill_unverified",
    code = "isolation_failed",
    source_operation_id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
    linked_run_id = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
    reason = "untrusted text must never render",
  }
  local function send(items)
    pipe.read(nil, nvim.json.encode({ type = "snapshot", snapshot = { generation = 1, items = items } }) .. "\n")
  end
  local async = assert(nvim.uv.new_async(function()
    assert(nvim.in_fast_event())
    send({ item })
  end))
  MiniTest.finally(function()
    async:close()
  end)
  async:send()
  assert(nvim.wait(1000, function()
    return table.concat(nvim.api.nvim_buf_get_lines(view.buffer, 0, -1, false), "\n"):find("isolation_failed", 1, true)
      ~= nil
  end))
  local lines = table.concat(nvim.api.nvim_buf_get_lines(view.buffer, 0, -1, false), "\n")
  assert(lines:find(item.subject_id, 1, true))
  assert(lines:find(item.linked_run_id, 1, true))
  assert(not lines:find(item.reason, 1, true))
  MiniTest.expect.equality(nvim.bo[view.buffer].modifiable, false)
  MiniTest.expect.equality(pipe.writes, {})
  send({})
  assert(nvim.wait(1000, function()
    return table.concat(nvim.api.nvim_buf_get_lines(view.buffer, 0, -1, false), "\n"):find("No unresolved", 1, true)
      ~= nil
  end))
  item.code = "candidate_text_must_not_render"
  send({ item })
  assert(nvim.wait(1000, function()
    return nvim.api.nvim_buf_get_lines(view.buffer, 0, -1, false)[1] == "Attention unavailable: invalid durable item"
  end))
  send({ item })
  view:dispose()
  nvim.wait(20)
  MiniTest.expect.equality(nvim.api.nvim_buf_is_valid(view.buffer), false)
  MiniTest.expect.equality(pipe.closing, true)
end

T["command requires opt-in and repeated registration disposes its view"] = function()
  local Command = require("louiselm.ui.chat.command")
  local View = require("louiselm.ui.attention_view")
  local original = View.open
  local opened, disposed = 0, 0
  ---@diagnostic disable-next-line: duplicate-set-field
  View.open = function()
    opened = opened + 1
    return {
      buffer = 0,
      disposed = false,
      dispose = function()
        disposed = disposed + 1
        return true
      end,
    }
  end
  MiniTest.finally(function()
    Command.configure(nil)
    Command.register()
    View.open = original
  end)
  Command.configure(nil)
  Command.register()
  local ok, err = pcall(nvim.cmd, "LouiselmAttention")
  MiniTest.expect.equality(ok, false)
  assert(tostring(err):find("Attention is disabled", 1, true))
  MiniTest.expect.equality(opened, 0)
  Command.configure({ attention = { enabled = true } })
  nvim.cmd("LouiselmAttention")
  MiniTest.expect.equality(opened, 1)
  Command.register()
  MiniTest.expect.equality(disposed, 1)
end

return T
