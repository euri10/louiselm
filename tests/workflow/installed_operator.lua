-- Private-mount VM composition: real operator caller, capture CLI and broker.
-- Only the measured ACP peer and reviewed offline verification plan are fixtures.
---@diagnostic disable-next-line: undefined-global -- Neovim standalone fixture.
local nvim = vim
nvim.opt.rtp:prepend(nvim.fn.getcwd())
local selection = nvim.json.decode(table.concat(nvim.fn.readfile(nvim.env.LOUISELM_BEADS_FIXTURE), "\n"))
local completed, failure = false, nil
---@type louiselm.workflow.BeadRunSummary?
local summary
local approvals, heads = 0, { selection.worktree.head }
local controller = assert(require("louiselm.workflow.operator").new(selection, {
  on_promotion = function(preview, decide)
    approvals = approvals + 1
    assert(preview.selection.expected_head == heads[approvals])
    assert(nvim.deep_equal(preview.changes.added, { "accepted-" .. approvals .. ".txt" }))
    assert(#preview.changes.modified == 0 and #preview.changes.deleted == 0)
    decide(true)
  end,
  system = function(argv, options, done)
    assert(nvim.fn.executable(argv[1]) == 1, "missing installed operator executable: " .. argv[1])
    return nvim.system(argv, options, function(result)
      if argv[1] == "louiselm-control" and argv[2] == "promotion" and argv[3] == "commit" and result.code == 0 then
        heads[#heads + 1] = nvim.json.decode(result.stdout).commit
      end
      done(result)
    end)
  end,
}))
local on_worker = controller.options.on_worker
controller.options.on_worker = function(result, continue)
  if result.error then
    print("offline worker startup: " .. result.error)
  end
  on_worker(result, continue)
end
assert(controller:start(function(ok, err, result)
  failure, summary, completed = not ok and err or nil, result, true
end))
assert(
  nvim.wait(90000, function()
    return completed
  end, 10),
  "operator Run timed out"
)
assert(controller:dispose())
assert(not failure, failure)
assert(summary and #summary.accepted == 3 and #summary.failed == 0)
assert(approvals == 3 and #heads == 4, "expected three approvals and commits: " .. summary.text)
assert(summary.branch == "run/" .. selection.envelope.run_id)
for index, bead in ipairs(selection.beads) do
  assert(summary.commits[bead.id] == heads[index + 1])
  local snapshot = selection.snapshot_parent .. "/" .. selection.envelope.run_id .. "-" .. index
  local metadata = nvim.json.decode(table.concat(nvim.fn.readfile(snapshot .. "/snapshot.json"), "\n"))
  assert(metadata.base_commit == heads[index], "next snapshot must use the last accepted commit")
end
-- `run list` intentionally reports only cold-Parked Runs, not this active ledger.
-- Use public CLI refusals and its documented identical-attachment readback;
-- never open capture's capability files or expose its admission token.
local duplicate = nvim
  .system({
    "louiselm-capture",
    "--require-interface=1",
    "run",
    "admit",
    "--id",
    selection.envelope.run_id,
    "--generated-work-max",
    "3",
    "--park-ttl-ms",
    "3600000",
  }, { text = true })
  :wait()
assert(
  duplicate.code ~= 0 and duplicate.stderr:find("Run UUID conflicts with existing record", 1, true),
  "real capture must retain the exact admitted Run"
)
local attach = {
  "louiselm-capture",
  "--require-interface=1",
  "run",
  "attach",
  "--id",
  selection.envelope.run_id,
  "--session-id",
  selection.manifest.agent.id .. "/fixture-acp",
  "--agent",
  selection.manifest.agent.id,
  "--acp-session-id",
  "fixture-acp",
  "--cwd",
  "/var/lib/louiselm/sessions/" .. selection.envelope.run_id .. "-worker-1/workspace",
  "--load-session",
  "false",
}
local changed = nvim.deepcopy(attach)
changed[8] = "different-anchor"
local refused = nvim.system(changed, { text = true }):wait()
assert(
  refused.code ~= 0 and refused.stderr:find("only an admitted Run can attach a Session", 1, true),
  "a conflicting anchor must not replace the initialized Session"
)
local retained = nvim.system(attach, { text = true }):wait()
assert(
  retained.code == 0 and nvim.json.decode(retained.stdout).state == "active",
  "real capture must retain the exact initialized anchor"
)
print("installed operator: three accepted commits, current-HEAD snapshots and real capture ledger")
