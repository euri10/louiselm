---The reference qa-review workflow used to exercise Run budget enforcement.
---
---This is the shape `louiselm-qbr.3.2` exists to bound: a `qa-review` stage that
---*both* produces work and loops back automatically. Neither half is unusual on
---its own, and the repository's other manifests have one or the other — the
---combination is what turns a bounded graph into an unbounded effect, because
---acyclicity says nothing about termination when each traversal creates more
---work to review.
---
---It is a hand-built manifest rather than a parsed document because no path from
---workflow frontmatter to a stage manifest exists yet (`louiselm-qbr.3.7`).
---Budget enforcement is provable without one: a manifest bounds a loop
---identically whether it was parsed from YAML or written here.

local M = {}

---Build the reference qa-review manifest.
---@param options? { generated_work_max?: integer, generates_max?: integer, max_iterations?: integer }
---@return table<string, table> manifest Stage manifest accepted by `Workflow.validate`.
function M.qa_review(options)
  options = options or {}
  return {
    execute = {
      workflow = "qa-review",
      entry = true,
      ["generated-work"] = { max = options.generated_work_max or 5 },
      ["park-expiry"] = "1h",
      outcomes = {
        { name = "review", to = "qa_review" },
      },
    },
    qa_review = {
      workflow = "qa-review",
      -- The Generator and the back-edge sit on the same stage on purpose: each
      -- review round files findings and then asks for another round.
      generates = { max = options.generates_max or 3 },
      outcomes = {
        {
          name = "another_round",
          to = "execute",
          ["back-edge"] = true,
          resolver = "agent",
          ["max-iterations"] = options.max_iterations or 4,
          ["on-exhausted"] = "accepted",
        },
        { name = "accepted", terminal = true },
      },
    },
  }
end

return M
