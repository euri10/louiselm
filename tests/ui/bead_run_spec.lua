local MiniTest = require("mini.test")
---@diagnostic disable-next-line: undefined-global -- Neovim runtime API.
local nvim = vim
local T = MiniTest.new_set()

local function fixture()
  local f = { disposed = 0 }
  local controller = {
    status = "new",
    options = { envelope = { run_id = "run-1" } },
    summary = { accepted = {}, failed = {} },
  }
  function controller:start(done)
    self.status = "active"
    f.done = done
    return true
  end
  function controller:dispose()
    f.disposed = f.disposed + 1
    self.status = "disposed"
    return true
  end
  -- Only the public controller operations used by the UI are doubled.
  ---@cast controller louiselm.workflow.BeadExecutor
  f.controller = controller
  f.buffer = assert(require("louiselm.ui.bead_run").open(controller))
  MiniTest.finally(function()
    if nvim.api.nvim_buf_is_valid(f.buffer) then
      nvim.api.nvim_buf_delete(f.buffer, { force = true })
    end
  end)
  function f:text()
    return table.concat(nvim.api.nvim_buf_get_lines(self.buffer, 0, -1, false), "\n")
  end
  return f
end

T["Run view shows current Bead and phase without stealing review focus"] = function()
  local f = fixture()
  local other = nvim.api.nvim_create_buf(false, true)
  MiniTest.finally(function()
    nvim.api.nvim_buf_delete(other, { force = true })
  end)
  nvim.api.nvim_set_current_buf(other)
  for _, phase in ipairs({ "preparing", "working", "verifying", "awaiting_promotion", "promoting" }) do
    f.controller.options.on_progress({ run_id = "run-1", bead_id = "b-2", index = 2, total = 3, phase = phase })
    assert(f:text():find("b-2", 1, true), f:text())
    assert(f:text():find("2/3", 1, true), f:text())
    assert(not f:text():find("Starting", 1, true), f:text())
    MiniTest.expect.equality(nvim.api.nvim_get_current_buf(), other)
  end
  assert(f:text():find("Promoting", 1, true), f:text())
end

T["completion distinguishes task acceptance from traversal"] = MiniTest.new_set({
  parametrize = {
    { 0, 0, 3, "Run ended with failures" },
    { 1, 1, 1, "Run ended with failures" },
    { 0, 3, 0, "Run ended with rejections" },
    { 3, 0, 0, "Run complete" },
  },
})
T["completion distinguishes task acceptance from traversal"]["summary"] = function(accepted, rejected, failed, headline)
  local f = fixture()
  local summary = { accepted = {}, failed = {}, text = "Run run-1\nAccepted: (test handoff)" }
  for i = 1, accepted do
    summary.accepted[i] = "b-" .. i
  end
  for _ = 1, rejected do
    summary.failed[#summary.failed + 1] = { outcome = "rejected" }
  end
  for _ = 1, failed do
    summary.failed[#summary.failed + 1] = { outcome = "verification_failed" }
  end
  f.controller.status = "completed"
  f.done(true, nil, summary)
  assert(f:text():find(headline, 1, true), f:text())
  local counts = string.format("Accepted: %d | Rejected: %d | Failed: %d", accepted, rejected, failed)
  assert(f:text():find(counts, 1, true), f:text())
  MiniTest.expect.equality(f.disposed, 1)
  local final = f:text()
  f.controller.options.on_progress({ run_id = "run-1", bead_id = "b-1", phase = "working", index = 1, total = 3 })
  MiniTest.expect.equality(f:text(), final)
end

T["closing active Run view disposes its controller once"] = function()
  local f = fixture()
  nvim.api.nvim_buf_delete(f.buffer, { force = true })
  MiniTest.expect.equality(f.disposed, 1)
end

T["budget hold keeps Sessions until explicit stop"] = function()
  local f = fixture()
  f.controller.status = "held"
  f.done(false, "budget exhausted", { accepted = {}, failed = {}, text = "Provider budget held" })
  MiniTest.expect.equality(f.disposed, 0)
  assert(f:text():find("Run held", 1, true), f:text())
  nvim.api.nvim_buf_delete(f.buffer, { force = true })
  MiniTest.expect.equality(f.disposed, 1)
end

T["q stops a held Run and preserves its multiline handoff"] = function()
  local f = fixture()
  f.controller.status = "held"
  f.controller.summary.text = "Run run-1\nProvider budget held"
  f.done(false, "budget exhausted", f.controller.summary)
  nvim.api.nvim_feedkeys("q", "mx!", false)
  MiniTest.expect.equality(f.disposed, 1)
  assert(f:text():find("Run stopped by operator", 1, true), f:text())
  assert(f:text():find("Provider budget held", 1, true), f:text())
end

return T
