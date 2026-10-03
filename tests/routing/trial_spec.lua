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
  local expires = os.time() * 1000 + 60000
  local f = { calls = {}, sessions = {}, sends = 0, completions = 0 }
  f.selection = {
    schema = "louiselm.qualification-run/1",
    policy_revision = "policy-1",
    workload = { kind = "main", id = "selected-suite" },
    retention_days = 7,
    manifest = {
      routes = {
        baseline = {
          mode = "direct",
          agent = "codex",
          provider = "openai",
          options = { model = "big", reasoning_effort = "high" },
        },
        candidate = {
          mode = "direct",
          agent = "codex",
          provider = "openai",
          options = { model = "small", reasoning_effort = "low" },
        },
      },
      limits = { model_requests = 12, elapsed_seconds = 60, input_bytes = 10000, output_bytes = 2000 },
      fixtures = {
        {
          id = "bulk",
          provenance = { reference = "selected fixture" },
          acceptance = {
            reference_checks = { { answer_contains = "Idle", citation = "[A]" } },
            commands = {},
            human_review = nvim.NIL,
          },
        },
        {
          id = "mechanical",
          provenance = { reference = "selected fixture" },
          acceptance = { reference_checks = {}, commands = { { "python3", "test.py" } }, human_review = nvim.NIL },
        },
        {
          id = "reasoning",
          provenance = { reference = "selected fixture" },
          acceptance = { reference_checks = {}, commands = {}, human_review = "operator rubric" },
        },
      },
    },
    prompts = { "selected bulk input", "selected mechanical input", "selected reasoning input" },
    snapshot = "/fixture/snapshot",
    snapshot_digest = DIGEST,
    base_commit = string.rep("b", 40),
    plan = "/fixture/plan",
    cache = "/fixture/cache",
    input_manifest = {
      agent = { id = "codex" },
      envelope = { id = "env", revision = 1 },
      skill_generation = { generation_digest = DIGEST },
      source_snapshot_digest = DIGEST,
      source_base_digest = DIGEST,
      cache_base_digest = DIGEST,
    },
    envelope = {
      schema = "louiselm.broker.run-envelope/1",
      run_id = "trial-1",
      envelope_id = "env",
      envelope_revision = 1,
      controller_uid = 1000,
      expires_at_ms = expires,
      max_sessions = 4,
      verification_plan_digest = DIGEST,
      provider_requests = {
        provider = "openai",
        models = { "big", "small" },
        max_effort = "high",
        max_run_requests = 12,
        expires_at_ms = expires,
      },
    },
  }
  f.options = {
    session_api = {
      create_session = function(_, agent, options, done)
        MiniTest.expect.equality(nvim.in_fast_event(), false)
        local verifier = options.broker_session_id:find("verifier", 1, true) ~= nil
        local state = {
          agent = agent,
          acp_session_id = options.broker_session_id,
          status = "ready",
          config_options = {
            {
              id = "model",
              category = "model",
              type = "select",
              current_value = "big",
              options = { { value = "big" }, { value = "small" } },
            },
            {
              id = "reasoning_effort",
              category = "thought_level",
              type = "select",
              current_value = "high",
              options = { { value = "high" }, { value = "low" } },
            },
          },
        }
        local session = { options = options, state = state, disposed = 0 }
        function session:inspect()
          return nvim.deepcopy(state)
        end
        function session:on(callback)
          self.listener = callback
          return function()
            self.listener = nil
          end
        end
        function session:set_config_option(id, value, callback)
          for _, option in ipairs(state.config_options) do
            if option.id == id then
              option.current_value = value
            end
          end
          later(callback, f.bad_confirmation and {} or state.config_options)
          return "config-1"
        end
        function session:prompt(_, callback)
          MiniTest.expect.equality(nvim.in_fast_event(), false)
          MiniTest.expect.equality(verifier, false)
          state.status = "prompting"
          f.sends = f.sends + 1
          f.turns = f.turns or {}
          f.turns[#f.turns + 1] = nvim.deepcopy(state.config_options)
          state.turn_identity = {
            agent = agent,
            provider = "openai",
            model = state.config_options[1].current_value,
            options = {
              model = state.config_options[1].current_value,
              reasoning_effort = state.config_options[2].current_value,
            },
          }
          f.prompt_done = function()
            later(function()
              if session.listener then
                session.listener({
                  type = "chunk",
                  data = { content = { type = "text", text = f.large_output and string.rep("x", 2001) or "Idle [A]" } },
                })
              end
              if state.status ~= "disposed" then
                state.status = "ready"
              end
              callback(f.worker_failure and nil or {}, f.worker_failure and "unsafe raw worker error" or nil)
            end)
          end
          if not f.hold_prompt then
            f.prompt_done()
          end
          return "turn-" .. f.sends
        end
        function session:cancel()
          self.cancelled = true
          return true
        end
        function session:dispose()
          self.disposed = self.disposed + 1
          state.status = "disposed"
          return true
        end
        f.sessions[#f.sessions + 1] = session
        later(done, session)
        return session
      end,
    },
    system = function(argv, options, done)
      MiniTest.expect.equality(nvim.in_fast_event(), false)
      f.calls[#f.calls + 1] = { argv = argv, request = options.stdin and nvim.json.decode(options.stdin) }
      local request = f.calls[#f.calls].request
      local reply
      if argv[2] == "launch-inputs" then
        reply = {
          schema = "louiselm.launch-inputs.staged/1",
          manifest_digest = DIGEST,
          source_snapshot_digest = DIGEST,
          source_base_digest = DIGEST,
          cache_base_digest = DIGEST,
          base_commit = f.selection.base_commit,
        }
      elseif argv[2] == "run" then
        if request.kind == "run" then
          reply = {
            kind = "run",
            receipt = {
              schema = "louiselm.broker.run-authorization/1",
              run_id = "trial-1",
              envelope_revision = 1,
              envelope_digest = DIGEST,
            },
          }
        else
          local launch = request.grant.request
          reply = {
            kind = "session",
            receipt = {
              schema = "louiselm.broker.child-authorization/1",
              run_id = launch.run_id,
              session_id = launch.session_id,
              authorization_id = launch.authorization_id,
              envelope_revision = launch.envelope_revision,
              envelope_digest = DIGEST,
              request_digest = "sha256:" .. nvim.fn.sha256(assert(require("louiselm.acp.launch").encode(launch))),
            },
          }
        end
      elseif argv[2] == "session" then
        reply = { state = "running", broker_head = { sequence = 1, digest = DIGEST } }
      elseif request.kind == "stage" then
        reply = { kind = "staged", input_digest = DIGEST }
      elseif request.kind == "park" then
        reply = { kind = "parked", head = { sequence = 2, digest = DIGEST } }
      elseif request.kind == "export" then
        reply = { kind = "exported", export_digest = DIGEST, evidence = { job = { job_digest = DIGEST } } }
      elseif request.kind == "run" then
        f.verifications = (f.verifications or 0) + 1
        f.last_verification = request.request
        if f.lost_verification_reply then
          later(done, { code = 1, stdout = "", stderr = "private payload" })
          return {}
        end
        reply = { kind = "executed" }
      elseif request.kind == "status" then
        reply = {
          kind = "status",
          status = {
            state = f.unknown_verification and "unknown" or "completed",
            detail = {
              execution = {
                request = f.last_verification,
                job = { job_digest = DIGEST },
                cleanup_proven = not f.unproven_cleanup,
                interrupted = false,
                steps = { { state = "completed", exit_code = f.failed_check and 1 or 0, timed_out = false } },
              },
            },
          },
        }
      end
      if f.refuse_authorization and argv[2] == "run" then
        later(done, { code = 1, stdout = "", stderr = "secret" })
      else
        later(done, { code = 0, stdout = nvim.json.encode(reply), stderr = "" })
      end
      return { kill = function() end }
    end,
  }
  return f
end

local function start(f)
  local controller = assert(require("louiselm.routing.trial").new(f.selection, f.options))
  f.controller = controller
  MiniTest.finally(function()
    local ok, err = controller:dispose()
    if not f.cleanup_failure then
      assert(ok, err)
    end
  end)
  assert(controller:start(function(result, err)
    MiniTest.expect.equality(nvim.in_fast_event(), false)
    f.completions = f.completions + 1
    f.result, f.error = result, err
  end))
  return controller
end

local function wait(f)
  assert(
    nvim.wait(3000, function()
      return f.completions > 0
    end, 5),
    "trial did not settle"
  )
end

T["explicit launch pins both arms, verifies them and leaves human judgment pending"] = function()
  local f = fixture()
  local controller = assert(require("louiselm.routing.trial").new(f.selection, f.options))
  MiniTest.expect.equality(#f.calls, 0)
  assert(controller:dispose())
  start(f)
  wait(f)
  MiniTest.expect.equality(f.error, nil)
  MiniTest.expect.equality(f.sends, 6)
  MiniTest.expect.equality(#f.sessions, 4)
  MiniTest.expect.equality(f.turns[4][1].current_value, "small")
  MiniTest.expect.equality(f.turns[4][2].current_value, "low")
  MiniTest.expect.equality(f.result.report.fixtures[1].checks[1].candidate, "pass")
  MiniTest.expect.equality(f.result.report.fixtures[2].checks[1].candidate, "pass")
  MiniTest.expect.equality(f.result.report.fixtures[3].human, nil)
  MiniTest.expect.equality(f.result.report.fixtures[3].human_review_required, true)
  MiniTest.expect.equality(f.result.observations.provider_requests, nvim.NIL)
  local serialized = nvim.json.encode(f.result)
  MiniTest.expect.equality(serialized:find("selected bulk input", 1, true), nil)
  MiniTest.expect.equality(serialized:find("Idle [A]", 1, true), nil)
  for _, call in ipairs(f.calls) do
    MiniTest.expect.equality(call.argv[1], "louiselm-control")
    if call.request and call.request.kind == "session" then
      MiniTest.expect.equality(call.request.grant.beads_mutations, nvim.NIL)
      MiniTest.expect.equality(call.request.grant.commands, nvim.NIL)
    end
  end
end

T["refused approval never starts an Agent"] = function()
  local f = fixture()
  f.refuse_authorization = true
  start(f)
  wait(f)
  MiniTest.expect.equality(#f.sessions, 0)
  MiniTest.expect.equality(f.sends, 0)
  MiniTest.expect.equality(f.error, "Control broker refused run")
end

T["both fixed verifiers initialize before any worker or prompt"] = function()
  local f = fixture()
  local create, ready, worker_readiness = f.options.session_api.create_session, 0, {}
  f.options.session_api.create_session = function(api, agent, options, done)
    if options.broker_session_id:find("worker", 1, true) then
      worker_readiness[#worker_readiness + 1] = ready
    end
    return create(api, agent, options, function(session, err)
      if options.broker_session_id:find("verifier", 1, true) then
        ready = ready + 1
      end
      done(session, err)
    end)
  end
  start(f)
  wait(f)
  MiniTest.expect.equality(worker_readiness, { 2, 2 })
  MiniTest.expect.equality(f.sends, 6)
end

T["unsupported second verifier starts no worker and exposes no Agent stderr"] = function()
  local f = fixture()
  local create = f.options.session_api.create_session
  f.options.session_api.create_session = function(api, agent, options, done)
    return create(api, agent, options, function(session, err)
      if options.broker_session_id == "trial-1-candidate-verifier" then
        done(nil, "synthetic-secret-from-Agent-stderr")
      else
        done(session, err)
      end
    end)
  end
  start(f)
  wait(f)
  MiniTest.expect.equality(f.sends, 0)
  MiniTest.expect.equality(#f.sessions, 2)
  MiniTest.expect.equality(f.result.observations.arms.candidate.outcome, "verifier_unsupported")
  MiniTest.expect.equality(nvim.json.encode(f.result):find("synthetic-secret", 1, true), nil)
  assert(f.error:find("unsupported", 1, true))
end

T["cancellation during preflight ignores late readiness without starting workers"] = function()
  local f = fixture()
  local create, ready = f.options.session_api.create_session, nil
  f.options.session_api.create_session = function(api, agent, options, done)
    return create(api, agent, options, function(session, err)
      ready = function()
        done(session, err)
      end
    end)
  end
  local controller = start(f)
  assert(nvim.wait(3000, function()
    return ready ~= nil
  end, 5))
  MiniTest.expect.equality(f.sessions[1].options.broker_session_id, "trial-1-baseline-verifier")
  assert(controller:dispose())
  assert(ready)()
  wait(f)
  nvim.wait(30, function()
    return false
  end)
  MiniTest.expect.equality(f.sends, 0)
  MiniTest.expect.equality(#f.sessions, 1)
  MiniTest.expect.equality(f.sessions[1].disposed, 1)
  MiniTest.expect.equality(f.completions, 1)
end

T["launch failures cannot disclose Agent stderr in trial artifacts"] = function()
  local f = fixture()
  local create = f.options.session_api.create_session
  f.options.session_api.create_session = function(api, agent, options, done)
    return create(api, agent, options, function()
      done(nil, "synthetic-secret-from-Agent-stderr")
    end)
  end
  start(f)
  wait(f)
  MiniTest.expect.equality(
    f.error,
    "unsupported: configured Agent cannot initialize the fixed verifier offline; inspect named Session receipts"
  )
  MiniTest.expect.equality(nvim.json.encode(f.result):find("synthetic-secret", 1, true), nil)
  MiniTest.expect.equality(f.completions, 1)
  MiniTest.expect.equality(f.sends, 0)
end

T["unconfirmed options never send and failed checks do not become quality passes"] = function()
  local f = fixture()
  f.bad_confirmation = true
  start(f)
  wait(f)
  MiniTest.expect.equality(f.sends, 3)
  MiniTest.expect.equality(f.result.report.fixtures[1].checks[1].candidate, "pending")
  f = fixture()
  f.failed_check = true
  f.lost_verification_reply = true
  start(f)
  wait(f)
  MiniTest.expect.equality(f.verifications, 2)
  MiniTest.expect.equality(f.result.report.fixtures[2].checks[1].candidate, "fail")
end

T["cancellation settles once and late Model callbacks start no new work"] = function()
  local f = fixture()
  f.hold_prompt = true
  local controller = start(f)
  assert(nvim.wait(3000, function()
    return f.prompt_done ~= nil
  end, 5))
  assert(controller:dispose())
  wait(f)
  f.prompt_done()
  nvim.wait(30, function()
    return false
  end)
  MiniTest.expect.equality(f.sends, 1)
  MiniTest.expect.equality(f.completions, 1)
  MiniTest.expect.equality(f.sessions[3].cancelled, true)
end

T["disposal stops pending control processes and keeps cleanup failures explicit"] = function()
  local f = fixture()
  f.cleanup_failure = true
  local killed, reply = false, nil
  f.options.system = function(_, _, done)
    reply = done
    return {
      kill = function()
        killed = true
        error("fixture kill failure")
      end,
    }
  end
  local controller = start(f)
  MiniTest.expect.equality(controller:dispose(), false)
  wait(f)
  MiniTest.expect.equality(killed, true)
  MiniTest.expect.equality(controller:dispose(), false)
  assert(reply)({ code = 0, stdout = "{}" })
  nvim.wait(30, function()
    return false
  end)
  MiniTest.expect.equality(f.completions, 1)
  MiniTest.expect.equality(#f.sessions, 0)
end

T["output cap and worker failure preserve incomplete outcomes without replay"] = function()
  for _, mutate in ipairs({
    function(f)
      f.large_output = true
    end,
    function(f)
      f.worker_failure = true
    end,
    function(f)
      f.unknown_verification = true
    end,
    function(f)
      f.unproven_cleanup = true
    end,
  }) do
    local f = fixture()
    mutate(f)
    start(f)
    wait(f)
    MiniTest.expect.equality(f.result.report.fixtures[2].checks[1].candidate, "pending")
    MiniTest.expect.equality(nvim.json.encode(f.result):find("unsafe raw worker error", 1, true), nil)
    if f.large_output or f.worker_failure then
      MiniTest.expect.equality(f.sends <= 2, true)
    end
  end
end

T["unsupported Providers, widened budgets and reused identities refuse before effects"] = function()
  for _, mutate in ipairs({
    function(s)
      s.manifest.routes.candidate.provider = "other"
    end,
    function(s)
      s.envelope.provider_requests.max_run_requests = 13
    end,
    function(s)
      s.envelope.max_sessions = 5
    end,
    function(s)
      s.input_manifest.source_snapshot_digest = "sha256:" .. string.rep("b", 64)
    end,
  }) do
    local f = fixture()
    mutate(f.selection)
    local controller, err = require("louiselm.routing.trial").new(f.selection, f.options)
    MiniTest.expect.equality(controller, nil)
    MiniTest.expect.equality(type(err), "string")
    MiniTest.expect.equality(#f.calls, 0)
  end
end

return T
