local MiniTest = require("mini.test")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local ProcessIdentity = require("louiselm.phone_control.process_identity")
local namespace = assert(nvim.uv.fs_readlink("/proc/self/ns/pid"))
local service = nvim.env.LOUISELM_TEST_CAPTURE and { nvim.env.LOUISELM_TEST_CAPTURE }
  or { "python3", nvim.fn.getcwd() .. "/tests/fixtures/phone_clock_verifier.py", "healthy" }

local T = MiniTest.new_set()

T["a missing process verifier leaves the retained identity unavailable"] = function()
  local identity = assert(ProcessIdentity.new(nvim.uv.os_getpid(), namespace, { "/missing/phone-helper" }))
  MiniTest.finally(function()
    identity:dispose()
  end)
  local opened, cause
  identity:open(function(ok, err)
    opened, cause = ok, err
  end)
  assert(nvim.wait(5000, function()
    return opened ~= nil
  end))
  MiniTest.expect.equality({ opened, cause, identity:alive() }, { false, "unavailable", false })
end

T["a malformed verifier response leaves the process unavailable"] = function()
  local identity = assert(ProcessIdentity.new(nvim.uv.os_getpid(), namespace, {
    "python3",
    nvim.fn.getcwd() .. "/tests/fixtures/phone_clock_verifier.py",
    "process_malformed",
  }))
  MiniTest.finally(function()
    identity:dispose()
  end)
  local opened, cause, fast
  identity:open(function(ok, err)
    opened, cause, fast = ok, err, nvim.in_fast_event()
  end)
  assert(nvim.wait(5000, function()
    return opened ~= nil
  end))
  MiniTest.expect.equality({ opened, cause, fast, identity:alive() }, { false, "unavailable", false, false })
end

T["disposing an in-flight verifier releases both descriptors and its child"] = function()
  local marker = nvim.fn.tempname()
  local identity = assert(ProcessIdentity.new(nvim.uv.os_getpid(), namespace, {
    "python3",
    nvim.fn.getcwd() .. "/tests/fixtures/phone_clock_verifier.py",
    "process_hold",
    marker,
  }))
  local called = false
  MiniTest.finally(function()
    identity:dispose()
    nvim.fn.delete(marker)
  end)
  identity:open(function()
    called = true
  end)
  assert(nvim.wait(5000, function()
    return nvim.uv.fs_stat(marker) ~= nil
  end))
  local pid = assert(tonumber(nvim.fn.readfile(marker)[1]))
  local fd, status_fd = identity.fd, identity.status_fd
  assert(identity:dispose())
  assert(nvim.wait(5000, function()
    return nvim.uv.kill(pid, 0) == nil
  end))
  MiniTest.expect.equality({ nvim.uv.fs_fstat(fd), nvim.uv.fs_fstat(status_fd), identity:alive(), called }, {
    nil,
    nil,
    false,
    false,
  })
end

T["the retained handle observes a normal child exit"] = function()
  local exited = false
  local child = nvim.system({ "cat" }, { stdin = true }, function()
    exited = true
  end)
  local identity = assert(ProcessIdentity.new(child.pid, namespace, service))
  MiniTest.finally(function()
    identity:dispose()
    if not exited then
      child:kill(9)
    end
  end)
  local opened
  identity:open(function(ok)
    opened = ok
  end)
  assert(nvim.wait(5000, function()
    return opened ~= nil
  end))
  MiniTest.expect.equality({ opened, identity:alive() }, { true, true })
  child:write(nil)
  assert(nvim.wait(5000, function()
    return exited
  end))
  MiniTest.expect.equality(identity:alive(), false)
end

