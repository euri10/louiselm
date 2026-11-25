local MiniTest = require("mini.test")
local Rehearsal = require("louiselm.workflow.rehearsal")

local T = MiniTest.new_set()

T["cleanup failure refuses dispatch and remains reportable after disposal"] = function()
  local decision, err = Rehearsal.after_cleanup(false, "permission denied", false)
  MiniTest.expect.equality(decision, false)
  assert(err)
  assert(err:find("cleanup failed", 1, true), err)
  local disposed, disposed_error = Rehearsal.after_cleanup(false, "permission denied", true)
  MiniTest.expect.equality(disposed, nil)
  assert(disposed_error)
  assert(disposed_error:find("cleanup failed", 1, true), disposed_error)
end

T["only a cleaned current controller continues rehearsal validation"] = function()
  MiniTest.expect.equality(Rehearsal.after_cleanup(true, nil, false), true)
  MiniTest.expect.equality(Rehearsal.after_cleanup(true, nil, true), nil)
end

return T
