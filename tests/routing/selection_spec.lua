local MiniTest = require("mini.test")
local Routing = require("louiselm.routing.routing")

---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local T = MiniTest.new_set()

local function request()
  return {
    agent = "alpha",
    definition = {
      provider = "OpenAI",
      capabilities = { "coding" },
      auto = {
        model = "large",
        effort = "high",
        rules = {
          implementation = {
            model = "small",
            effort = "low",
            policy_revision = "policy-1",
            require_traits = { "coding" },
          },
        },
      },
    },
    options = {
      {
        id = "model",
        category = "model",
        type = "select",
        current_value = "large",
        options = { { value = "large" }, { value = "small" } },
      },
      {
        id = "effort",
        category = "thought_level",
        type = "select",
        current_value = "high",
        options = { { value = "high" }, { value = "low" } },
      },
    },
    metadata = { phase = { primary = "implementation", secondary = {}, source = "explicit", confidence = 1 } },
  }
end

local function proof(scope)
  return {
    revision = 7,
    report_id = "comparison",
    policy_revision = "policy-1",
    report = {
      workload = nvim.deepcopy(scope.workload),
      baseline = nvim.deepcopy(scope.baseline),
      candidate = nvim.deepcopy(scope.candidate),
      policy_revision = scope.policy_revision,
    },
    economics = {
      { route = "baseline", kind = "estimated", metric = "api_cost", value = 1, unit = "USD", provenance = "operator" },
      {
        route = "candidate",
        kind = "estimated",
        metric = "api_cost",
        value = 0.25,
        unit = "USD",
        provenance = "operator",
      },
    },
  }
end

T["submission metadata is closed and contradictory phases cannot be consumed"] = function()
  MiniTest.expect.equality(Routing.valid_metadata(nil), true)
  MiniTest.expect.equality(Routing.valid_metadata({ parent_turn_id = "parent", skill = "selected" }), true)
  MiniTest.expect.equality(Routing.valid_metadata(request().metadata), true)
  for _, metadata in ipairs({
    "implementation",
    { workload = "" },
    { skill = false },
    { parent_turn_id = 3 },
    { prompt = "not allowed" },
    { phase = { primary = "implementation", source = "explicit", confidence = 0.5 } },
    { phase = { primary = "unknown", source = "explicit", confidence = 1 } },
    { phase = { primary = "implementation", source = "inferred", confidence = 0 / 0 } },
  }) do
    MiniTest.expect.equality(Routing.valid_metadata(metadata), false)
  end
end

T["selection records reject malformed proof"] = function()
  local scope = assert(Routing.scope(request()))
  for _, change in ipairs({
    function(value)
      value.prompt = "not recorded"
    end,
    function(value)
      value.requested.model = true
    end,
    function(value)
      value.qualification.revision = -1
    end,
    function(value)
      value.qualification.approval_revision = "new"
    end,
    function(value)
      value.economic_basis[1].value = -1
    end,
    function(value)
      value.economic_basis[1].route = "other"
    end,
    function(value)
      value.qualification = nil
    end,
  }) do
    local selected = Routing.select(scope, proof(scope))
    change(selected)
    MiniTest.expect.equality(Routing.valid_selection(selected), false)
  end
end

T["explicit scope"] = function()
  local input = request()
  local before = nvim.deepcopy(input)
  local scope = assert(Routing.scope(input))
  MiniTest.expect.equality(input, before)
  MiniTest.expect.equality(scope.workload, { kind = "main", id = "implementation" })
  MiniTest.expect.equality(scope.baseline.options, { model = "large", effort = "high" })
  MiniTest.expect.equality(scope.candidate.options, { model = "small", effort = "low" })
  local approved = proof(scope)
  local decision = Routing.select(scope, approved)
  MiniTest.expect.equality(decision.reason, "qualified")
  MiniTest.expect.equality(decision.requested, { model = "small", effort = "low" })
  MiniTest.expect.equality(decision.baseline, { model = "large", effort = "high" })
  MiniTest.expect.equality(decision.qualification.report_id, "comparison")
  MiniTest.expect.equality(decision.economic_basis, approved.economics)
  MiniTest.expect.equality(Routing.valid_selection(decision), true)
end

