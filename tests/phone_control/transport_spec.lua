local MiniTest = require("mini.test")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local Controller = require("louiselm.phone_control.controller")
local Transport = require("louiselm.phone_control.transport")

local T = MiniTest.new_set()

T["registers the original receiver connection after opening its process identity"] = function()
  local directory = nvim.fn.tempname()
  assert(nvim.fn.mkdir(directory, "p") == 1)
  assert(nvim.fn.writefile({ "00112233-4455-6677-8899-aabbccddeeff" }, directory .. "/capability", "b") == 0)
  local listener = assert(nvim.uv.new_pipe(false))
  assert(listener:bind(directory .. "/socket") == 0)
  local peer, received
  local id, generation = string.rep("a", 32), string.rep("b", 32)
  assert(listener:listen(1, function()
    peer = assert(nvim.uv.new_pipe(false))
    assert(listener:accept(peer) == 0)
    local buffer = ""
    peer:read_start(function(err, data)
      assert(not err)
      if data == nil then
        return
      end
      buffer = buffer .. data
      local ending = buffer:find("\n", 1, true)
      if not ending then
        return
      end
      local message = nvim.json.decode(buffer:sub(1, ending - 1))
      buffer = buffer:sub(ending + 1)
      if message.type == "attach_write" then
        peer:write(nvim.json.encode({
          v = 1,
          type = "registration_offer",
          controller_id = id,
          generation = generation,
          receiver_pid = nvim.uv.os_getpid(),
          receiver_namespace = nvim.uv.fs_readlink("/proc/self/ns/pid"),
        }) .. "\n")
      elseif message.type == "register" then
        received = message
        peer:write(nvim.json.encode({
          v = 1,
          type = "registered",
          controller_id = id,
          generation = generation,
          nonce = message.nonce,
        }) .. "\n")
      end
    end)
  end) == 0)
  local controller = Controller.new({ service = { "unused" } })
  local transport = assert(Transport.connect({
    controller = controller,
    socket_path = directory .. "/socket",
    capability_path = directory .. "/capability",
    retry_ms = 1000,
  }))
  MiniTest.finally(function()
    transport:dispose()
    controller:dispose()
    if peer and not peer:is_closing() then
      peer:close()
    end
    if not listener:is_closing() then
      listener:close()
    end
    nvim.fn.delete(directory, "rf")
  end)
  assert(nvim.wait(3000, function()
    return controller.registration ~= nil
  end))
  MiniTest.expect.equality(controller.controller_id, id)
  MiniTest.expect.equality(controller.registration.generation, generation)
  MiniTest.expect.equality(received.controller_id, id)
  MiniTest.expect.equality(#received.nonce, 32)
  peer:close()
  assert(nvim.wait(3000, function()
    return controller.controller_id == nil
  end))
  MiniTest.expect.equality(controller.registration, nil)
end

T["an older read-only receiver remains usable"] = function()
  local directory = nvim.fn.tempname()
  assert(nvim.fn.mkdir(directory, "p") == 1)
  assert(nvim.fn.writefile({ "00112233-4455-6677-8899-aabbccddeeff" }, directory .. "/capability", "b") == 0)
  local listener, peers, requests = assert(nvim.uv.new_pipe(false)), {}, {}
  assert(listener:bind(directory .. "/socket") == 0)
  assert(listener:listen(4, function()
    local peer = assert(nvim.uv.new_pipe(false))
    assert(listener:accept(peer) == 0)
    peers[#peers + 1] = peer
    peer:read_start(function(err, data)
      assert(not err)
      if not data then
        return
      end
      local request = nvim.json.decode(data:match("^(.-)\n"))
      requests[#requests + 1] = request.type
      if request.type == "attach_write" then
        peer:write('{"v":1,"type":"error","code":"unsupported"}\n')
      else
        peer:write(nvim.json.encode({ v = 1, type = "attached", controller_id = string.rep("1", 32) }) .. "\n")
      end
    end)
  end) == 0)
  local controller = Controller.new({ service = { "unused" } })
  local transport = assert(Transport.connect({
    controller = controller,
    socket_path = directory .. "/socket",
    capability_path = directory .. "/capability",
    retry_ms = 20,
  }))
  MiniTest.finally(function()
    transport:dispose()
    controller:dispose()
    for _, peer in ipairs(peers) do
      if not peer:is_closing() then
        peer:close()
      end
    end
    if not listener:is_closing() then
      listener:close()
    end
    nvim.fn.delete(directory, "rf")
  end)
  assert(nvim.wait(3000, function()
    return controller.controller_id ~= nil
  end))
  MiniTest.expect.equality(requests, { "attach_write", "attach" })
  MiniTest.expect.equality(controller.registration, nil)
end

T["attaches over a private local socket and reattaches with fresh subjects"] = function()
  local directory = nvim.fn.tempname()
  assert(nvim.fn.mkdir(directory, "p") == 1)
  local socket = directory .. "/control.sock"
  local capability_path = directory .. "/capability"
  local capability = "00112233-4455-6677-8899-aabbccddeeff"
  assert(nvim.fn.writefile({ capability }, capability_path, "b") == 0)
  local listener = assert(nvim.uv.new_pipe(false))
  assert(listener:bind(socket) == 0)
  local peers, attaches, responses = {}, {}, {}
  assert(listener:listen(4, function(err)
    assert(not err)
    local peer = assert(nvim.uv.new_pipe(false))
    assert(listener:accept(peer) == 0)
    peers[#peers + 1] = peer
    local index, buffer = #peers, ""
    peer:read_start(function(read_err, data)
      assert(not read_err)
      if data == nil then
        return
      end
      buffer = buffer .. data
      while buffer:find("\n", 1, true) do
        local ending = buffer:find("\n", 1, true)
        local message = nvim.json.decode(buffer:sub(1, ending - 1))
        buffer = buffer:sub(ending + 1)
        if message.type == "attach" or message.type == "attach_write" then
          attaches[#attaches + 1] = message
          peer:write(
            nvim.json.encode({ v = 1, type = "attached", controller_id = string.rep(tostring(index), 32) }) .. "\n"
          )
        else
          responses[#responses + 1] = message
        end
      end
    end)
  end) == 0)
  local controller =
    Controller.new({ service = { nvim.fn.getcwd() .. "/tests/fixtures/phone_control_verify_stub.sh" } })
  local state = { id = "session-1", agent = "mock", working_dir = nvim.uv.cwd(), status = "ready" }
  local owner = { sessions = {} }
  local session = {
    state = state,
    owner = owner,
    inspect = function(self)
      return nvim.deepcopy(self.state)
    end,
  }
  require("tests.fixtures.phone_control_history")(session, controller)
  owner.sessions[state.id] = session
  controller:track(session)
  local transport = assert(Transport.connect({
    controller = controller,
    socket_path = socket,
    capability_path = capability_path,
    retry_ms = 20,
  }))
  MiniTest.finally(function()
    transport:dispose()
    controller:dispose()
    for _, peer in ipairs(peers) do
      if not peer:is_closing() then
        peer:close()
      end
    end
    if not listener:is_closing() then
      listener:close()
    end
    nvim.fn.delete(directory, "rf")
  end)
  assert(nvim.wait(5000, function()
    return controller.controller_id ~= nil
  end))
  MiniTest.expect.equality(attaches[1], { v = 1, type = "attach_write", capability = capability })
  local first_subject = assert(controller:track(session))
  local request = {
    v = 1,
    type = "read_session",
    controller_id = controller.controller_id,
    grant = { projects = { assert(nvim.uv.fs_realpath(state.working_dir)) }, agents = { "mock" }, groups = { "read" } },
    period = {},
    proof = {},
    request = {
      controller_id = controller.controller_id,
      subject_id = first_subject,
      operation_id = string.rep("a", 32),
      action = 1,
      payload = "00000000000000000014",
      expected_revision = 0,
    },
  }
  peers[1]:write(nvim.json.encode(request) .. "\n")
  assert(nvim.wait(5000, function()
    return #responses == 1
  end))
  MiniTest.expect.equality(responses[1].type, "session_page")
  peers[1]:write(string.rep("x", 262145))
  assert(nvim.wait(5000, function()
    return controller.controller_id == string.rep("2", 32)
  end))
  local next_subject = assert(controller:track(session))
  MiniTest.expect.no_equality(next_subject, first_subject)
  MiniTest.expect.equality(#attaches, 2)
  peers[2]:write(nvim.json.encode(request) .. "\n")
  assert(nvim.wait(5000, function()
    return #responses == 2
  end))
  MiniTest.expect.equality(responses[2].code, "stale_subject")
end

T["rejects whitespace inside an attached identity and retries"] = function()
  local directory = nvim.fn.tempname()
  assert(nvim.fn.mkdir(directory, "p") == 1)
  assert(nvim.fn.writefile({ "00112233-4455-6677-8899-aabbccddeeff" }, directory .. "/capability", "b") == 0)
  local listener = assert(nvim.uv.new_pipe(false))
  assert(listener:bind(directory .. "/socket") == 0)
  local peers = {}
  assert(listener:listen(4, function()
    local peer = assert(nvim.uv.new_pipe(false))
    assert(listener:accept(peer) == 0)
    peers[#peers + 1] = peer
    local index = #peers
    peer:read_start(function(_, data)
      if data == nil then
        return
      end
      local kind = index == 1 and "atta ched" or "attached"
      peer:write('{"v":1,"type":"' .. kind .. '","controller_id":"' .. string.rep(tostring(index), 32) .. '"}\n')
    end)
  end) == 0)
  local controller = Controller.new({ service = { "unused" } })
  local transport = assert(Transport.connect({
    controller = controller,
    socket_path = directory .. "/socket",
    capability_path = directory .. "/capability",
    retry_ms = 20,
  }))
  MiniTest.finally(function()
    transport:dispose()
    controller:dispose()
    for _, peer in ipairs(peers) do
      if not peer:is_closing() then
        peer:close()
      end
    end
    if not listener:is_closing() then
      listener:close()
    end
    nvim.fn.delete(directory, "rf")
  end)
  assert(nvim.wait(1000, function()
    return controller.controller_id == string.rep("2", 32)
  end))
  MiniTest.expect.equality(#peers, 2)
end

return T
