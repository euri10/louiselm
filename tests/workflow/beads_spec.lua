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
  local f = { calls = {}, prepared = {}, workers = {}, verifiers = {}, handoffs = {}, finished = 0 }
  f.options = {
    agent_id = "codex",
    envelope = {
      run_id = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee",
      envelope_id = "env",
      envelope_revision = 1,
      expires_at_ms = 9999999999999,
      verification_plan_digest = DIGEST,
      max_sessions = 4,
      bead_scope = { issue_ids = { "b-1", "b-2" }, max_mutations = 2 },
    },
    bead_ids = { "b-1", "b-2" },
    prepare = function(id, done, expected_head)
      MiniTest.expect.equality(nvim.in_fast_event(), false)
      f.prepared[#f.prepared + 1] = id
      if expected_head then
        f.prepared_heads = f.prepared_heads or {}
        f.prepared_heads[#f.prepared_heads + 1] = expected_head
      end
      local index = #f.prepared
      local grant = {
        request = {
          schema = "louiselm.launch.request/3",
          protocol_version = 1,
          request_id = "request-" .. index,
          authorization_id = "authorization-" .. index,
          session_id = "worker-" .. index,
          run_id = f.options.envelope.run_id,
          envelope_id = "env",
          envelope_revision = 1,
          agent_id = "codex",
          skills = { mode = "enabled", generation_digest = DIGEST, view_digest = "sha256:" .. string.rep("c", 64) },
          session_input_manifest_id = DIGEST,
        },
        beads_mutations = { issue_ids = { id }, role = "worker" },
      }
      f.prepared_grant = grant
      local verifier_grant = {
        request = nvim.tbl_extend("force", {}, grant.request, {
          request_id = "verify-request-" .. index,
          authorization_id = "verify-authorization-" .. index,
          session_id = "verifier-" .. index,
        }),
      }
      f.verifier_grant = verifier_grant
      f.prepare_done = function()
        later(done, {
          grant = grant,
          prompt = "Work on " .. id,
          base_commit = f.snapshot_wrong and string.rep("f", 40) or expected_head,
          verification = {
            snapshot = "/fixture/snapshot",
            snapshot_digest = DIGEST,
            plan = "/fixture/plan",
            plan_digest = DIGEST,
            verifier_grant = verifier_grant,
          },
        })
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
      if argv[2] == "verification" then
        local input = nvim.json.decode(options.stdin)
        if input.kind == "stage" then
          f.stage_requests = f.stage_requests or {}
          f.stage_requests[#f.stage_requests + 1] = input
          response = { kind = "staged", input_digest = DIGEST }
        elseif input.kind == "park" then
          response = { kind = "parked", head = { sequence = 2, digest = DIGEST } }
        elseif input.kind == "export" then
          response = {
            kind = "exported",
            export_digest = DIGEST,
            evidence = {
              job = { job_digest = DIGEST, command_count = 1 },
            },
          }
        elseif input.kind == "run" then
          f.run_calls = (f.run_calls or 0) + 1
          f.last_run = input.request
          response = { kind = "executed", commands_passed = true }
        elseif input.kind == "status" then
          response = f.unknown_verification
              and { kind = "status", commands_passed = false, status = { state = "unknown" } }
            or {
              kind = "status",
              commands_passed = not f.fail_verification,
              status = {
                state = "completed",
                detail = {
                  execution = {
                    request = { request_id = "verify-observation-" .. #f.verifiers },
                    steps = { { state = "completed", exit_code = f.fail_verification and 7 or 0, timed_out = false } },
                  },
                },
              },
            }
        end
        if input.kind == "run" and f.unknown_verification then
          later(done, { code = 1, stdout = "", stderr = "" })
          return { kill = function() end }
        end
      elseif argv[2] == "session" then
        response = {
          state = "running",
          broker_head = {
            sequence = argv[4]:match("^verifier") and 1 or 1,
            digest = DIGEST,
          },
        }
      elseif argv[2] == "promotion" then
        local input = nvim.json.decode(options.stdin)
        if argv[3] == "preview" then
          f.preview_calls = (f.preview_calls or 0) + 1
          input.selection.expires_at_ms = f.preview_expiry or 9999999999999
          response = {
            schema = "louiselm.run-promotion-preview/1",
            selection = input.selection,
            changes = { added = { "file" }, modified = {}, deleted = {} },
            approval_digest = DIGEST,
          }
        else
          f.commit_calls = (f.commit_calls or 0) + 1
          if f.fail_promotion_commit then
            later(done, { code = 1, stdout = "", stderr = f.promotion_stderr or "private unstructured diagnostic" })
            return { kill = function() end }
          end
          response = {
            schema = "louiselm.run-promotion-commit/1",
            bead_id = input.selection.bead_id,
            commit = string.rep("b", 40),
          }
        end
      elseif argv[2] == "run" then
        local input = nvim.json.decode(options.stdin)
        if input.kind == "provider_status" then
          response = f.provider_status
            or {
              kind = "provider_status",
              run_id = f.options.envelope.run_id,
              envelope_digest = DIGEST,
              hold = f.provider_hold or nvim.NIL,
            }
          later(done, { code = 0, stdout = nvim.json.encode(response), stderr = "" })
          return { kill = function() end }
        end
        if input.kind == "session" and f.child_hold then
          response = { kind = "held", envelope_digest = DIGEST, hold = f.child_hold }
          later(done, { code = 0, stdout = nvim.json.encode(response), stderr = "" })
          return { kill = function() end }
        end
        if input.kind == "bead_failure" then
          MiniTest.expect.equality(input.run_id, nil)
          f.failure_requests = f.failure_requests or {}
          f.failure_requests[#f.failure_requests + 1] = input.report
          response = {
            kind = "bead_failure",
            receipt = {
              request_id = input.report.request_id,
              operation_id = "11111111-2222-4333-8444-555555555555",
              outcome = { kind = f.unknown_comment and "unknown" or "completed" },
            },
          }
          later(done, { code = 0, stdout = nvim.json.encode(response), stderr = "" })
          return { kill = function() end }
        end
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
          return "turn-" .. #f.workers
        end
        if options.permission_policy.name == "contained-verifier" then
          f.verifiers[#f.verifiers + 1] = worker
        else
          f.workers[#f.workers + 1] = worker
        end
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
    assert(self.runner:start(function(ok, err, summary)
      self.finished = self.finished + 1
      self.ok, self.error, self.summary = ok, err, summary
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
  MiniTest.expect.equality(worker.options.start_timeout_ms, 60000)
  worker.complete()
  wait(function()
    return #f.handoffs == 1
  end)
  MiniTest.expect.equality(#f.prepared, 1)
  MiniTest.expect.equality(f.handoffs[1].bead_id, "b-1")
  MiniTest.expect.equality(f.handoffs[1].envelope_digest, DIGEST)
  MiniTest.expect.equality(f.handoffs[1].request_digest == DIGEST, false)
  MiniTest.expect.equality(f.handoffs[1].verification_passed, true)
  MiniTest.expect.equality(f.handoffs[1].verification.detail.execution.steps[1].exit_code, 0)
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
  MiniTest.expect.equality(f.summary.accepted, { "b-1", "b-2" })
  MiniTest.expect.equality(f.summary.failed, {})
  MiniTest.expect.equality(f.runner.executor:inspect().status, "completed")
  MiniTest.expect.equality(#f.verifiers, 2)
  for _, verifier in ipairs(f.verifiers) do
    MiniTest.expect.equality(verifier.options.start_timeout_ms, 60000)
  end
  MiniTest.expect.equality(f.last_run.operation.kind, "run")
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

T["requires capacity for a distinct verifier per Bead"] = function()
  local f = fixture()
  f.options.envelope.max_sessions = 3
  local runner, err = require("louiselm.workflow.beads").new(f.options)
  MiniTest.expect.equality(runner, nil)
  MiniTest.expect.equality(err, "Run envelope needs one worker and one verifier Session per Bead")
  MiniTest.expect.equality(f.calls, {})
end

T["rejects plan drift and ordinary verifier authority before staging"] = function()
  local f = fixture()
  f.options.envelope.verification_plan_digest = "sha256:" .. string.rep("b", 64)
  f:start()
  wait(function()
    return f.finished == 1
  end)
  MiniTest.expect.equality(f.error, "verification requires the approved plan, baseline and verifier grant")
  MiniTest.expect.equality(#f.workers, 0)
  f = fixture()
  f.hold_prepare = true
  f:start()
  wait(function()
    return f.prepare_done ~= nil
  end)
  f.verifier_grant.commands = { command_digest = DIGEST }
  f.prepare_done()
  wait(function()
    return f.finished == 1
  end)
  MiniTest.expect.equality(f.error, "verifier must be a distinct grant without ordinary capabilities")
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

T["worker failure gets one broker comment and the Run continues"] = function()
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
    return f.workers[2] and f.workers[2].complete
  end)
  MiniTest.expect.equality(f.prepared, { "b-1", "b-2" })
  MiniTest.expect.equality(#f.failure_requests, 1)
  MiniTest.expect.equality(f.failure_requests[1].bead_id, "b-1")
  MiniTest.expect.equality(f.failure_requests[1].observation_ids, { "turn-1" })
  MiniTest.expect.equality(nvim.inspect(f.failure_requests):find("worker failed", 1, true), nil)
  f.workers[2].complete()
  wait(function()
    return #f.handoffs == 2
  end)
  later(f.verify_done, true)
  wait(function()
    return f.finished == 1
  end)
  MiniTest.expect.equality(f.ok, true)
  MiniTest.expect.equality(f.summary.accepted, { "b-2" })
  MiniTest.expect.equality(f.summary.failed[1].bead_id, "b-1")
  MiniTest.expect.equality(f.summary.failed[1].comment_operation_id, "11111111-2222-4333-8444-555555555555")
  MiniTest.expect.equality(f.summary.text:find("b-1", 1, true) ~= nil, true)
  MiniTest.expect.equality(f.summary.text:find("worker failed", 1, true), nil)
end

T["a durable Provider hold stops the Run before another child"] = function()
  for _, reason in ipairs({ "exhausted", "expired" }) do
    local f = fixture()
    f.options.on_worker = function(_, continue)
      continue(false)
    end
    f:start()
    wait(function()
      return f.workers[1] and f.workers[1].complete
    end)
    -- Captured Run 1405a0d6, live-ready-20261020.4E1JBs/audit.jsonl:
    -- durable exhausted hold -> worker prompt error -> successor authorization.
    -- Expired uses the same typed boundary; it was not observed in that capture.
    f.provider_hold = {
      run_id = f.options.envelope.run_id,
      reason = reason,
      held_at_ms = 1792490237790,
    }
    f.workers[1].complete("exceeded retry limit, last status: 429 Too Many Requests")
    wait(function()
      return f.finished == 1 or #f.workers > 1
    end)
    MiniTest.expect.equality(f.finished, 1)
    MiniTest.expect.equality(f.ok, false)
    MiniTest.expect.equality(f.error, "Run Provider budget " .. reason .. "; explicit operator action required")
    MiniTest.expect.equality(f.runner.status, "held")
    MiniTest.expect.equality(f.runner.run.status, "parked")
    MiniTest.expect.equality(f.summary.provider_hold, f.provider_hold)
    MiniTest.expect.equality(f.prepared, { "b-1" })
    MiniTest.expect.equality(#f.workers, 1)
    MiniTest.expect.equality(#f.verifiers, 0)
    MiniTest.expect.equality(f.workers[1].disposed, 0)
  end
end

T["an upstream 429 without a broker hold remains an ordinary worker failure"] = function()
  local f = fixture()
  f.options.on_worker = function(_, continue)
    continue(false)
  end
  f:start()
  wait(function()
    return f.workers[1] and f.workers[1].complete
  end)
  f.workers[1].complete("429 Too Many Requests")
  wait(function()
    return f.workers[2] and f.workers[2].complete
  end)
  MiniTest.expect.equality(f.runner.status, "active")
  MiniTest.expect.equality(#f.failure_requests, 1)
end

T["malformed or foreign Provider status cannot advance a Run"] = function()
  for _, mutation in ipairs({
    function(status)
      status.run_id = "other"
    end,
    function(status)
      status.envelope_digest = "sha256:" .. string.rep("b", 64)
    end,
    function(status)
      status.hold = nil
    end,
    function(status)
      status.hold = { run_id = "other", reason = "exhausted", held_at_ms = 1 }
    end,
    function(status)
      status.hold.reason = "429"
    end,
    function(status)
      status.hold.held_at_ms = -1
    end,
  }) do
    local f = fixture()
    f.provider_status = {
      kind = "provider_status",
      run_id = f.options.envelope.run_id,
      envelope_digest = DIGEST,
      hold = { run_id = f.options.envelope.run_id, reason = "exhausted", held_at_ms = 1 },
    }
    mutation(f.provider_status)
    f:start()
    wait(function()
      return f.workers[1] and f.workers[1].complete
    end)
    f.workers[1].complete()
    wait(function()
      return f.finished == 1
    end)
    MiniTest.expect.equality(f.error, "Control broker returned invalid Provider status")
    MiniTest.expect.equality(#f.verifiers, 0)
    MiniTest.expect.equality(#f.workers, 1)
  end
end

T["a hold arriving after the status check still prevents verifier launch"] = function()
  local f = fixture()
  f:start()
  wait(function()
    return f.workers[1] and f.workers[1].complete
  end)
  f.child_hold = { run_id = f.options.envelope.run_id, reason = "exhausted", held_at_ms = 1 }
  f.workers[1].complete()
  wait(function()
    return f.finished == 1
  end)
  MiniTest.expect.equality(f.runner.status, "held")
  MiniTest.expect.equality(#f.verifiers, 0)
  MiniTest.expect.equality(f.summary.provider_hold, f.child_hold)
end

T["a Provider hold reaches the operator while the worker prompt remains pending"] = function()
  local f = fixture()
  f:start()
  wait(function()
    return f.workers[1] and f.workers[1].complete
  end)
  f.provider_hold = { run_id = f.options.envelope.run_id, reason = "exhausted", held_at_ms = 1 }
  wait(function()
    return f.finished == 1
  end)
  MiniTest.expect.equality(f.runner.status, "held")
  MiniTest.expect.equality(f.workers[1].cancelled, 1)
  MiniTest.expect.equality(f.workers[1].disposed, 0)
  f.workers[1].complete("late cancellation")
  nvim.wait(20, function()
    return false
  end, 1)
  MiniTest.expect.equality(f.finished, 1)
  MiniTest.expect.equality(#f.workers, 1)
  MiniTest.expect.equality(#f.verifiers, 0)
end

T["failed command is reported, commented and advances the next Bead"] = function()
  local f = fixture()
  f.fail_verification = true
  f:start()
  wait(function()
    return f.workers[1] and f.workers[1].complete
  end)
  f.workers[1].complete()
  wait(function()
    return #f.handoffs == 1
  end)
  MiniTest.expect.equality(f.handoffs[1].verification.detail.execution.steps[1].exit_code, 7)
  MiniTest.expect.equality(f.handoffs[1].verification_passed, false)
  later(f.verify_done, true)
  wait(function()
    return f.workers[2] and f.workers[2].complete
  end)
  MiniTest.expect.equality(f.failure_requests[1].outcome, "verification_failed")
  MiniTest.expect.equality(f.failure_requests[1].observation_ids, { "turn-1", "verify-observation-1" })
  f.workers[2].complete()
  wait(function()
    return #f.handoffs == 2
  end)
  later(f.verify_done, true)
  wait(function()
    return f.finished == 1
  end)
  MiniTest.expect.equality(f.ok, true)
  MiniTest.expect.equality(f.summary.failed[1].outcome, "verification_failed")
end

T["operator rejection is commented and a lost comment result stops without retry"] = function()
  local f = fixture()
  f:start()
  wait(function()
    return f.workers[1] and f.workers[1].complete
  end)
  f.workers[1].complete()
  wait(function()
    return #f.handoffs == 1
  end)
  later(f.verify_done, false)
  wait(function()
    return f.workers[2] and f.workers[2].complete
  end)
  MiniTest.expect.equality(f.failure_requests[1].outcome, "rejected")
  f.unknown_comment = true
  f.workers[2].complete("failed after rejection")
  wait(function()
    return #f.handoffs == 2
  end)
  later(f.verify_done, false)
  wait(function()
    return f.finished == 1
  end)
  MiniTest.expect.equality(f.ok, false)
  MiniTest.expect.equality(f.error, "broker Bead failure comment is not confirmed")
  MiniTest.expect.equality(#f.failure_requests, 2)
  MiniTest.expect.equality(f.summary.failed[1].outcome, "rejected")
  MiniTest.expect.equality(#f.summary.failed, 2)
  MiniTest.expect.equality(f.summary.failed[2].comment_status, "unconfirmed")
  MiniTest.expect.equality(f.summary.text:find("unconfirmed comment", 1, true) ~= nil, true)
end

T["spent verification with a lost reply stays unknown and is not replayed"] = function()
  local f = fixture()
  f.unknown_verification = true
  f:start()
  wait(function()
    return f.workers[1] and f.workers[1].complete
  end)
  f.workers[1].complete()
  wait(function()
    return #f.handoffs == 1
  end)
  MiniTest.expect.equality(f.handoffs[1].verification.state, "unknown")
  MiniTest.expect.equality(f.run_calls, 1)
  later(f.verify_done, true)
  wait(function()
    return f.finished == 1
  end)
  MiniTest.expect.equality(f.error, "Run verification outcome is uncertain")
end

T["accepted promotion commits before the next Bead snapshots the new HEAD"] = function()
  local f = fixture()
  local first_head = string.rep("a", 40)
  f.options.worktree = { path = "/run-worktree", journal_parent = "/journal", head = first_head }
  f.options.on_promotion = function(preview, decide)
    f.preview = preview
    f.decide = decide
  end
  f:start()
  wait(function()
    return f.workers[1] and f.workers[1].complete
  end)
  f.workers[1].complete()
  wait(function()
    return f.verify_done ~= nil
  end)
  f.verify_done(true)
  wait(function()
    return f.decide ~= nil
  end)
  MiniTest.expect.equality(f.commit_calls, nil)
  MiniTest.expect.equality(f.prepared, { "b-1" })
  MiniTest.expect.equality(f.preview.changes.added, { "file" })
  f.decide(true)
  wait(function()
    return f.workers[2] and f.workers[2].complete
  end)
  MiniTest.expect.equality(f.commit_calls, 1)
  MiniTest.expect.equality(f.workers[1].disposed, 1)
  MiniTest.expect.equality(f.stage_requests[1].expected_base_commit, first_head)
  MiniTest.expect.equality(f.stage_requests[2].expected_base_commit, string.rep("b", 40))
  MiniTest.expect.equality(f.prepared_heads, { first_head, string.rep("b", 40) })
  f.decide = nil
  f.workers[2].complete()
  wait(function()
    return #f.handoffs == 2
  end)
  f.verify_done(true)
  wait(function()
    return f.decide ~= nil
  end)
  f.decide(true)
  wait(function()
    return f.finished == 1
  end)
  MiniTest.expect.equality(f.summary.branch, "run/" .. f.options.envelope.run_id)
  MiniTest.expect.equality(f.summary.commits["b-1"], string.rep("b", 40))
  MiniTest.expect.equality(f.summary.text:find("Commit b-1: " .. string.rep("b", 40), 1, true) ~= nil, true)
end

T["rejected promotion does not commit and a stale next snapshot refuses"] = function()
  local f = fixture()
  local head = string.rep("a", 40)
  f.options.worktree = { path = "/run-worktree", journal_parent = "/journal", head = head }
  f.options.on_promotion = function(_, decide)
    f.decide = decide
  end
  f.snapshot_wrong = true
  f:start()
  wait(function()
    return f.finished == 1
  end)
  MiniTest.expect.equality(f.error, "next Bead snapshot does not match the Run branch HEAD")
  MiniTest.expect.equality(#f.workers, 0)
  local rejected = fixture()
  rejected.options.worktree = { path = "/run-worktree", journal_parent = "/journal", head = head }
  rejected.options.on_promotion = function(_, decide)
    rejected.decide = decide
  end
  rejected:start()
  wait(function()
    return rejected.workers[1] and rejected.workers[1].complete
  end)
  rejected.workers[1].complete()
  wait(function()
    return rejected.verify_done ~= nil
  end)
  rejected.verify_done(true)
  wait(function()
    return rejected.decide ~= nil
  end)
  rejected.decide(false)
  wait(function()
    return #rejected.prepared == 2
  end)
  MiniTest.expect.equality(rejected.commit_calls, nil)
  MiniTest.expect.equality(rejected.prepared_heads, { head, head })
end

T["promotion preview requires a usable deadline before opening the prompt"] = function()
  local f = fixture()
  f.options.worktree = { path = "/run-worktree", journal_parent = "/journal", head = string.rep("a", 40) }
  f.options.on_promotion = function()
    error("invalid expiry must not reach the UI")
  end
  f.preview_expiry = nvim.NIL
  f:start()
  wait(function()
    return f.workers[1] and f.workers[1].complete
  end)
  f.workers[1].complete()
  wait(function()
    return f.verify_done ~= nil
  end)
  f.verify_done(true)
  wait(function()
    return f.finished == 1
  end)
  MiniTest.expect.equality(f.error, "Run promotion returned a mismatched preview")
  MiniTest.expect.equality(f.commit_calls, nil)
end

T["promotion refusals stop before another snapshot and expose only structured safe messages"] = MiniTest.new_set({
  parametrize = {
    {
      nvim.json.encode({
        schema = "louiselm.run-promotion-error/1",
        message = "promotion preview expired; no changes applied",
      }),
      "Run promotion commit refused: promotion preview expired; no changes applied",
    },
    {
      nvim.json.encode({
        schema = "louiselm.run-promotion-error/1",
        message = "promotion applied but Git commit failed; inspect worktree",
      }),
      "Run promotion commit refused: promotion applied but Git commit failed; inspect worktree",
    },
    { "private unstructured diagnostic", "Run promotion commit outcome unavailable; inspect state before any retry" },
    {
      nvim.json.encode({ schema = "wrong-schema", message = "private diagnostic" }),
      "Run promotion commit outcome unavailable; inspect state before any retry",
    },
    {
      nvim.json.encode({ schema = "louiselm.run-promotion-error/1", message = "unsafe\ncontrol character" }),
      "Run promotion commit outcome unavailable; inspect state before any retry",
    },
  },
})

T["promotion refusals stop before another snapshot and expose only structured safe messages"]["commit"] = function(
  stderr,
  expected
)
  local f = fixture()
  f.options.worktree = { path = "/run-worktree", journal_parent = "/journal", head = string.rep("a", 40) }
  f.options.on_promotion = function(_, decide)
    decide(true)
  end
  f.fail_promotion_commit = true
  f.promotion_stderr = stderr
  f:start()
  wait(function()
    return f.workers[1] and f.workers[1].complete
  end)
  f.workers[1].complete()
  wait(function()
    return f.verify_done ~= nil
  end)
  f.verify_done(true)
  wait(function()
    return f.finished == 1
  end)
  MiniTest.expect.equality(f.error, expected)
  MiniTest.expect.equality(f.prepared, { "b-1" })
  MiniTest.expect.equality(f.summary.accepted, {})
  MiniTest.expect.equality(f.summary.commits, {})
  MiniTest.expect.equality(f.commit_calls, 1)
end

return T
