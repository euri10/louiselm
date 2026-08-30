local MiniTest = require("mini.test")
local Workflow = require("louiselm.workflow")
local Reference = require("louiselm.dev.reference_workflow")

local T = MiniTest.new_set()

---@param stage table
---@param name string
local function outcome(stage, name)
  for _, candidate in ipairs(stage.outcomes) do
    if candidate.name == name then
      return candidate
    end
  end
  return nil
end

T["the reference qa-review workflow is accepted by Validation"] = function()
  local result = Workflow.validate("qa-review", Reference.qa_review())

  MiniTest.expect.equality(result.ok, true)
  MiniTest.expect.equality(result.generated_work_max, 5)
end

T["one stage both produces work and loops automatically"] = function()
  local manifest = Reference.qa_review()
  local stage = manifest.qa_review

  -- This combination is the entire point of the fixture. Every other manifest
  -- in the repository has a Generator or an automatic back-edge but not both,
  -- and it is the pairing that makes a bounded graph an unbounded effect: each
  -- traversal creates more work to review. A budget proof built on a manifest
  -- missing either half would pass while proving nothing.
  MiniTest.expect.equality(stage.generates.max, 3)
  local loop = assert(outcome(stage, "another_round"))
  MiniTest.expect.equality(loop["back-edge"], true)
  MiniTest.expect.equality(loop.resolver, "agent")
  MiniTest.expect.equality(loop["max-iterations"], 4)
  MiniTest.expect.equality(loop["on-exhausted"], "accepted")
  MiniTest.expect.equality(assert(outcome(stage, "accepted")).terminal, true)
end

T["callers can narrow every bound the budget proof depends on"] = function()
  local manifest = Reference.qa_review({ generated_work_max = 2, generates_max = 1, max_iterations = 1 })

  MiniTest.expect.equality(Workflow.validate("qa-review", manifest).ok, true)
  MiniTest.expect.equality(manifest.execute["generated-work"].max, 2)
  MiniTest.expect.equality(manifest.qa_review.generates.max, 1)
  MiniTest.expect.equality(assert(outcome(manifest.qa_review, "another_round"))["max-iterations"], 1)
end

return T
