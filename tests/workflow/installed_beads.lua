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
print("installed verification case " .. index)
local completed, failure, observed = false, nil, false
local controller = assert(Workflow.new_bead_executor({
  agent_id = "agent",
  envelope = case.envelope,
  bead_ids = { case.bead_id },
  system = function(argv, options, done)
    if argv[1] == "louiselm-capture" then
      nvim.schedule(function()
        done({ code = 0, stdout = '{"token":"fixture-capture-only"}', stderr = "" })
      end)
      return nil
    end
    return nvim.system(argv, options, done)
  end,
  prepare = function(id, done)
    assert(id == case.bead_id)
    nvim.schedule(function()
      done({
        grant = case.grant,
        prompt = index == 3 and "unapprovable" or "Complete this offline fixture turn.",
        verification = {
          snapshot = case.snapshot,
          snapshot_digest = case.snapshot_digest,
          plan = case.plan,
          plan_digest = case.plan_digest,
          verifier_grant = case.verifier_grant,
        },
      })
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
      assert(step.exit_code == (index == 1 and 0 or 7))
      assert(result.verification_passed == (index == 1))
      continue(true)
    end
    observed = true
    assert(result.session:dispose())
  end,
}))
assert(controller:start(function(ok, err, summary)
  if not ok then
    failure = err or "unexpected Run outcome"
  end
  if summary then
    assert(#summary.accepted == (index == 1 and 1 or 0))
    assert(#summary.failed == (index == 1 and 0 or 1))
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
assert(observed, "installed Lua controller did not report worker result for case " .. index)
print("installed Lua Bead executor case " .. index .. " complete")