T["scope refuses uncertain workloads, capabilities and unsupported options"] = function()
  local cases = {
    {
      reason = "workload_unknown",
      change = function(input)
        input.metadata = {}
      end,
    },
    {
      reason = "workload_unknown",
      change = function(input)
        input.metadata.phase.source = "inferred"
      end,
    },
    {
      reason = "rule_missing",
      change = function(input)
        input.metadata.workload = "unqualified-task"
      end,
    },
    {
      reason = "capability_unavailable",
      change = function(input)
        input.definition.capabilities = {}
      end,
    },
    {
      reason = "option_unsupported",
      change = function(input)
        input.options[1].options = { { value = "large" } }
      end,
    },
    {
      reason = "option_unsupported",
      change = function(input)
        input.options[2].options = { { value = "high" } }
      end,
    },
    {
      reason = "option_unsupported",
      change = function(input)
        input.definition.auto.model = "removed-baseline"
      end,
    },
    {
      reason = "option_unsupported",
      change = function(input)
        input.definition.auto.effort = "removed-baseline"
      end,
    },
    {
      reason = "provider_unresolved",
      change = function(input)
        input.definition.provider = { option = "model", prefixes = { large = "OpenAI" } }
      end,
    },
  }
  for _, case in ipairs(cases) do
    local input = request()
    case.change(input)
    MiniTest.expect.equality({ Routing.scope(input) }, { nil, case.reason })
  end
  local input = request()
  input.metadata.skill = "selected-skill"
  input.definition.auto.rules["selected-skill"] = nvim.deepcopy(input.definition.auto.rules.implementation)
  MiniTest.expect.equality(assert(Routing.scope(input)).workload.id, "selected-skill")
  input.metadata.workload = "implementation"
  MiniTest.expect.equality(assert(Routing.scope(input)).workload.id, "implementation")
end

T["missing, mismatched or role-less approval cannot route"] = function()
  local scope = assert(Routing.scope(request()))
  MiniTest.expect.equality(Routing.select(scope, nil).fallback_reason, "unqualified")
  local cases = {
    function(value)
      value.report.workload.id = "review"
    end,
    function(value)
      value.report.candidate.provider = "other"
    end,
    function(value)
      value.report.candidate.agent = "other"
    end,
    function(value)
      value.report.candidate.options.effort = "high"
    end,
    function(value)
      value.report.baseline.model = "other"
    end,
    function(value)
      value.report.policy_revision = "policy-2"
    end,
  }
  for _, change in ipairs(cases) do
    local approved = proof(scope)
    change(approved)
    MiniTest.expect.equality(Routing.select(scope, approved).fallback_reason, "unqualified")
  end
  local approved = proof(scope)
  approved.economics = nil
  MiniTest.expect.equality(Routing.select(scope, approved).fallback_reason, "economics_unknown")
  approved = proof(scope)
  approved.economics[1].route = nil
  MiniTest.expect.equality(Routing.select(scope, approved).fallback_reason, "economics_unknown")
end

T["currencies, quota and latency never stand in for comparable API cost"] = function()
  local scope = assert(Routing.scope(request()))
  for _, metric in ipairs({ "quota", "latency" }) do
    local approved = proof(scope)
    for _, entry in ipairs(approved.economics) do
      entry.metric, entry.unit = metric, "requests"
    end
    MiniTest.expect.equality(Routing.select(scope, approved).fallback_reason, "economics_unknown")
  end
  local approved = proof(scope)
  approved.economics[2].unit = "EUR"
  MiniTest.expect.equality(Routing.select(scope, approved).fallback_reason, "economics_unknown")
  approved = proof(scope)
  approved.economics[2].kind = "measured"
  MiniTest.expect.equality(Routing.select(scope, approved).fallback_reason, "economics_unknown")
  approved = proof(scope)
  approved.economics[3] = nvim.deepcopy(approved.economics[2])
  MiniTest.expect.equality(Routing.select(scope, approved).fallback_reason, "economics_unknown")
end

T["extra API cost needs a matching allowance and real comparable quota improvement"] = function()
  local scope = assert(Routing.scope(request()))
  local approved = proof(scope)
  approved.economics[2].value = 1.5
  approved.economics[3] = {
    route = "baseline",
    kind = "measured",
    metric = "quota",
    value = 2,
    unit = "requests",
    provenance = "approved capture",
  }
  approved.economics[4] = {
    route = "candidate",
    kind = "measured",
    metric = "quota",
    value = 1,
    unit = "requests",
    provenance = "approved capture",
  }
  MiniTest.expect.equality(Routing.select(scope, approved).fallback_reason, "allowance_missing")
  approved.api_for_quota = { max_extra_cost = { value = 0.4, currency = "USD" }, provenance = "operator" }
  MiniTest.expect.equality(Routing.select(scope, approved).fallback_reason, "allowance_insufficient")
  approved.api_for_quota.max_extra_cost = { value = 1, currency = "EUR" }
  MiniTest.expect.equality(Routing.select(scope, approved).fallback_reason, "allowance_insufficient")
  approved.api_for_quota.max_extra_cost.currency = "USD"
  MiniTest.expect.equality(Routing.select(scope, approved).reason, "qualified")
  approved.economics[4].value = 3
  MiniTest.expect.equality(Routing.select(scope, approved).fallback_reason, "economics_worse")
  approved.economics[4].value = 2
  MiniTest.expect.equality(Routing.select(scope, approved).fallback_reason, "no_economic_benefit")
end

return T
