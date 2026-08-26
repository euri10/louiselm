local MiniTest = require("mini.test")
local Workflow = require("louiselm.routing")

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local T = MiniTest.new_set()
local phase = {
  primary = "implementation",
  secondary = {},
  source = "explicit",
  confidence = 1,
}

local function state(status)
  return {
    agent = "alpha",
    status = status,
    config_options = {
      {
        id = "model",
        name = "Model",
        category = "model",
        type = "select",
        current_value = "sonnet",
        options = {
          { value = "sonnet", name = "Sonnet" },
          { value = "opus", name = "Opus" },
        },
      },
    },
    context = { size = 100, used = 10 },
  }
end

local function workflow()
  local path = nvim.fn.tempname()
  local value = assert(Workflow.new({
    alpha = { capabilities = { "coding" } },
    beta = { capabilities = { "reasoning" } },
  }, path))
  return value, path
end

T["recommend"] = MiniTest.new_set()

T["recommend"]["joins evidence, ranking, and approval at an idle boundary"] = function()
  local workflow, path = workflow()
  local pending, ranking, error_message = workflow:recommend(phase, state("ready"))

  MiniTest.expect.equality(error_message, nil)
  local top_ranking = assert(ranking).candidates[1]
  local top_pending = assert(pending).candidates[1]
  MiniTest.expect.equality(top_ranking.action, "continue")
  MiniTest.expect.equality(top_pending.action, "continue")
  MiniTest.expect.equality(top_pending.label:match("CONTINUE"), "CONTINUE")

  local active_pending, _, active_error = workflow:recommend(phase, state("prompting"))
  MiniTest.expect.equality(active_pending, nil)
  MiniTest.expect.equality(active_error, "recommendations require an idle session")
  nvim.fn.delete(path)
end

T["recommend"]["records outcome and feedback against the phase-scoped target"] = function()
  local workflow, path = workflow()
  local current = state("ready")
  assert(workflow:observe(phase, current, "completed"))
  assert(workflow:feedback(phase, current, "good"))

  local evidence = assert(workflow.evidence:evidence())
  local phase_evidence
  for _, record in ipairs(evidence) do
    if record.phase == "implementation" then
      phase_evidence = record
      break
    end
  end
  assert(phase_evidence)
  MiniTest.expect.equality(phase_evidence.agent, "alpha")
  MiniTest.expect.equality(phase_evidence.samples, 1)
  MiniTest.expect.equality(phase_evidence.quality, 1)
  nvim.fn.delete(path)
end

return T
