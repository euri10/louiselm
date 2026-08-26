---Workflow definitions: the declarative stage graph a Run executes.
---
---This namespace holds the definition side of the two contracts settled in `louiselm-qbr.8`.
---Validation here is pure, total and binary: it reads structured frontmatter, never prose, and a
---definition is either accepted or rejected. The Run side — admission, budgets, Park — is a
---separate contract, because an open skill graph makes recursion undecidable from any single
---document and a validator that claimed otherwise would be selling a guarantee it cannot deliver.
---
---Not to be confused with `louiselm.routing`, which ranks Agents for a phase of work.

local Validate = require("louiselm.workflow.validate")
local Run = require("louiselm.workflow.run")
local Escape = require("louiselm.workflow.escape")

local M = {}

---Validate a workflow after synthesizing its mandatory human Park escape.
---@param workflow string Workflow name.
---@param manifest table<string, table> Discovered stage manifest.
---@return louiselm.workflow.Result result
function M.validate(workflow, manifest)
  return Validate.validate(workflow, Escape.apply(workflow, manifest))
end

M.synthesize = Escape.apply
M.new_run = Run.new

return M
