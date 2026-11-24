-- Trusted disposable-VM fixture; never sourced by production or a worker.
---@diagnostic disable-next-line: undefined-global -- Neovim fixture runtime.
local nvim = vim

return function(case, original_preview, original, done)
  local id = original.request.request_id
  local function execute(argv, payload, cwd)
    return nvim
      .system(argv, { stdin = payload and nvim.json.encode(payload) or nil, text = true, cwd = cwd or "/" })
      :wait()
  end
  local function control(verb, payload)
    return execute({ "louiselm-control", "promotion", verb, "--json" }, payload)
  end
  local function inspect()
    local response = control("inspect", { result_id = id })
    assert(response.code == 0, response.stderr)
    return nvim.json.decode(response.stdout).result
  end
  local function git(arguments)
    local argv = {
      "/usr/bin/git",
      "-c",
      "user.name=refresh-fixture",
      "-c",
      "user.email=refresh@invalid",
      "-c",
      "commit.gpgsign=false",
    }
    nvim.list_extend(argv, arguments)
    local result = execute(argv, nil, case.worktree)
    assert(result.code == 0, result.stderr)
    return nvim.trim(result.stdout)
  end
  local function now_ms()
    local seconds, micros = nvim.uv.gettimeofday()
    return seconds * 1000 + math.floor(micros / 1000)
  end
  local function new_base(name)
    nvim.fn.writefile({ name }, case.worktree .. "/" .. name)
    git({ "add", "--", name })
    git({ "commit", "-qm", "independent fixture movement" })
    return git({ "rev-parse", "HEAD" })
  end
  local function prepare(number, passed)
    local snapshot = nvim.fs.dirname(case.worktree) .. "/refresh-snapshot-" .. number
    local head = git({ "rev-parse", "HEAD" })
    local prepared = execute({
      "louiselm-skills",
      "workspace",
      "prepare",
      "--repository",
      case.worktree,
      "--output",
      snapshot,
      "--robot-json",
    })
    assert(prepared.code == 0, prepared.stderr)
    local source = nvim.json.decode(prepared.stdout)
    assert(source.base_commit == head)
    assert(execute({ "/usr/bin/chmod", "-R", "a+rX", "--", snapshot }).code == 0)
    local plan = nvim.fs.dirname(case.worktree) .. "/refresh-plan-" .. number .. ".json"
    local bytes = nvim.json.encode({
      schema = "louiselm.workspace.verification-plan/1",
      commands = {
        { argv = passed and { "true" } or { "sh", "-c", "exit 7" }, cwd = ".", timeout_ms = 5000 },
      },
    }) .. string.rep(" ", number)
    nvim.fn.writefile({ bytes }, plan, "b")
    assert(nvim.uv.fs_chmod(plan, 292))
    local saved = inspect()
    local manifest = nvim.deepcopy(case.stage_inputs.manifest)
    manifest.source_snapshot_digest, manifest.source_base_digest = source.snapshot_digest, source.base_digest
    local envelope = nvim.deepcopy(case.envelope)
    envelope.run_id = string.format("33333333-3333-4333-8333-%012d", number)
    envelope.max_sessions, envelope.expires_at_ms = 1, now_ms() + 60000
    envelope.bead_scope, envelope.provider_requests, envelope.commands, envelope.skill_requests =
      nvim.NIL, nvim.NIL, nvim.NIL, nvim.NIL
    envelope.verification_plan_digest = "sha256:" .. nvim.fn.sha256(bytes)
    return {
      selection = {
        result_id = id,
        revision_digest = saved.request.verification_digest,
        input_id = "refresh-input-" .. number,
        snapshot = snapshot,
        snapshot_digest = source.snapshot_digest,
        expected_head = head,
        plan = plan,
        plan_digest = envelope.verification_plan_digest,
      },
      envelope = envelope,
      manifest = manifest,
      cache = case.stage_inputs.cache,
      session_id = "refresh-verifier-" .. number,
    }
  end
  local function refresh(options, callback)
    options.system = function(argv, parameters, complete)
      return nvim.system(argv, parameters, function(output)
        nvim.schedule(function()
          print("REFRESH_PHASE " .. argv[2] .. "/" .. (argv[3] or "") .. " exit=" .. output.code)
          complete(output)
        end)
      end)
    end
    local cancel, err = require("louiselm.workflow.saved_result").refresh(options, function(...)
      local ok, callback_error = pcall(callback, ...)
      if not ok then
        done(callback_error)
      end
    end)
    assert(cancel, err)
  end
  local first_head = new_base("concurrent-base.txt")
  local stale = control("commit", { result_id = id, approval_digest = original_preview.approval_digest })
  assert(stale.code ~= 0 and stale.stderr:find("refresh required", 1, true))
  assert(not nvim.uv.fs_stat(case.worktree .. "/accepted.txt"))
  local function after_expiry()
    assert(now_ms() >= case.envelope.expires_at_ms)
    refresh(prepare(1, false), function(failed, err)
      assert(not failed and err)
      local status = execute(
        { "louiselm-control", "verification", "--json" },
        { kind = "status", session_id = "refresh-verifier-1" }
      )
      assert(status.code == 0, "failed verifier status unavailable: " .. err)
      local observation = nvim.json.decode(status.stdout)
      assert(
        observation.kind == "status" and observation.status.state == "completed",
        "expected an actual failed-plan observation, got: " .. err
      )
      assert(observation.status.detail.execution.steps[1].exit_code == 7)
      assert(nvim.deep_equal(inspect(), original), "failed fresh verification must preserve original")
      assert(git({ "rev-parse", "HEAD" }) == first_head)
      refresh(prepare(2, true), function(saved, refresh_error)
        assert(saved, refresh_error)
        assert(saved.selection.expected_head == first_head)
        assert(saved.verification.producer.request.launch.run_id == case.envelope.run_id)
        assert(saved.verification.execution.request.launch.run_id == "33333333-3333-4333-8333-000000000002")
        assert(saved.verification.execution.job.plan_digest ~= original.verification.execution.job.plan_digest)
        assert(git({ "rev-parse", "HEAD" }) == first_head and not nvim.uv.fs_stat(case.worktree .. "/accepted.txt"))
        local preview = control("preview", { result_id = id })
        assert(preview.code == 0, preview.stderr)
        preview = nvim.json.decode(preview.stdout)
        assert(preview.approval_digest ~= original_preview.approval_digest)
        local refused = control("commit", { result_id = id, approval_digest = original_preview.approval_digest })
        assert(refused.code ~= 0, "old approval cannot select refreshed bytes")
        local second_head = new_base("movement-after-refresh.txt")
        refused = control("commit", { result_id = id, approval_digest = preview.approval_digest })
        assert(refused.code ~= 0, "concurrent destination movement must refuse acceptance")
        assert(git({ "rev-parse", "HEAD" }) == second_head)
        refresh(prepare(3, true), function(current, final_error)
          assert(current, final_error)
          local fresh = control("preview", { result_id = id })
          assert(fresh.code == 0, fresh.stderr)
          fresh = nvim.json.decode(fresh.stdout)
          local committed = control("commit", { result_id = id, approval_digest = fresh.approval_digest })
          assert(committed.code == 0, committed.stderr)
          local repeated = control("commit", { result_id = id, approval_digest = fresh.approval_digest })
          assert(
            repeated.code == 0
              and nvim.deep_equal(nvim.json.decode(repeated.stdout), nvim.json.decode(committed.stdout))
          )
          assert(inspect().accepted.commit == nvim.json.decode(committed.stdout).commit)
          assert(git({ "status", "--porcelain" }) == "")
          assert(nvim.fn.readfile(case.worktree .. "/concurrent-base.txt")[1] == "concurrent-base.txt")
          assert(nvim.fn.readfile(case.worktree .. "/movement-after-refresh.txt")[1] == "movement-after-refresh.txt")
          print(
            "installed saved refresh: expired original, failed verification, new rules, moved base, stale approval and concurrent movement passed"
          )
          done()
        end)
      end)
    end)
  end
  nvim.defer_fn(after_expiry, math.max(1, case.envelope.expires_at_ms - now_ms() + 50))
end