T["an open process identity observes stop and death"] = function()
  local exited = false
  local child = nvim.system({ "sleep", "30" }, {}, function()
    exited = true
  end)
  local identity = assert(ProcessIdentity.new(child.pid, namespace, service))
  MiniTest.finally(function()
    identity:dispose()
    if not exited then
      child:kill(9)
    end
  end)

  local opened, open_error
  identity:open(function(ok, err)
    opened, open_error = ok, err
  end)
  assert(nvim.wait(5000, function()
    return opened ~= nil
  end))
  MiniTest.expect.equality({ opened, open_error }, { true, nil })
  MiniTest.expect.equality(identity:alive(), true)

  child:kill(19)
  assert(nvim.wait(5000, function()
    local fd = nvim.uv.fs_open("/proc/" .. child.pid .. "/stat", "r", 0)
    if fd == nil then
      return false
    end
    local stat = nvim.uv.fs_read(fd, 4096, 0)
    assert(nvim.uv.fs_close(fd))
    return type(stat) == "string" and stat:match("^%d+ %(.+%) T ") ~= nil
  end))
  MiniTest.expect.equality(identity:alive(), true)
  child:kill(9)
  assert(nvim.wait(5000, function()
    return exited
  end))
  MiniTest.expect.equality(identity:alive(), false)
  identity:dispose()
  MiniTest.expect.equality(identity:alive(), false)
end

T["invalid process identities fail closed"] = function()
  for _, pid in ipairs({ 0, -1, 1.5, 2147483648, "1" }) do
    local identity, err = ProcessIdentity.new(pid, namespace, service)
    MiniTest.expect.equality(identity, nil)
    MiniTest.expect.equality(err, "unavailable")
  end
  local identity, err = ProcessIdentity.new(nvim.uv.os_getpid(), "not-a-namespace", service)
  MiniTest.expect.equality(identity, nil)
  MiniTest.expect.equality(err, "unavailable")
end

T["a peer in a different PID namespace is unavailable"] = function()
  local other = namespace == "pid:[1]" and "pid:[2]" or "pid:[1]"
  local identity = assert(ProcessIdentity.new(nvim.uv.os_getpid(), other, service))
  MiniTest.finally(function()
    identity:dispose()
  end)
  local opened
  identity:open(function(ok)
    opened = ok
  end)
  assert(nvim.wait(5000, function()
    return opened ~= nil
  end))
  MiniTest.expect.equality(opened, false)
  MiniTest.expect.equality(identity:alive(), false)
end

T["a live duplicate socket does not disguise the original process death"] = function()
  local directory = nvim.fn.tempname()
  assert(nvim.fn.mkdir(directory, "p") == 1)
  local listener, client = assert(nvim.uv.new_pipe(false)), assert(nvim.uv.new_pipe(false))
  local peer, process, identity
  local exited = false
  MiniTest.finally(function()
    if identity ~= nil then
      identity:dispose()
    end
    if process ~= nil and not exited then
      process:kill(9)
    end
    for _, pipe in ipairs({ listener, client, peer }) do
      if not pipe:is_closing() then
        pipe:close()
      end
    end
    nvim.fn.delete(directory, "rf")
  end)
  assert(listener:bind(directory .. "/socket") == 0)
  assert(listener:listen(1, function(err)
    assert(not err)
    peer = assert(nvim.uv.new_pipe(false))
    assert(listener:accept(peer) == 0)
  end) == 0)
  client:connect(directory .. "/socket", function(err)
    assert(not err)
  end)
  assert(nvim.wait(5000, function()
    return peer ~= nil
  end))
  local pid
  process, pid = nvim.uv.spawn("sleep", { args = { "30" }, stdio = { peer } }, function()
    exited = true
    process:close()
  end)
  assert(process and pid)
  identity = assert(ProcessIdentity.new(pid, namespace, service))
  local opened
  identity:open(function(ok)
    opened = ok
  end)
  assert(nvim.wait(5000, function()
    return opened ~= nil
  end))
  assert(opened)
  MiniTest.expect.equality(identity:alive(), true)
  process:kill(9)
  assert(nvim.wait(5000, function()
    return exited
  end))
  MiniTest.expect.equality(client:try_write("x"), 1)
  MiniTest.expect.equality(identity:alive(), false)
end

return T
