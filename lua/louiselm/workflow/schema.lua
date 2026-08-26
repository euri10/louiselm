---Field-level shape of a workflow stage's frontmatter block.
---
---The schema is derived from the rejections rather than from a guess at what workflows will
---eventually need: every field below exists because some rejection cannot be expressed without it.
---A field nobody rejects against does not belong here yet — `louiselm-qbr.3` owns the full format
---and should inherit this minimum, not a wishlist.
---
---One obligation is met by omission. There is no way to declare a detached or fire-and-forget
---worker, so a document that tries produces an unknown field. An obligation satisfied by
---construction cannot be forgotten by a later contributor.

local M = {}

---Keys a stage block may carry. Anything else is rejected as an unknown field.
M.STAGE_FIELDS = {
  workflow = "string",
  entry = "boolean",
  outcomes = "array",
  generates = "table",
  ["generated-work"] = "table",
}

---Keys an outcome may carry.
M.OUTCOME_FIELDS = {
  name = "string",
  to = "string",
  terminal = "boolean",
  resolver = "string",
  ["back-edge"] = "boolean",
  ["max-iterations"] = "number",
  ["on-exhausted"] = "string",
}

---Keys a `generates` block may carry.
M.GENERATES_FIELDS = {
  max = "number",
}

---Keys the entry stage's Run-wide generated-work block may carry.
M.GENERATED_WORK_FIELDS = {
  max = "number",
}

---Resolvers that act without human attention. A back edge taken by any of these needs a literal
---bound; a back edge a human takes is bounded by the human choosing to take it again.
M.AUTOMATIC_RESOLVERS = {
  agent = true,
  trigger = true,
  resolver = true,
}

---The resolver assumed when an outcome does not name one. Automatic is the safe default: a
---document that forgets to say who resolves a back edge is asked for a bound rather than excused
---from one.
M.DEFAULT_RESOLVER = "agent"

M.HUMAN_RESOLVER = "human"

return M
