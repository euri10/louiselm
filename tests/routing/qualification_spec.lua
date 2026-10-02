local MiniTest = require("mini.test")
local Qualification = require("louiselm.routing.qualification")
local Evidence = require("louiselm.routing.evidence")

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim
local T = MiniTest.new_set()

local function report()
  return {
    version = 1,
    id = "selected-comparison-1",
    policy_revision = "policy-1",
    workload = { kind = "main", id = "implementation" },
    baseline = {
      agent = "alpha",
      provider = "OpenAI",
      model = "large",
      model_option_id = "model",
      options = { model = "large", effort = "high" },
    },
    candidate = {
      agent = "alpha",
      provider = "OpenAI",
      model = "small",
      model_option_id = "model",
      options = { model = "small", effort = "low" },
    },
    fixtures = {
      {
        id = "selected-task-1",
        source = "operator-selected snapshot",
        digest = "sha256:" .. string.rep("a", 64),
        checks = { { id = "expected-result", baseline = "pass", candidate = "pass" } },
        human_review_required = true,
        human = { baseline = "pass", candidate = "pass", reviewer = "operator" },
      },
    },
  }
end

local function request(value)
  return {
    workload = value.workload,
    baseline = value.baseline,
    candidate = value.candidate,
    policy_revision = value.policy_revision,
  }
end

local function await_call(start)
  local done, first, second = false, nil, nil
  start(function(value, err)
    first, second, done = value, err, true
  end)
  assert(
    nvim.wait(2000, function()
      return done
    end),
    "qualification callback did not run"
  )
  return first, second
end

local function store(path, options)
  return assert(Qualification.new(path, options))
end

local function decide(value, input)
  return await_call(function(callback)
    value:decide(input, callback)
  end)
end

local function lookup(value, selected, revision)
  return await_call(function(callback)
    value:lookup(selected, revision, callback)
  end)
end

local function revision(value)
  return await_call(function(callback)
    value:revision(callback)
  end)
end

T["approval"] = MiniTest.new_set()

