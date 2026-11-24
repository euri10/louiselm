-- Invoked only by the private-mount disposable-VM Rust fixture. Authorization,
-- launching, ACP permission replies and supervisor cleanup are production paths.
-- Capture admission/attachment is doubled here; its real CLI has separate gates.
---@diagnostic disable-next-line: undefined-global -- Neovim standalone fixture.
local nvim = vim
nvim.opt.rtp:prepend(nvim.fn.getcwd())
local Workflow = require("louiselm.workflow")
local input = nvim.json.decode(table.concat(nvim.fn.readfile(nvim.env.LOUISELM_BEADS_FIXTURE), "\n"))
local index = assert(tonumber(nvim.env.LOUISELM_BEADS_CASE), "explicit installed case")
local case = assert(input.cases[index], "selected installed case")
local delayed_promotion = nvim.env.LOUISELM_TEST_PROMOTION_DELAY == "1"
local startup_failure = nvim.env.LOUISELM_TEST_STARTUP_FAILURE == "1"
if startup_failure then
  case.envelope.bead_scope.issue_ids = { case.bead_id, "startup-successor" }
  table.sort(case.envelope.bead_scope.issue_ids)
  case.envelope.bead_scope.max_mutations = 2
  case.envelope.max_sessions = 4
end
local function now_ms()
  local seconds, micros = nvim.uv.gettimeofday()
  return seconds * 1000 + math.floor(micros / 1000)
end
if index == 1 then
  -- Live Run fddd29b9 (1uqo1) reached export with almost three hours left.
  -- Keep the real broker/supervisor operation limits in this offline regression.
  case.envelope.expires_at_ms = now_ms() + 10800000
