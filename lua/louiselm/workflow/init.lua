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

local M = {}

M.validate = Validate.validate
M.new_run = Run.new

return M
