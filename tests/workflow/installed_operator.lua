-- Private-mount VM composition: real operator caller, capture CLI and broker.
-- The ordinary case uses a measured ACP fixture. Stock cases require the real
-- pinned runtime; only the reviewed verification plan and local TLS peer are fixtures.
---@diagnostic disable-next-line: undefined-global -- Neovim standalone fixture.
local nvim = vim
nvim.opt.rtp:prepend(nvim.fn.getcwd())
assert(nvim.fn.exepath("louiselm-capture") == "/usr/local/lib/louiselm-tools/louiselm-capture")
local selection = nvim.json.decode(table.concat(nvim.fn.readfile(nvim.env.LOUISELM_BEADS_FIXTURE), "\n"))
assert(nvim.deep_equal(selection.envelope.bead_scope.issue_ids, { "operator-1", "operator-2", "operator-3" }))
assert(nvim.deep_equal(
  nvim.tbl_map(function(bead)
    return bead.id
  end, selection.beads),
  { "operator-3", "operator-1", "operator-2" }
))
local completed, failure = false, nil
---@type louiselm.workflow.BeadRunSummary?
local summary
local approvals, heads = 0, { selection.worktree.head }
local stock = nvim.env.LOUISELM_TEST_STOCK_OPERATOR == "1"
local startup_only = nvim.env.LOUISELM_TEST_STOCK_STARTUP_ONLY == "1"
-- Exercise the shipped first-Run entrypoint with the same valid selection.
-- A fixture pass is never a rehearsal attestation for this boot/release.
local private_selection = nvim.env.XDG_STATE_HOME .. "/selection.json"
nvim.fn.writefile({ nvim.json.encode(selection) }, private_selection)
assert(nvim.fn.setfperm(private_selection, "rw-------") == 1)
nvim.g.louiselm_bead_run_selection = private_selection
local opened, refusal = pcall(nvim.api.nvim_exec2, "luafile scripts/bead-run.lua", { output = true })
assert(opened, refusal)
local check_copies = nvim.fn.glob(selection.snapshot_parent .. "/rehearsal-check-*", false, true)
assert(#check_copies == 1, "readiness must pin the controller selection in one private copy")
local check_metadata = assert(nvim.uv.fs_lstat(check_copies[1]))
assert(check_metadata.type == "file" and check_metadata.mode % 512 == 384 and check_metadata.uid == nvim.uv.getuid())
assert(nvim.deep_equal(nvim.json.decode(table.concat(nvim.fn.readfile(check_copies[1]), "\n")), selection))
assert(
  nvim.wait(120000, function()
    return nvim.api
      .nvim_exec2("messages", { output = true }).output
      :find("Paid Run requires a passing rehearsal", 1, true) ~= nil
  end, 20),
  "paid entrypoint did not refuse dispatch"
)
assert(nvim.uv.fs_lstat(check_copies[1]) == nil, "readiness selection copy leaked after completion")
assert(#require("louiselm.session").exit_verdict() == 0, "paid entrypoint started a Session")
if startup_only then
  local argv =
    { "louiselm-control", "run", "rehearse", "--selection", private_selection, "--plugin", nvim.fn.getcwd(), "--json" }
  local result = nvim.system(argv, { text = true }):wait(240000)
  local fault = nvim.env.LOUISELM_TEST_REHEARSAL_FAILURE
  if fault ~= nil and fault ~= "" then
    assert(result.code ~= 0, "injected failure passed rehearsal")
    local report = nvim.json.decode(result.stdout)
    if fault == "replica" then
      assert(
        report.status == "refused" and report.phase == "tracker_prepare" and report.code == "unavailable",
        result.stdout
      )
    else
      assert(report.status == "failed" and #report.workers == 1, result.stdout)
      local worker = report.workers[1]
      assert(worker.terminal_head and not worker.recovery_required and worker.provider_attempts == 0, result.stdout)
      assert(
        worker.observation.failure.phase == "workspace_prepare" and worker.observation.failure.code == "unavailable",
        result.stdout
      )
      assert(not worker.observation.started and not worker.observation.attached)
    end
    nvim.fn.writefile({ nvim.json.encode(report) }, nvim.env.XDG_STATE_HOME .. "/rehearsal.json")
    argv[3] = "rehearsal-check"
    assert(nvim.system(argv, { text = true }):wait(120000).code ~= 0, "failed rehearsal enabled paid dispatch")
    print("REHEARSAL_CHARACTERIZATION " .. fault .. " refused paid dispatch without a Provider request")
    return
  end
  assert(result.code == 0, "real rehearsal failed: " .. result.stdout .. result.stderr)
  local report = nvim.json.decode(result.stdout)
  assert(report.status == "passed" and #report.workers == #selection.beads)
  for _, worker in ipairs(report.workers) do
    assert(worker.observation.started and worker.observation.attached)
    assert(worker.terminal_head and not worker.recovery_required and worker.provider_attempts == 0)
  end
  nvim.fn.writefile({ nvim.json.encode(report) }, nvim.env.XDG_STATE_HOME .. "/rehearsal.json")
  argv[3] = "rehearsal-check"
  assert(nvim.system(argv, { text = true }):wait(120000).code == 0, "same-input rehearsal check failed")
  local changed = nvim.deepcopy(selection)
  changed.beads[1].prompt = changed.beads[1].prompt .. " changed"
  nvim.fn.writefile({ nvim.json.encode(changed) }, private_selection)
  assert(nvim.system(argv, { text = true }):wait(120000).code ~= 0, "changed selection reused a rehearsal")
  nvim.fn.writefile({ nvim.json.encode(selection) }, private_selection)
  print(
    "REHEARSAL passed: three stock workers, real capture, verified terminal chains, zero Provider attempts; changed selection refused"
  )
end
local attachment
local started = nvim.uv.hrtime()
local function progress(phase)
  io.stderr:write(
    "INSTALLED_OPERATOR phase=" .. phase .. " elapsed_ms=" .. math.floor((nvim.uv.hrtime() - started) / 1000000) .. "\n"
  )
end
local controller
controller = assert(require("louiselm.workflow.operator").new(selection, {
  on_promotion = function(preview, decide)
    approvals = approvals + 1
    progress("promotion-" .. approvals)
    assert(preview.selection.expected_head == heads[approvals])
    assert(nvim.deep_equal(preview.changes.added, { "accepted-" .. approvals .. ".txt" }))
    assert(#preview.changes.modified == 0 and #preview.changes.deleted == 0)
    decide(true)
    return nil
  end,
  system = function(argv, options, done)
    assert(nvim.fn.executable(argv[1]) == 1, "missing installed operator executable: " .. argv[1])
    local operation = table.concat({ argv[1], argv[2] or "", argv[3] or "" }, "/")
    progress("start-" .. operation)
    return nvim.system(argv, options, function(result)
      progress("end-" .. operation .. "-code-" .. result.code)
      if argv[1] == "louiselm-capture" and argv[3] == "run" and argv[4] == "attach" and result.code == 0 then
        attachment = nvim.deepcopy(argv)
        if startup_only then
          -- Hold the completed attach callback before it can send the first
          -- prompt. Everything through the production daemon and stock ACP
          -- initialization is real; this case deliberately ends at that cut.
          nvim.schedule(function()
            local inspected = nvim
              .system({
                "louiselm-control",
                "session",
                "inspect",
                selection.envelope.run_id .. "-worker-1",
                "--json",
              }, { text = true })
              :wait()
            assert(inspected.code == 0, "stock worker inspection failed")
            local state = nvim.json.decode(inspected.stdout)
            assert(state.state == "running" and state.broker_head.sequence == 1)
            assert(controller:dispose())
            completed = true
          end)
          return
        end
      end
      if argv[1] == "louiselm-control" and argv[2] == "promotion" and argv[3] == "commit" and result.code == 0 then
        heads[#heads + 1] = nvim.json.decode(result.stdout).commit
      end
      done(result)
    end)
  end,
}))
local on_worker = controller.options.on_worker
controller.options.on_worker = function(result, continue)
  progress("worker-result")
  on_worker(result, continue)
end
assert(controller:start(function(ok, err, result)
  progress(ok and "complete" or "failed")
  failure, summary, completed = not ok and err or nil, result, true
end))
assert(
  nvim.wait(stock and 270000 or nvim.env.LOUISELM_TEST_SOURCE_SIZED == "1" and 180000 or 90000, function()
    return completed
  end, 10),
  "operator Run timed out"
)
assert(controller:dispose())
if stock then
  nvim.fn.writefile({ nvim.json.encode(heads) }, nvim.env.XDG_STATE_HOME .. "/stock-heads.json")
end
if startup_only then
  assert(stock and attachment and approvals == 0 and #heads == 1)
  assert(failure == "Bead Run disposed", "probe must end by its deliberate disposal, not a startup failure")
  print("installed stock operator: production daemon, Running receipt and real capture attachment; no prompt")
  return
end
assert(not failure, failure)
assert(summary and #summary.accepted == 3 and #summary.failed == 0)
assert(approvals == 3 and #heads == 4, "expected three approvals and commits: " .. summary.text)
assert(summary.branch == "run/" .. selection.envelope.run_id)
for index, bead in ipairs(selection.beads) do
  assert(summary.accepted[index] == bead.id, "execution must preserve the reviewed order")
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
local attach = stock and assert(attachment, "stock Run never attached its real ACP Session")
  or {
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
print(
  "installed operator: reviewed nonalphabetic order, three accepted commits, current-HEAD snapshots and real capture ledger"
)
