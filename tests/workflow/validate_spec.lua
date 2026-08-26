local MiniTest = require("mini.test")
local Workflow = require("louiselm.workflow")
local RawValidate = require("louiselm.workflow.validate")

local T = MiniTest.new_set()

---Collect the reason identifiers from a validation result, sorted for stable comparison.
---@param result louiselm.workflow.Result
---@return string[]
local function reasons(result)
  local collected = {}
  for _, rejection in ipairs(result.rejections) do
    collected[#collected + 1] = rejection.reason
  end
  table.sort(collected)
  return collected
end

---@param result louiselm.workflow.Result
---@param reason string
---@return louiselm.workflow.Rejection
local function rejection_of(result, reason)
  for _, rejection in ipairs(result.rejections) do
    if rejection.reason == reason then
      return rejection
    end
  end
  error("no rejection with reason '" .. reason .. "'; got " .. table.concat(reasons(result), ", "))
end

--- The reference loop from docs/example-workflow.md, reduced to what the rejections need.
--- Grill hands to route, route to execute, execute to qa-review, qa-review accepts or returns
--- work to execute through a human-resolved back edge.
---@return table<string, table>
local function reference_manifest()
  return {
    ["grill"] = {
      workflow = "reference",
      entry = true,
      outcomes = { { name = "agreed", to = "route" } },
    },
    ["route"] = {
      workflow = "reference",
      generates = { max = 20 },
      outcomes = { { name = "filed", to = "execute" } },
    },
    ["execute"] = {
      workflow = "reference",
      outcomes = { { name = "committed", to = "qa-review" } },
    },
    ["qa-review"] = {
      workflow = "reference",
      outcomes = {
        { name = "accepted", terminal = true },
        { name = "defect-found", to = "execute", ["back-edge"] = true, resolver = "human" },
      },
    },
  }
end

T["validate"] = MiniTest.new_set()

T["validate"]["accepts the reference workflow"] = function()
  local result = Workflow.validate("reference", reference_manifest())

  MiniTest.expect.equality(reasons(result), {})
  MiniTest.expect.equality(result.ok, true)
end

--- Criterion 9. A universal-bound rule would reject this, and rejecting it is the failure the
--- resolver-keyed rule exists to avoid. The human re-deciding is the bound.
T["validate"]["accepts a human-resolved back edge carrying no bound"] = function()
  local manifest = reference_manifest()

  local result = Workflow.validate("reference", manifest)

  MiniTest.expect.equality(result.ok, true)
  MiniTest.expect.equality(manifest["qa-review"].outcomes[2]["max-iterations"], nil)
end

T["validate"]["accepts a bounded automatic loop whose exhaustion leaves it"] = function()
  local manifest = reference_manifest()
  manifest["route"].outcomes = { { name = "filed", to = "critique" } }
  manifest["critique"] = {
    workflow = "reference",
    outcomes = {
      { name = "survives", to = "execute" },
      {
        name = "needs-revision",
        to = "route",
        ["back-edge"] = true,
        resolver = "agent",
        ["max-iterations"] = 3,
        ["on-exhausted"] = "escalate",
      },
      { name = "escalate", to = "execute", resolver = "human" },
    },
  }

  local result = Workflow.validate("reference", manifest)

  MiniTest.expect.equality(reasons(result), {})
end

T["rejects"] = MiniTest.new_set()

--- Rejection 1.
T["rejects"]["a cycle-forming transition that is not declared a back edge"] = function()
  local manifest = reference_manifest()
  manifest["qa-review"].outcomes[2] = { name = "defect-found", to = "execute", resolver = "human" }

  local result = Workflow.validate("reference", manifest)

  MiniTest.expect.equality(result.ok, false)
  local rejection = rejection_of(result, "undeclared_back_edge")
  MiniTest.expect.equality(rejection.stage, "qa-review")
  MiniTest.expect.equality(rejection.outcome, "defect-found")
end

--- Rejection 2, back-edge form.
T["rejects"]["an automatic back edge with no literal bound"] = function()
  local manifest = reference_manifest()
  manifest["qa-review"].outcomes[2].resolver = "agent"
  manifest["qa-review"].outcomes[2]["on-exhausted"] = "accepted"

  local result = Workflow.validate("reference", manifest)

  MiniTest.expect.equality(rejection_of(result, "unbounded_generator").outcome, "defect-found")
end

--- Rejection 2, generating-stage form. The Generator is the bounding unit, so a stage that files
--- work is bound by the same rule as a back edge.
T["rejects"]["a generating stage with no declared maximum"] = function()
  local manifest = reference_manifest()
  manifest["route"].generates = {}

  local result = Workflow.validate("reference", manifest)

  MiniTest.expect.equality(rejection_of(result, "unbounded_generator").stage, "route")
end

--- A bound must be a literal integer in the document. An expression or a config reference is the
--- thing the rule exists to forbid, so it must not pass as a bound.
T["rejects"]["a bound that is not a literal positive integer"] = function()
  local manifest = reference_manifest()
  manifest["route"].generates = { max = "config.limits.issues" }

  local result = Workflow.validate("reference", manifest)

  MiniTest.expect.equality(rejection_of(result, "unbounded_generator").stage, "route")
end

--- Rejection 3.
T["rejects"]["an exhaustion outcome that transitions back inside its own loop"] = function()
  local manifest = reference_manifest()
  manifest["qa-review"].outcomes[2] = {
    name = "defect-found",
    to = "execute",
    ["back-edge"] = true,
    resolver = "agent",
    ["max-iterations"] = 3,
    ["on-exhausted"] = "retry",
  }
  manifest["qa-review"].outcomes[3] = { name = "retry", to = "execute", resolver = "human" }

  local result = Workflow.validate("reference", manifest)

  MiniTest.expect.equality(rejection_of(result, "exhaustion_inside_loop").outcome, "defect-found")
end

--- A bounded automatic loop with no exhaustion outcome at all still fails to terminate: it
--- exhausts and has nowhere to go.
T["rejects"]["a bounded automatic loop with no exhaustion outcome"] = function()
  local manifest = reference_manifest()
  manifest["qa-review"].outcomes[2].resolver = "agent"
  manifest["qa-review"].outcomes[2]["max-iterations"] = 3

  local result = Workflow.validate("reference", manifest)

  MiniTest.expect.equality(rejection_of(result, "missing_exhaustion_outcome").outcome, "defect-found")
end

--- Rejection 4.
T["rejects"]["a named outcome with neither a target nor a terminal marker"] = function()
  local manifest = reference_manifest()
  manifest["execute"].outcomes = { { name = "committed" } }

  local result = Workflow.validate("reference", manifest)

  MiniTest.expect.equality(rejection_of(result, "unmapped_outcome").outcome, "committed")
end

--- Rejection 5.
T["rejects"]["a transition whose target is absent from the manifest"] = function()
  local manifest = reference_manifest()
  manifest["execute"].outcomes = { { name = "committed", to = "qa-reveiw" } }

  local result = Workflow.validate("reference", manifest)

  MiniTest.expect.equality(rejection_of(result, "unresolved_target").target, "qa-reveiw")
end

--- A skill can exist and still not be part of this workflow. Transitioning into one is as broken
--- as transitioning into nothing.
T["rejects"]["a transition into a skill outside this workflow"] = function()
  local manifest = reference_manifest()
  manifest["unrelated-skill"] = { workflow = "other", outcomes = { { name = "done", terminal = true } } }
  manifest["execute"].outcomes = { { name = "committed", to = "unrelated-skill" } }

  local result = Workflow.validate("reference", manifest)

  MiniTest.expect.equality(rejection_of(result, "unresolved_target").target, "unrelated-skill")
end

--- Rejection 6.
T["rejects"]["a stage from which no terminal outcome is reachable"] = function()
  local manifest = reference_manifest()
  manifest["qa-review"].outcomes = {
    { name = "defect-found", to = "execute", ["back-edge"] = true, resolver = "human" },
  }

  local result = RawValidate.validate("reference", manifest)

  MiniTest.expect.equality(rejection_of(result, "no_reachable_terminal").stage ~= nil, true)
end

--- Rejection 7.
T["rejects"]["a stage claiming membership that the entry cannot reach"] = function()
  local manifest = reference_manifest()
  manifest["orphan"] = {
    workflow = "reference",
    outcomes = { { name = "done", terminal = true } },
  }

  local result = Workflow.validate("reference", manifest)

  MiniTest.expect.equality(rejection_of(result, "unreachable_stage").stage, "orphan")
end

--- Rejection 8. The schema has no way to express a detached worker, so declaring one is an
--- unknown field. Satisfying the obligation by construction beats checking for it.
T["rejects"]["a stage declaring a detached worker"] = function()
  local manifest = reference_manifest()
  manifest["execute"].detached = true

  local result = Workflow.validate("reference", manifest)

  MiniTest.expect.equality(rejection_of(result, "unknown_field").field, "detached")
end

T["rejects"]["a stage declaring a fire-and-forget worker"] = function()
  local manifest = reference_manifest()
  manifest["execute"]["fire-and-forget"] = true

  local result = Workflow.validate("reference", manifest)

  MiniTest.expect.equality(rejection_of(result, "unknown_field").field, "fire-and-forget")
end

T["rejects"]["a workflow with no entry stage"] = function()
  local manifest = reference_manifest()
  manifest["grill"].entry = nil

  local result = Workflow.validate("reference", manifest)

  MiniTest.expect.equality(rejection_of(result, "missing_entry").reason, "missing_entry")
end

T["rejects"]["a workflow declaring more than one entry stage"] = function()
  local manifest = reference_manifest()
  manifest["execute"].entry = true

  local result = Workflow.validate("reference", manifest)

  MiniTest.expect.equality(rejection_of(result, "ambiguous_entry").reason, "ambiguous_entry")
end

T["rejects"]["two outcomes of one stage sharing a name"] = function()
  local manifest = reference_manifest()
  manifest["qa-review"].outcomes[2].name = "accepted"

  local result = Workflow.validate("reference", manifest)

  MiniTest.expect.equality(rejection_of(result, "duplicate_outcome").outcome, "accepted")
end

T["contract"] = MiniTest.new_set()

--- Criterion 10. There is no severity, and no argument that lets a rejected definition through.
T["contract"]["carries no severity and offers no override"] = function()
  local manifest = reference_manifest()
  manifest["execute"].outcomes = { { name = "committed" } }

  local strict = Workflow.validate("reference", manifest)
  -- The diagnostic below IS the assertion: there is no third parameter to carry an override. The
  -- runtime check keeps the guarantee if the signature ever widens.
  ---@diagnostic disable-next-line: redundant-parameter
  local attempted_override = Workflow.validate("reference", manifest, { force = true })

  MiniTest.expect.equality(strict.ok, false)
  MiniTest.expect.equality(attempted_override.ok, false)
  -- Likewise: a rejection carries no severity, so there is no tier to downgrade one into.
  ---@diagnostic disable-next-line: undefined-field
  MiniTest.expect.equality(rejection_of(strict, "unmapped_outcome").severity, nil)
end

--- Criterion 12. Callers assert on the reason, not on prose.
T["contract"]["gives every rejection a stable identifier and a message"] = function()
  local manifest = reference_manifest()
  manifest["execute"].outcomes = { { name = "committed" } }

  local rejection = rejection_of(Workflow.validate("reference", manifest), "unmapped_outcome")

  MiniTest.expect.equality(type(rejection.reason), "string")
  MiniTest.expect.equality(type(rejection.message), "string")
  MiniTest.expect.equality(rejection.message ~= rejection.reason, true)
end

--- Criterion 11. Topology comes from structured fields; the body is never consulted. A document
--- whose prose describes transitions that the fields do not declare is not a workflow that has
--- them.
T["contract"]["reads topology from fields alone and never from prose"] = function()
  local manifest = reference_manifest()
  manifest["execute"].body = "On failure this stage returns to grill and retries forever."

  local result = Workflow.validate("reference", manifest)

  MiniTest.expect.equality(rejection_of(result, "unknown_field").field, "body")
end

--- The regression that protects the design. Generative growth mints a new tracker id every pass,
--- so it forms no cycle and no reachability rule can see it. Only a Run budget stops it. If a
--- future change makes this reject, a graph check has been asked to do a job it cannot do.
T["contract"]["accepts a generating workflow that grows without forming a cycle"] = function()
  local manifest = {
    ["qa-review"] = {
      workflow = "generative",
      entry = true,
      generates = { max = 5 },
      outcomes = { { name = "round-complete", terminal = true } },
    },
  }

  local result = Workflow.validate("generative", manifest)

  MiniTest.expect.equality(reasons(result), {})
end

return T
