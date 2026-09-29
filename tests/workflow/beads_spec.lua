local MiniTest = require("mini.test")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local T = MiniTest.new_set()
local DIGEST = "sha256:" .. string.rep("a", 64)

local function later(callback, first, second)
  local timer = assert(nvim.uv.new_timer())
  timer:start(0, 0, function()
    timer:close()
    callback(first, second)
  end)
end

local function fixture()
  local f = { calls = {}, prepared = {}, workers = {}, handoffs = {}, finished = 0 }
  f.options = {
    agent_id = "codex",
    envelope = {
      run_id = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee",
      envelope_id = "env",
      envelope_revision = 1,
      bead_scope = { issue_ids = { "b-1", "b-2" } },
    },
    bead_ids = { "b-1", "b-2" },
    prepare = function(id, done)
      MiniTest.expect.equality(nvim.in_fast_event(), false)
      f.prepared[#f.prepared + 1] = id
      local index = #f.prepared
      local grant = {
        request = {
          schema = "louiselm.launch.request/2",
          protocol_version = 1,
          request_id = "request-" .. index,
          authorization_id = "authorization-" .. index,
          session_id = "worker-" .. index,
          run_id = f.options.envelope.run_id,
          envelope_id = "env",
          envelope_revision = 1,
          agent_id = "codex",
          skill_generation_id = DIGEST,
          session_input_manifest_id = DIGEST,
        },
        beads_mutations = { issue_ids = { id }, role = "worker" },
      }
      f.prepared_grant = grant
      f.prepare_done = function()
        later(done, { grant = grant, prompt = "Work on " .. id })
      end
      if not f.hold_prepare then
        f.prepare_done()
      end
      return true
    end,
    on_worker = function(result, done)
      MiniTest.expect.equality(nvim.in_fast_event(), false)
      f.handoffs[#f.handoffs + 1] = result
      f.verify_done = done
    end,
    system = function(argv, options, done)
      f.calls[#f.calls + 1] = argv
      local response
      if argv[2] == "run" then
        local input = nvim.json.decode(options.stdin)
        local request = input.grant and input.grant.request
        response = {
          kind = input.kind,
          receipt = {
            schema = input.kind == "run" and "louiselm.broker.run-authorization/1"
              or "louiselm.broker.child-authorization/1",
            run_id = f.options.envelope.run_id,
            envelope_revision = 1,
            envelope_digest = DIGEST,
            session_id = request and request.session_id,
            authorization_id = request and request.authorization_id,
            request_digest = request
              and "sha256:" .. nvim.fn.sha256(assert(require("louiselm.acp.launch").encode(request))),
          },
        }
        if f.wrong_digest and request then
          response.receipt.request_digest = DIGEST
        end
      else
        response = { token = "private-ledger-token" }
      end
      later(done, { code = 0, stdout = nvim.json.encode(response), stderr = "" })
      return { kill = function() end }
    end,
    session_api = {
      create_session = function(_, name, options, ready)
        MiniTest.expect.equality(nvim.in_fast_event(), false)
        local worker = { status = "starting", options = options, agent = name, cancelled = 0, disposed = 0 }
        function worker:inspect()
          return { status = self.status, agent = name, acp_session_id = "acp-" .. #f.workers, working_dir = options.cwd }
        end
        function worker:cancel()
          self.cancelled = self.cancelled + 1
          return true
        end
        function worker:dispose()
          self.disposed = self.disposed + 1
          self.status = "disposed"
          return true
        end
        function worker:prompt(prompt, done)
          self.status = "running"
          self.prompt_text = prompt
          self.complete = function(err)
            self.status = "ready"
            later(done, { stopReason = "end_turn" }, err)
          end
          return "prompt"
        end
        f.workers[#f.workers + 1] = worker
        later(function()
          worker.status = "ready"
          ready(worker)
        end)
        return worker
      end,
    },
  }
  function f:start()
    self.runner = assert(require("louiselm.workflow.beads").new(self.options))
    MiniTest.finally(function()
      self.runner:dispose()
    end)
    assert(self.runner:start(function(ok, err)
      self.finished = self.finished + 1
      self.ok, self.error = ok, err
    end))
  end
  return f
end

local function wait(predicate)
  MiniTest.expect.equality(nvim.wait(2000, predicate, 1), true)
end

T["sequences contained workers across the verification continuation"] = function()
  local f = fixture()
  f:start()
  wait(function()
    return #f.workers == 1 and f.workers[1].complete ~= nil
  end)
  MiniTest.expect.equality(f.prepared, { "b-1" })
  local worker = f.workers[1]
  MiniTest.expect.equality(worker.options.permission_policy:evaluate({ kind = "unknown" }), "allow")
  MiniTest.expect.equality(worker.options.env, nil)
  MiniTest.expect.equality(worker.options.launch_request.session_id, "worker-1")
  MiniTest.expect.equality(worker.options.broker_session_id, "worker-1")
  worker.complete()
  wait(function()
    return #f.handoffs == 1
  end)
  MiniTest.expect.equality(#f.prepared, 1)
  MiniTest.expect.equality(f.handoffs[1].bead_id, "b-1")
  MiniTest.expect.equality(f.handoffs[1].envelope_digest, DIGEST)
  MiniTest.expect.equality(f.handoffs[1].request_digest == DIGEST, false)
  local continue = f.verify_done
  later(continue, true)
  later(continue, true)
  wait(function()
    return #f.workers == 2 and f.workers[2].complete ~= nil
  end)
  f.workers[2].complete()
  wait(function()
    return #f.handoffs == 2
  end)
  later(f.verify_done, true)
  wait(function()
    return f.finished == 1
  end)
  MiniTest.expect.equality(f.ok, true)
  MiniTest.expect.equality(f.runner.executor:inspect().status, "completed")
  MiniTest.expect.equality(#f.calls, 5) -- Run authorization, admission, attachment, two child grants.
end

T["refuses an unapproved list before effects and widened children before launch"] = function()
  local f = fixture()
  f.options.bead_ids = { "outside" }
  local runner, err = require("louiselm.workflow.beads").new(f.options)
  MiniTest.expect.equality(runner, nil)
  MiniTest.expect.equality(err, "Bead is outside the approved list: outside")
  MiniTest.expect.equality(f.calls, {})
  f = fixture()
  f.hold_prepare = true
  f:start()
  wait(function()
    return f.prepare_done ~= nil
  end)
  f.prepared_grant.beads_mutations.issue_ids = { "b-1", "outside" }
  f.prepare_done()
  wait(function()
    return f.finished == 1
  end)
  MiniTest.expect.equality(f.error, "worker grant must name only its assigned Bead with the worker role")
  MiniTest.expect.equality(#f.workers, 0)
end

T["rejects a mismatched child receipt"] = function()
  local f = fixture()
  f.wrong_digest = true
  f:start()
  wait(function()
    return f.finished == 1
  end)
  MiniTest.expect.equality(f.error, "broker returned a mismatched child authorization")
  MiniTest.expect.equality(#f.workers, 0)
end

T["disposal cancels the owned worker and suppresses late completion"] = function()
  local f = fixture()
  f:start()
  wait(function()
    return #f.workers == 1 and f.workers[1].complete ~= nil
  end)
  assert(f.runner:dispose())
  f.workers[1].complete()
  wait(function()
    return f.finished == 1
  end)
  MiniTest.expect.equality(f.workers[1].cancelled, 1)
  MiniTest.expect.equality(f.workers[1].disposed, 1)
  MiniTest.expect.equality(f.handoffs, {})
  MiniTest.expect.equality(#f.prepared, 1)
  MiniTest.expect.equality(f.error, "Bead Run disposed")
end

T["disposal during preparation cannot start a late worker"] = function()
  local f = fixture()
  f.hold_prepare = true
  f:start()
  wait(function()
    return f.prepare_done ~= nil
  end)
  assert(f.runner:dispose())
  f.prepare_done()
  wait(function()
    return f.finished == 1
  end)
  MiniTest.expect.equality(#f.workers, 0)
end

T["real ACP permission requests complete without a human event"] = function()
  local f = fixture()
  local root = nvim.fn.getcwd()
  local api = assert(require("louiselm.session").new({
    codex = {
      provider = "test-service",
      command = nvim.v.progpath,
      args = {
        "--headless",
        "--noplugin",
        "-u",
        root .. "/tests/mock/init.lua",
        "-c",
        "lua require('louiselm.dev.mock_agent').run()",
      },
      env = { LOUISELM_MOCK_MODE = "permission" },
    },
  }))
  MiniTest.finally(function()
    api:dispose()
  end)
  local permissions = 0
  f.options.session_api = {
    create_session = function(_, name, options, ready)
      -- Only the privileged process boundary is doubled. Keep the controller's
      -- real permission policy and Session lifecycle over the offline ACP peer.
      local opts = nvim.tbl_extend("force", {}, options, { cwd = root })
      opts.launch_request, opts.broker_session_id = nil, nil
      opts.on_event = function(event)
        if event.type == "permission_requested" then
          permissions = permissions + 1
        end
      end
      return api:create_session(name, opts, ready)
    end,
  }
  f.options.on_worker = function(result, continue)
    MiniTest.expect.equality(result.error, nil)
    f.handoffs[#f.handoffs + 1] = result
    continue(true)
  end
  f:start()
  wait(function()
    return f.finished == 1
  end)
  MiniTest.expect.equality(f.ok, true)
  MiniTest.expect.equality(#f.handoffs, 2)
  MiniTest.expect.equality(permissions, 0)
end

T["worker failure is handed off once without retry and verification can stop"] = function()
  local f = fixture()
  f:start()
  wait(function()
    return #f.workers == 1 and f.workers[1].complete ~= nil
  end)
  f.workers[1].complete("worker failed")
  wait(function()
    return #f.handoffs == 1
  end)
  MiniTest.expect.equality(f.handoffs[1].error, "worker failed")
  later(f.verify_done, false, "verification refused")
  wait(function()
    return f.finished == 1
  end)
  MiniTest.expect.equality(f.error, "verification refused")
  MiniTest.expect.equality(f.prepared, { "b-1" })
end

return T
