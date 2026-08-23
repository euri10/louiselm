local MiniTest = require("mini.test")
local Approval = require("louiselm.workflow.approval")

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local T = MiniTest.new_set()

local phase = {
  primary = "implementation",
  secondary = {},
  source = "explicit",
  confidence = 1,
}

local function candidate(action, agent, model, score)
  return {
    action = action,
    agent = agent,
    model = model,
    score = score,
    confidence = score,
    reasons = { "reason " .. agent },
  }
end

local function ranking()
  return {
    candidates = {
      candidate("handoff", "beta", "flash", 0.9),
      candidate("model", "alpha", "opus", 0.8),
      candidate("continue", "alpha", "sonnet", 0.7),
    },
    rejected = {},
  }
end

local function pending(approval)
  return assert(approval:pending(phase))
end

T["queue"] = MiniTest.new_set()

T["queue"]["waits for an idle boundary and presents a short ranked list"] = function()
  local approval = Approval.new()
  local ok, error_message = approval:queue(phase, ranking(), "prompting")

  MiniTest.expect.equality(ok, false)
  MiniTest.expect.equality(error_message, "recommendations require an idle session")

  assert(approval:queue(phase, ranking(), "ready"))
  MiniTest.expect.equality(pending(approval).candidates, {
    {
      action = "handoff",
      agent = "beta",
      model = "flash",
      score = 0.9,
      confidence = 0.9,
      reasons = { "reason beta" },
      label = "HANDOFF beta/flash · 90% confidence · reason beta",
    },
    {
      action = "model",
      agent = "alpha",
      model = "opus",
      score = 0.8,
      confidence = 0.8,
      reasons = { "reason alpha" },
      label = "MODEL alpha/opus · 80% confidence · reason alpha",
    },
    {
      action = "continue",
      agent = "alpha",
      model = "sonnet",
      score = 0.7,
      confidence = 0.7,
      reasons = { "reason alpha" },
      label = "CONTINUE alpha/sonnet · 70% confidence · reason alpha",
    },
  })
end

T["queue"]["does not mutate the ranking or replace an open phase decision"] = function()
  local approval = Approval.new()
  local value = ranking()
  local original = nvim.deepcopy(value)

  assert(approval:queue(phase, value, "ready"))
  local ok, error_message = approval:queue(phase, value, "ready")

  MiniTest.expect.equality(ok, false)
  MiniTest.expect.equality(error_message, "phase already has pending recommendations")
  MiniTest.expect.equality(value, original)
end

T["approve"] = MiniTest.new_set()

T["approve"]["keeps the approved choice scoped to its phase"] = function()
  local approval = Approval.new()
  assert(approval:queue(phase, ranking(), "ready"))

  local choice = pending(approval).candidates[2]
  local approved = assert(approval:approve(choice))

  MiniTest.expect.equality(approved.action, "model")
  MiniTest.expect.equality(approval:pending(phase), nil)
  MiniTest.expect.equality(approval:approved(phase), approved)
  MiniTest.expect.equality(
    approval:approved({ primary = "qa", secondary = {}, source = "explicit", confidence = 1 }),
    nil
  )
end

T["approve"]["rejects a choice that is not pending"] = function()
  local approval = Approval.new()
  local ok, error_message = approval:approve(candidate("continue", "alpha", "sonnet", 0.5))

  MiniTest.expect.equality(ok, nil)
  MiniTest.expect.equality(error_message, "recommendation is not pending")
end

T["reject"] = MiniTest.new_set()

T["reject"]["suppresses a rejected recommendation until reconsideration"] = function()
  local approval = Approval.new()
  assert(approval:queue(phase, ranking(), "ready"))
  assert(approval:reject(pending(approval).candidates[1]))

  MiniTest.expect.equality(pending(approval).candidates[1].agent, "alpha")
  approval:clear_pending(phase)
  assert(approval:queue(phase, ranking(), "ready"))
  MiniTest.expect.equality(pending(approval).candidates[1].agent, "alpha")

  assert(approval:reconsider(phase, "ready"))
  MiniTest.expect.equality(pending(approval).candidates[1].agent, "beta")
end

T["reject"]["allows phase invalidation to start a fresh decision"] = function()
  local approval = Approval.new()
  assert(approval:queue(phase, ranking(), "ready"))
  assert(approval:approve(pending(approval).candidates[1]))

  assert(approval:invalidate(phase))
  MiniTest.expect.equality(approval:approved(phase), nil)
  assert(approval:queue(phase, ranking(), "ready"))
  MiniTest.expect.equality(pending(approval).candidates[1].agent, "beta")
end

return T
