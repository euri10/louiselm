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
local function now_ms()
  local seconds, micros = nvim.uv.gettimeofday()
  return seconds * 1000 + math.floor(micros / 1000)
end
print("installed verification case " .. index)
local completed, failure, observed = false, nil, false
local controller = assert(Workflow.new_bead_executor({
  agent_id = "agent",
  envelope = case.envelope,
  bead_ids = delayed_promotion and { case.bead_id, "promotion-successor" }
    or index >= 6 and { case.bead_id, "held-successor" }
    or { case.bead_id },
  worktree = index == 4 and { path = case.worktree, journal_parent = case.journal_parent, head = case.base_commit }
    or nil,
  on_promotion = index == 4 and function(preview, decide)
    assert(preview.schema == "louiselm.run-promotion-preview/1")
    assert(preview.selection.expected_head == case.base_commit)
    assert(nvim.deep_equal(preview.changes.added, { "accepted.txt" }))
    if delayed_promotion then
      -- Same event order as Run e0500a6e (pynio): verification, preview,
      -- human delay, Accept. Use a shorter explicit deadline, never alter the
      -- returned selection or its approval digest to simulate expiration.
      assert(preview.selection.expires_at_ms > now_ms(), "preview must arrive before expiry")
      nvim.defer_fn(function()
        decide(true)
      end, preview.selection.expires_at_ms - now_ms() + 50)
    else
      decide(true)
    end
  end or nil,
  system = function(argv, options, done)
    if delayed_promotion and argv[2] == "promotion" and argv[3] == "preview" then
      local request = nvim.json.decode(options.stdin)
      request.selection.expires_at_ms = now_ms() + 30000
      options.stdin = nvim.json.encode(request)
    end
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
          assert(duplicate.code == 6, "immutable staging must refuse duplicate publication")
          assert(nvim.json.decode(duplicate.stderr).error == "status_unavailable")
          prepared()
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
  if delayed_promotion then
    assert(not ok and controller.status == "failed")
    assert(err == "Run promotion commit refused: promotion preview expired; no changes applied", err)
    assert(#summary.accepted == 0 and #summary.failed == 0 and next(summary.commits) == nil)
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
assert(index >= 6 or observed, "installed Lua controller did not report worker result for case " .. index)
print("installed Lua Bead executor case " .. index .. " complete")
