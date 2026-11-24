local MiniTest = require("mini.test")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local T = MiniTest.new_set()
local DIGEST = "sha256:" .. string.rep("a", 64)

local function fixture()
  local f = { calls = {}, created = 0, disposed = 0, completed = 0 }
  f.options = {
    selection = { result_id = "saved", input_id = "refresh", plan_digest = DIGEST, snapshot_digest = DIGEST },
    envelope = {
      run_id = "fresh-run",
      envelope_id = "env",
      envelope_revision = 1,
      verification_plan_digest = DIGEST,
      max_sessions = 1,
      expires_at_ms = 9999999999999,
    },
    manifest = {
      agent = { id = "codex" },
      envelope = { id = "env", revision = 1 },
      source_snapshot_digest = DIGEST,
      skills = { mode = "off", view_digest = "sha256:" .. nvim.fn.sha256("") },
    },
    cache = "/cache",
    session_id = "fresh-verifier",
  }
  local session = {
    dispose = function()
      f.disposed = f.disposed + 1
      return true
    end,
  }
  f.options.session_api = {
    create_session = function(api, _, options, ready)
      assert(api == f.options.session_api, "caller-owned Session API identity must not be copied")
      f.created = f.created + 1
      f.launch = options.launch_request
      nvim.schedule(function()
        ready(session)
      end)
      return session
    end,
  }
  f.options.system = function(argv, options, done)
    local input = nvim.json.decode(options.stdin)
    f.calls[#f.calls + 1] = { argv = argv, input = input }
    local response
    if argv[2] == "launch-inputs" then
      response = { manifest_digest = DIGEST }
    elseif argv[2] == "session" then
      response = { state = "running", broker_head = { sequence = 1, digest = DIGEST } }
    elseif argv[2] == "run" then
      if input.kind == "run" then
        response = { kind = "run", receipt = { envelope_digest = DIGEST } }
      else
        local bytes = assert(require("louiselm.acp.launch").encode(input.grant.request))
        response = { kind = "session", receipt = { request_digest = "sha256:" .. nvim.fn.sha256(bytes) } }
      end
    elseif input.kind == "saved_result" then
      response = {
        kind = "saved_result",
        result = { selection = { run_id = "expired-original" }, request = { job = { job_digest = DIGEST } } },
      }
    elseif input.kind == "stage_refresh" then
      response = { kind = "refresh_prepared", input_digest = DIGEST, input = { job = { job_digest = DIGEST } } }
    elseif input.kind == "run" then
      response = { kind = "executed", commands_passed = not f.fail_verification }
    else
      error("unexpected refresh command")
    end
    local function deliver()
      done({ code = 0, stdout = nvim.json.encode(response), stderr = "" })
    end
    if f.hold and input.kind == f.hold then
      f.deliver = deliver
    else
      nvim.schedule(deliver)
    end
    return {
      kill = function()
        f.killed = true
      end,
    }
  end
  function f.start()
    return require("louiselm.workflow.saved_result").refresh(f.options, function(result, err)
      MiniTest.expect.equality(nvim.in_fast_event(), false)
      f.completed = f.completed + 1
      f.result, f.error = result, err
    end)
  end
  return f
end

T["refresh uses a fresh unpaid verifier and returns a new unaccepted saved review"] = function()
  local f = fixture()
  assert(f.start())
  assert(nvim.wait(1000, function()
    return f.completed == 1
  end))
  assert(f.result, f.error)
  MiniTest.expect.equality(f.created, 1)
  MiniTest.expect.equality(f.disposed, 1)
  MiniTest.expect.equality(f.launch.run_id, "fresh-run")
  for _, call in ipairs(f.calls) do
    assert(call.argv[2] ~= "promotion")
    if type(call.input) == "table" and call.input.kind == "session" then
      MiniTest.expect.equality(call.input.grant.role, "fixed_verifier")
      MiniTest.expect.equality(call.input.grant.provider_requests, nvim.NIL)
      MiniTest.expect.equality(call.input.grant.beads_mutations, nvim.NIL)
    end
  end
end

T["failed verification leaves the original pending and never requests acceptance"] = function()
  local f = fixture()
  f.fail_verification = true
  assert(f.start())
  assert(nvim.wait(1000, function()
    return f.completed == 1
  end))
  assert(not f.result and f.error:find("original remains pending", 1, true))
  MiniTest.expect.equality(f.disposed, 1)
end

T["cancellation ignores late preparation without starting a Session"] = function()
  local f = fixture()
  f.hold = "stage_refresh"
  local cancel = assert(f.start())
  assert(nvim.wait(1000, function()
    return f.deliver ~= nil
  end))
  cancel()
  cancel()
  f.deliver()
  nvim.wait(20, function()
    return false
  end)
  MiniTest.expect.equality(f.created, 0)
  MiniTest.expect.equality(f.completed, 1)
  MiniTest.expect.equality(f.killed, true)
end

T["paid authority, original Run reuse and invalid selections are refused"] = function()
  local f = fixture()
  f.options.envelope.provider_requests = {}
  local cancel, err = f.start()
  assert(not cancel and err)
  MiniTest.expect.equality(#f.calls, 0)
  f = fixture()
  f.options.envelope.run_id = "expired-original"
  assert(f.start())
  assert(nvim.wait(1000, function()
    return f.completed == 1
  end))
  assert(not f.result and f.error)
  MiniTest.expect.equality(f.created, 0)
end

return T