end
print("installed verification case " .. index)
local completed, failure, observed = false, nil, false
local reviewed
local controller = assert(Workflow.new_bead_executor({
  agent_id = "agent",
  envelope = case.envelope,
  bead_ids = startup_failure and { case.bead_id, "startup-successor" } or delayed_promotion and {
    case.bead_id,
    "promotion-successor",
  } or index >= 6 and { case.bead_id, "held-successor" } or { case.bead_id },
  worktree = index == 4 and { path = case.worktree, journal_parent = case.journal_parent, head = case.base_commit }
    or nil,
  on_promotion = index == 4
      and function(preview, decide)
        assert(preview.schema == "louiselm.run-promotion-preview/1")
        assert(preview.selection.expected_head == case.base_commit)
        assert(nvim.deep_equal(preview.changes.added, { "accepted.txt" }))
        assert(#require("louiselm.session").exit_verdict() == 0, "saved workers must already be released")
        nvim.fn.writefile({}, case.saved_ready)
        assert(
          nvim.wait(30000, function()
            return nvim.uv.fs_stat(case.saved_cleaned) ~= nil
          end, 10),
          "expired Session cleanup did not finish"
        )
        local inspected = nvim
          .system({ "louiselm-control", "promotion", "inspect", "--json" }, {
            stdin = nvim.json.encode({ result_id = preview.selection.request_id }),
            text = true,
          })
          :wait()
        assert(inspected.code == 0, "saved result inspection must work after producer Disposal")
        local saved = nvim.json.decode(inspected.stdout).result
        assert(saved.request.request_id == preview.selection.request_id and saved.discarded == false)
        assert(saved.verification.execution.cleanup_proven == true)
        assert(preview.selection.expires_at_ms == nil, "saved review must have no deadline")
        local function promotion(verb, payload)
          return nvim
            .system({ "louiselm-control", "promotion", verb, "--json" }, {
              stdin = nvim.json.encode(payload),
              text = true,
            })
            :wait()
        end
        local rules = nvim.fn.readfile(case.plan, "b")
        assert(nvim.uv.fs_chmod(case.plan, 420))
        local changed = nvim.deepcopy(rules)
        changed[#changed] = changed[#changed] .. " "
        nvim.fn.writefile(changed, case.plan, "b")
        local refused =
          promotion("commit", { result_id = preview.selection.request_id, approval_digest = preview.approval_digest })
        assert(
          refused.code == 1
            and nvim.json.decode(refused.stderr).message == "verification rules changed; refresh required"
        )
        nvim.fn.writefile(rules, case.plan, "b")
        assert(nvim.uv.fs_chmod(case.plan, 292))
        local reopened = promotion("preview", { result_id = preview.selection.request_id })
        assert(reopened.code == 0, reopened.stderr)
        assert(
          nvim.json.decode(reopened.stdout).approval_digest == preview.approval_digest,
          "fresh transport must preserve exact review identity"
        )
        reviewed =
          { id = preview.selection.request_id, approval = preview.approval_digest, verification = saved.verification }
        if delayed_promotion then
          nvim.defer_fn(function()
            assert(now_ms() >= case.envelope.expires_at_ms)
            decide(true)
          end, math.max(1, case.envelope.expires_at_ms - now_ms() + 50))
        else
          decide(true)
        end
      end
    or nil,
  system = function(argv, options, done)
    if argv[1] == "louiselm-capture" then
      nvim.schedule(function()
        done({ code = 0, stdout = '{"token":"fixture-capture-only"}', stderr = "" })
      end)
      return nil
    end
    if index >= 6 and argv[2] == "run" then
      local request = nvim.json.decode(options.stdin)
      local inject = index == 6 and request.kind == "provider_status"
        or index == 7 and request.kind == "session" and request.grant.role == "fixed_verifier"
      if inject then
        nvim.fn.writefile({}, case.hold_ready)
        local deadline = nvim.uv.hrtime() + 10000000000
        local function published()
          if nvim.uv.fs_stat(case.hold_published) then
            nvim.system(argv, options, done)
          elseif nvim.uv.hrtime() >= deadline then
            done({ code = 1, stdout = "", stderr = "fixture hold publication timed out" })
          else
            nvim.defer_fn(published, 10)
          end
        end
        published()
        return nil
      end
    end
    return nvim.system(argv, options, done)
  end,
  prepare = function(id, done, expected_head)
    assert(id == case.bead_id)
    if index == 4 then
      assert(expected_head == case.base_commit)
    end
    local function prepared()
      done({
        grant = case.grant,
        base_commit = index == 4 and case.base_commit or nil,
        prompt = index == 3 and "unapprovable"
          or index == 4 and "promote-fixture"
          or "Complete this offline fixture turn.",
        verification = {
          snapshot = case.snapshot,
          snapshot_digest = case.snapshot_digest,
          plan = case.plan,
          plan_digest = case.plan_digest,
          verifier_grant = case.verifier_grant,
        },
      })
    end
    if index ~= 4 then
      nvim.schedule(prepared)
      return true
    end
    local request = nvim.deepcopy(case.stage_inputs)
    request.expected_base_commit = (case.base_commit:sub(1, 1) == "a" and "b" or "a") .. case.base_commit:sub(2)
    local function stage(input, callback)
      nvim.system({ "louiselm-control", "launch-inputs", "stage", "--json" }, {
        stdin = nvim.json.encode(input),
        text = true,
        cwd = "/",
      }, function(result)
        nvim.schedule(function()
          callback(result)
        end)
      end)
    end
    stage(request, function(stale)
      assert(stale.code == 2, "stale HEAD must refuse before publication")
      assert(nvim.json.decode(stale.stderr).error == "invalid_request")
      stage(case.stage_inputs, function(result)
        if result.code ~= 0 then
          done(nil, "installed broker source staging failed")
          return
        end
        local binding = nvim.json.decode(result.stdout)
        assert(binding.schema == "louiselm.launch-inputs.staged/1")
        assert(binding.manifest_digest == case.grant.request.session_input_manifest_id)
        assert(binding.source_snapshot_digest == case.snapshot_digest)
        assert(binding.source_base_digest == case.stage_inputs.manifest.source_base_digest)
        assert(binding.cache_base_digest == case.stage_inputs.manifest.cache_base_digest)
        assert(binding.base_commit == case.base_commit)
        stage(case.stage_inputs, function(duplicate)
          assert(duplicate.code == 0, "identical immutable inputs must be reusable after remeasurement")
          assert(nvim.deep_equal(nvim.json.decode(duplicate.stdout), binding))
          local changed = nvim.deepcopy(case.stage_inputs)
          changed.cache = changed.snapshot
          stage(changed, function(refused)
            assert(refused.code == 6, "changed incoming cache must refuse reuse")
            assert(nvim.json.decode(refused.stderr).error == "status_unavailable")
            stage(case.stage_inputs, function(retained)
              assert(retained.code == 0 and nvim.deep_equal(nvim.json.decode(retained.stdout), binding))
              prepared()
            end)
          end)
        end)
      end)
    end)
    return true
  end,
  on_worker = function(result, continue)
    assert(result.bead_id == case.bead_id)
    assert(result.envelope_digest ~= result.request_digest)
    if index == 3 then
      assert(
        result.error and result.error:find("exited", 1, true),
        "unapprovable request must fail without a human decision"
      )
      assert(result.verification == nil)
      continue(false, result.error)
    else
      if result.error then
        failure = "case " .. index .. ": " .. result.error
        observed = true
        return continue(false, result.error)
      end
      assert(result.verification and result.verification.state == "completed")
      local step = result.verification.detail.execution.steps[1]
      assert(step.state == "completed")
      assert(step.exit_code == (index == 2 and 7 or 0))
      assert(result.verification_passed == (index ~= 2))
      continue(true)
    end
    observed = true
    if index ~= 4 then
      assert(result.session:dispose())
    end
  end,
}))
assert(controller:start(function(ok, err, summary)
  if startup_failure then
    assert(not ok and err:find("worker startup failed", 1, true), err)
    assert(summary.startup_failure.session_id == case.grant.request.session_id)
    assert(summary.startup_failure.phase == "workspace_prepare")
    assert(summary.startup_failure.code == "unavailable")
    assert(summary.text:find("workspace_prepare/unavailable", 1, true))
    assert(#controller.run.workers == 1, "no successor worker or verifier may launch")
    assert(#summary.accepted == 0 and #summary.failed == 0 and not observed)
    completed = true
    return
  end
  if delayed_promotion then
    assert(ok and controller.status == "completed", err)
    assert(#summary.accepted == 1 and #summary.failed == 0 and summary.commits[case.bead_id])
    assert(#controller.run.workers == 2, "expiry must not start a successor")
    completed = true
    return
  end
  if index >= 6 then
    assert(not ok and controller.status == "held", err)
    assert(controller.run.status == "parked")
    assert(summary.provider_hold.reason == "exhausted")
    assert(#controller.run.workers == 1, "no successor worker or verifier may launch")
    assert(#summary.accepted == 0 and #summary.failed == 0)
    assert(not observed, "held Run must not reach the ordinary worker-failure continuation")
    completed = true
    return
  end
  if not ok then
    failure = err or "unexpected Run outcome"
  end
  if summary then
    if
      #summary.accepted ~= ((index == 1 or index == 4) and 1 or 0)
      or #summary.failed ~= ((index == 1 or index == 4) and 0 or 1)
      or (index == 4 and not (summary.commits[case.bead_id] or ""):match("^[0-9a-f]+$"))
    then
      failure = failure or nvim.inspect({ err = err, summary = summary })
    end
  end
  completed = true
end))
local finished = nvim.wait(90000, function()
  return completed
end, 10)
local disposed, dispose_error = controller:dispose()
assert(disposed, dispose_error)
assert(finished, "installed Lua controller timed out")
assert(not failure, failure)
if reviewed then
  local repeated = nvim
    .system({ "louiselm-control", "promotion", "commit", "--json" }, {
      stdin = nvim.json.encode({ result_id = reviewed.id, approval_digest = reviewed.approval }),
      text = true,
    })
    :wait()
  assert(repeated.code == 0, repeated.stderr)
  assert(nvim.json.decode(repeated.stdout).commit == controller.summary.commits[case.bead_id])
  local inspection = nvim
    .system({ "louiselm-control", "promotion", "inspect", "--json" }, {
      stdin = nvim.json.encode({ result_id = reviewed.id }),
      text = true,
    })
    :wait()
  assert(inspection.code == 0, inspection.stderr)
  local saved = nvim.json.decode(inspection.stdout).result
  assert(nvim.deep_equal(saved.verification, reviewed.verification), "unchanged evidence must be reused, not rerun")
  assert(saved.accepted.commit == controller.summary.commits[case.bead_id] and saved.application.state == "Completed")
end
assert(
  startup_failure or index >= 6 or observed,
  "installed Lua controller did not report worker result for case " .. index
)
print("installed Lua Bead executor case " .. index .. " complete")