T["approval"]["economics retains route attribution and refuses arbitrary route labels"] = function()
  local path = nvim.fn.tempname()
  MiniTest.finally(function()
    nvim.fn.delete(path)
  end)
  local value = store(path)
  local input = {
    report = report(),
    action = "approve",
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
  assert(decide(value, input))
  MiniTest.expect.equality(assert(lookup(value, request(input.report))).economics, input.economics)
  input.economics[2].route = "tool"
  MiniTest.expect.equality(select(2, decide(value, input)), "comparison economics or allowance has invalid schema")
end

T["approval"]["explicit approval survives reload and matches only its complete scope"] = function()
  local path = nvim.fn.tempname()
  local selected = report()
  local value = store(path)
  MiniTest.expect.equality(lookup(value, request(selected)), nil)

  local approved = assert(decide(value, { report = selected, action = "approve" }))
  local reopened = store(path)
  local qualified = assert(lookup(reopened, request(selected), approved.revision))
  MiniTest.expect.equality(qualified.report_id, selected.id)
  MiniTest.expect.equality(qualified.policy_revision, selected.policy_revision)
  MiniTest.expect.equality(qualified.revision, approved.revision)

  local wrong = request(selected)
  wrong.workload = { kind = "reader", id = "implementation" }
  MiniTest.expect.equality(lookup(reopened, wrong), nil)
  wrong = request(selected)
  wrong.candidate = nvim.deepcopy(selected.candidate)
  wrong.candidate.options.effort = "high"
  MiniTest.expect.equality(lookup(reopened, wrong), nil)
  wrong = request(selected)
  wrong.candidate = nvim.deepcopy(selected.candidate)
  wrong.candidate.provider = "other"
  MiniTest.expect.equality(lookup(reopened, wrong), nil)
  wrong = request(selected)
  wrong.baseline = nvim.deepcopy(selected.baseline)
  wrong.baseline.model = "different"
  wrong.baseline.options.model = "different"
  MiniTest.expect.equality(lookup(reopened, wrong), nil)
  wrong = request(selected)
  wrong.policy_revision = "policy-2"
  local mismatched, mismatch_error = lookup(reopened, wrong)
  MiniTest.expect.equality(mismatched, nil)
  MiniTest.expect.equality(mismatch_error, nil)
  MiniTest.expect.equality(
    select(2, lookup(reopened, request(selected), approved.revision - 1)),
    "approval revision changed"
  )
  nvim.fn.delete(path)
end

T["approval"]["reader approval is distinct and a later rejection removes qualification"] = function()
  local path = nvim.fn.tempname()
  local selected = report()
  selected.workload = { kind = "reader", id = "selected-content-question" }
  local value = store(path)
  assert(decide(value, { report = selected, action = "approve" }))
  MiniTest.expect.equality(assert(lookup(value, request(selected))).report_id, selected.id)
  assert(decide(value, { report = selected, action = "reject" }))
  MiniTest.expect.equality(lookup(store(path), request(selected)), nil)
  MiniTest.expect.equality(revision(store(path)), 2)
  nvim.fn.delete(path)
end

T["approval"]["incomplete review and failed acceptance cannot be approved"] = function()
  local path = nvim.fn.tempname()
  local value = store(path)
  local selected = report()
  selected.fixtures[1].human.candidate = "pending"
  MiniTest.expect.equality(
    select(2, decide(value, { report = selected, action = "approve" })),
    "comparison acceptance is incomplete or failed"
  )
  selected.fixtures[1].human.candidate = "pass"
  selected.fixtures[1].checks[1].candidate = "fail"
  MiniTest.expect.equality(
    select(2, decide(value, { report = selected, action = "approve" })),
    "comparison acceptance is incomplete or failed"
  )
  MiniTest.expect.equality(
    select(
      2,
      decide(value, {
        report = selected,
        action = "approve",
        economics = { { kind = "estimated", metric = "api_cost", value = 0, unit = "USD", provenance = "operator" } },
      })
    ),
    "comparison acceptance is incomplete or failed"
  )
  assert(decide(value, { report = selected, action = "reject" }))
  MiniTest.expect.equality(lookup(value, request(selected)), nil)
  selected.fixtures[1].checks[1].candidate = "pass"
  selected.fixtures[1].human = nil
  MiniTest.expect.equality(
    select(2, decide(value, { report = selected, action = "approve" })),
    "comparison acceptance is incomplete or failed"
  )
  assert(decide(value, { report = selected, action = "reject" }))
  nvim.fn.delete(path)
end

T["approval"]["malformed or unsupported reports cannot qualify"] = function()
  local path = nvim.fn.tempname()
  local value = store(path)
  local selected = report()
  selected.candidate.options.model = "unadvertised"
  MiniTest.expect.equality(
    select(2, decide(value, { report = selected, action = "approve" })),
    "comparison report has invalid schema"
  )
  selected = report()
  selected.qualified = true
  MiniTest.expect.equality(
    select(2, decide(value, { report = selected, action = "approve" })),
    "comparison report has invalid schema"
  )
  MiniTest.expect.equality(revision(value), 0)
  nvim.fn.delete(path)
end

T["approval"]["corrupt saved decisions fail closed"] = function()
  local path = nvim.fn.tempname()
  assert(nvim.fn.writefile({ nvim.json.encode({ version = 1, revision = 1, decisions = {} }) }, path) == 0)
  local selected = report()
  local qualified, error_message = lookup(store(path), request(selected))
  MiniTest.expect.equality(qualified, nil)
  MiniTest.expect.equality(error_message, "comparison approvals have invalid schema")
  nvim.fn.delete(path)
end

T["approval"]["estimates, measurements and API-for-quota allowance remain separate"] = function()
  local path = nvim.fn.tempname()
  local selected = report()
  local value = store(path)
  local result = assert(decide(value, {
    report = selected,
    action = "approve",
    economics = {
      { kind = "estimated", metric = "api_cost", value = 0.25, unit = "USD", provenance = "operator price sheet" },
      { kind = "measured", metric = "quota", value = 2, unit = "requests", provenance = "selected capture" },
    },
    api_for_quota = { max_extra_cost = { value = 1, currency = "USD" }, provenance = "operator allowance" },
  }))
  local qualified = assert(lookup(store(path), request(selected)))
  MiniTest.expect.equality(qualified.economics, result.economics)
  MiniTest.expect.equality(qualified.api_for_quota, result.api_for_quota)
  MiniTest.expect.equality(qualified.economics[1].kind, "estimated")
  MiniTest.expect.equality(qualified.economics[2].kind, "measured")
  nvim.fn.delete(path)
end

T["approval"]["failed publication keeps the preceding decision and revision"] = function()
  local path = nvim.fn.tempname()
  local selected = report()
  assert(decide(store(path), { report = selected, action = "approve" }))
  local failing = store(path, {
    write = function(_, _, callback)
      nvim.schedule(function()
        callback(false, "disk full")
      end)
    end,
  })
  MiniTest.expect.equality(select(2, decide(failing, { report = selected, action = "reject" })), "disk full")
  MiniTest.expect.equality(revision(store(path)), 1)
  MiniTest.expect.equality(assert(lookup(store(path), request(selected))).report_id, selected.id)
  nvim.fn.delete(path)
end

T["approval"]["completed turns and Good feedback are not qualification"] = function()
  local path = nvim.fn.tempname()
  local selected = report()
  local evidence = assert(Evidence.new(path .. ".evidence"))
  assert(evidence:observe({ phase = "implementation", agent = "alpha", model = "small" }, "completed"))
  assert(evidence:feedback({ phase = "implementation", agent = "alpha", model = "small" }, "good"))
  MiniTest.expect.equality(lookup(store(path), request(selected)), nil)
  nvim.fn.delete(path .. ".evidence")
end

T["command"] = MiniTest.new_set()

T["command"]["selected report command makes durable approve and reject decisions"] = function()
  local selected = report()
  local report_path = nvim.fn.tempname() .. ".json"
  assert(nvim.fn.writefile({ nvim.json.encode(selected) }, report_path) == 0)
  local path = nvim.fs.joinpath(require("louiselm.paths").state(), "routing-evidence.json.qualifications.json")
  nvim.cmd("LouiselmApproveComparison " .. nvim.fn.fnameescape(report_path))
  assert(nvim.wait(2000, function()
    return nvim.fn.filereadable(path) == 1
  end))
  MiniTest.expect.equality(assert(lookup(store(path), request(selected))).report_id, selected.id)
  local evidence_path = nvim.fs.joinpath(require("louiselm.paths").state(), "routing-evidence.json")
  local workflow = assert(require("louiselm.routing").new({ alpha = { capabilities = {} } }, evidence_path))
  local through_coordinator = assert(await_call(function(callback)
    workflow:qualified(request(selected), nil, callback)
  end))
  MiniTest.expect.equality(through_coordinator.report_id, selected.id)

  nvim.cmd("LouiselmRejectComparison " .. nvim.fn.fnameescape(report_path))
  assert(nvim.wait(2000, function()
    local content = nvim.fn.readfile(path)
    return nvim.json.decode(table.concat(content, "\n")).revision == 2
  end))
  MiniTest.expect.equality(lookup(store(path), request(selected)), nil)
  nvim.fn.delete(report_path)
  nvim.fn.delete(path)
end

return T
