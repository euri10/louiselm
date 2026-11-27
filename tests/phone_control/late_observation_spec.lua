local MiniTest = require("mini.test")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local Session = require("louiselm.session")
local PhoneControl = require("louiselm.phone_control")

local T = MiniTest.new_set()

T["recovers history before observation and keeps new Sessions complete"] = function()
  local root = nvim.fn.getcwd()
  local directory = nvim.fn.tempname()
  local workflow = directory .. "/louiselm/workflow"
  assert(nvim.fn.mkdir(workflow, "p") == 1)
  local capability = "00112233-4455-6677-8899-aabbccddeeff"
  assert(nvim.fn.writefile({ capability }, workflow .. "/operator-capability", "b") == 0)
  local previous_state, previous_override = nvim.env.XDG_STATE_HOME, nvim.env.LOUISELM_CAPTURE_STATE_DIR
  nvim.env.XDG_STATE_HOME, nvim.env.LOUISELM_CAPTURE_STATE_DIR = directory, nil
  local listener = assert(nvim.uv.new_pipe(false))
  assert(listener:bind(workflow .. "/phone-control.sock") == 0)
  local peer
  local received = ""
  local controller_id = string.rep("1", 32)
  assert(listener:listen(1, function(err)
    assert(not err)
    peer = assert(nvim.uv.new_pipe(false))
    assert(listener:accept(peer) == 0)
    peer:read_start(function(read_error, data)
      assert(not read_error)
      if data ~= nil then
        received = received .. data
        local ending = received:find("\n", 1, true)
        if ending then
          local attach = nvim.json.decode(received:sub(1, ending - 1))
          assert(attach.type == "attach_write" and attach.capability == capability)
          peer:write(nvim.json.encode({ v = 1, type = "attached", controller_id = controller_id }) .. "\n")
        end
      end
    end)
  end) == 0)
  local definition = {
    provider = "test-service",
    command = nvim.v.progpath,
    env = { LOUISELM_MOCK_MODE = "static" },
    args = {
      "--headless",
      "--noplugin",
      "-u",
      root .. "/tests/mock/init.lua",
      "-c",
      "lua require('louiselm.dev.mock_agent').run()",
    },
  }
  local api = assert(Session.new({ mock = definition }, nil, { usage_directory = directory .. "/usage" }))
  local owner
  MiniTest.finally(function()
    if owner then
      assert(owner:dispose())
    end
    assert(api:dispose())
    local flushed = false
    api:flush_recording(function()
      flushed = true
    end)
    assert(nvim.wait(6000, function()
      return flushed
    end))
    if peer and not peer:is_closing() then
      peer:close()
    end
    if not listener:is_closing() then
      listener:close()
    end
    nvim.env.XDG_STATE_HOME, nvim.env.LOUISELM_CAPTURE_STATE_DIR = previous_state, previous_override
    nvim.fn.delete(directory, "rf")
  end)

  local existing = assert(api:create_session("mock", { cwd = root }))
  assert(nvim.wait(6000, function()
    return existing:inspect().status == "ready"
  end))
  local done = false
  assert(existing:prompt("before phone control", function(_, err)
    assert(err == nil, err)
    done = true
  end))
  assert(nvim.wait(6000, function()
    return done
  end))

  owner = assert(PhoneControl.start({
    phone_control = { enabled = true },
    capture = { service = { root .. "/tests/fixtures/phone_control_verify_stub.sh" } },
  }))
  assert(nvim.wait(6000, function()
    return owner:subject(existing) ~= nil
  end))
  local controller = owner.controller

  local function page(session)
    local subject = assert(controller.sessions[session])
    local request = {
      v = 1,
      type = "read_session",
      controller_id = controller_id,
      grant = { projects = { assert(nvim.uv.fs_realpath(root)) }, agents = { "mock" }, groups = { "read" } },
      proof = {},
      period = {},
      request = {
        controller_id = controller_id,
        subject_id = subject.id,
        operation_id = string.rep("2", 32),
        action = 1,
        payload = "00000000000000000014",
        expected_revision = 0,
      },
    }
    local result
    controller:read(nvim.json.encode(request), function(value)
      result = value
    end)
    assert(nvim.wait(6000, function()
      return result ~= nil
    end))
    assert(result.type == "session_page")
    return result
  end

  local existing_page = page(existing)
  MiniTest.expect.equality(existing_page.complete, true)
  MiniTest.expect.equality(existing_page.entries[1].text, "before phone control")
  local fresh = assert(api:create_session("mock", { cwd = root }))
  assert(nvim.wait(6000, function()
    return fresh:inspect().status == "ready"
  end))
  MiniTest.expect.equality(page(fresh).complete, true)
end

return T
