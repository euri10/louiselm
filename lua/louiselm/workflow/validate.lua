---@class louiselm.workflow.Rejection
---@field reason string Stable machine-readable identifier; callers assert on this, never on prose.
---@field message string Human-facing sentence naming what to change.
---@field stage? string Stage the rejection belongs to.
---@field outcome? string Outcome within that stage, when the rejection is edge-shaped.
---@field target? string Transition target, when the rejection is about resolution.
---@field field? string Offending key, when the rejection is about shape.

---@class louiselm.workflow.Result
---@field ok boolean
---@field rejections louiselm.workflow.Rejection[]
---@field generated_work_max? integer Authored Run-wide ceiling when Validation succeeds.

local Schema = require("louiselm.workflow.schema")

local M = {}

---@param rejections louiselm.workflow.Rejection[]
---@param rejection louiselm.workflow.Rejection
local function reject(rejections, rejection)
  rejections[#rejections + 1] = rejection
end

---@param value unknown
---@return boolean
local function is_array(value)
  if type(value) ~= "table" then
    return false
  end
  local length = 0
  while value[length + 1] ~= nil do
    length = length + 1
  end
  for key in pairs(value) do
    if type(key) ~= "number" or key < 1 or key % 1 ~= 0 or key > length then
      return false
    end
  end
  return true
end

---A bound must be readable as a number by whoever reads the document. An expression, a config
---reference, or a fractional count is exactly what the literal requirement exists to forbid.
---@param value unknown
---@return boolean
local function is_literal_count(value)
  return type(value) == "number" and value % 1 == 0 and value >= 1
end

---@param value unknown
---@param expected string
---@return boolean
local function has_type(value, expected)
  if expected == "array" then
    return is_array(value)
  end
  return type(value) == expected
end

---Reject any key the schema does not name. This is how a detached worker is refused: the schema
---gives no way to declare one, so the attempt surfaces as an unknown field.
---@param rejections louiselm.workflow.Rejection[]
---@param block table
---@param allowed table<string, string>
---@param stage string
---@param outcome? string
local function check_fields(rejections, block, allowed, stage, outcome)
  local keys = {}
  for key in pairs(block) do
    keys[#keys + 1] = tostring(key)
  end
  table.sort(keys)
  for _, key in ipairs(keys) do
    local expected = allowed[key]
    if expected == nil then
      reject(rejections, {
        reason = "unknown_field",
        message = string.format("stage '%s' declares unknown field '%s'", stage, key),
        stage = stage,
        outcome = outcome,
        field = key,
      })
    elseif not has_type(block[key], expected) then
      reject(rejections, {
        reason = "wrong_field_type",
        message = string.format("stage '%s' field '%s' must be a %s", stage, key, expected),
        stage = stage,
        outcome = outcome,
        field = key,
      })
    end
  end
end

---@param stages table<string, table>
---@return table<string, table<string, boolean>> edges
local function build_edges(stages)
  local edges = {}
  for name, block in pairs(stages) do
    edges[name] = {}
    if is_array(block.outcomes) then
      for _, outcome in ipairs(block.outcomes) do
        if type(outcome) == "table" and type(outcome.to) == "string" and stages[outcome.to] ~= nil then
          edges[name][outcome.to] = true
        end
      end
    end
  end
  return edges
end

---@param edges table<string, table<string, boolean>>
---@param roots string[]
---@return table<string, boolean>
local function reachable_from(edges, roots)
  local seen = {}
  local queue = {}
  for _, root in ipairs(roots) do
    if edges[root] ~= nil and not seen[root] then
      seen[root] = true
      queue[#queue + 1] = root
    end
  end
  local index = 1
  while index <= #queue do
    local current = queue[index]
    index = index + 1
    for target in pairs(edges[current]) do
      if not seen[target] then
        seen[target] = true
        queue[#queue + 1] = target
      end
    end
  end
  return seen
end

---@param edges table<string, table<string, boolean>>
---@return table<string, table<string, boolean>>
local function reverse(edges)
  local reversed = {}
  for name in pairs(edges) do
    reversed[name] = reversed[name] or {}
  end
  for name, targets in pairs(edges) do
    for target in pairs(targets) do
      reversed[target][name] = true
    end
  end
  return reversed
end

---Classify back edges by ancestry from the entry, not by mutual reachability.
---
---In a cycle every edge can reach its own source, so "target reaches source" marks both the
---forward edge and the return edge. The back edge is the one that returns to a stage still open on
---the path from the entry — an ancestor. Successors are walked in sorted order, so the
---classification is deterministic for a given graph rather than an artefact of table iteration.
---@param edges table<string, table<string, boolean>>
---@param entry string
---@return table<string, table<string, boolean>> back_edges Source stage to targets it returns to.
local function classify_back_edges(edges, entry)
  local back = {}
  local on_path = {}
  local finished = {}

  local function walk(stage)
    on_path[stage] = true
    local targets = {}
    for target in pairs(edges[stage] or {}) do
      targets[#targets + 1] = target
    end
    table.sort(targets)
    for _, target in ipairs(targets) do
      if on_path[target] then
        back[stage] = back[stage] or {}
        back[stage][target] = true
      elseif not finished[target] and edges[target] ~= nil then
        walk(target)
      end
    end
    on_path[stage] = nil
    finished[stage] = true
  end

  if edges[entry] ~= nil then
    walk(entry)
  end
  return back
end

---@param names string[]
---@return string[]
local function sorted(names)
  table.sort(names)
  return names
end

---@param stages table<string, table>
---@return string[]
local function stage_names(stages)
  local names = {}
  for name in pairs(stages) do
    names[#names + 1] = name
  end
  return sorted(names)
end

---Validate one workflow, identified by name, against the manifest of discovered stage blocks.
---
---Deliberately takes no options. There is no flag, and no place to add one, that lets a rejected
---definition run: the moment such an argument exists it becomes what every caller reaches for when
---validation is inconvenient, and every rejection here degrades into a warning with extra steps.
---The escape valve for a workflow you disagree with is editing the document.
---@param workflow string
---@param manifest table<string, table>
---@return louiselm.workflow.Result
function M.validate(workflow, manifest)
  ---@type louiselm.workflow.Rejection[]
  local rejections = {}

  ---@type table<string, table>
  local stages = {}
  for name, block in pairs(manifest) do
    if type(block) == "table" and block.workflow == workflow then
      stages[name] = block
    end
  end

  local names = stage_names(stages)

  local entries = {}
  local has_automatic_generator = false
  for _, name in ipairs(names) do
    local block = stages[name]
    check_fields(rejections, block, Schema.STAGE_FIELDS, name)
    if block.entry == true then
      entries[#entries + 1] = name
    end
    if type(block.generates) == "table" then
      has_automatic_generator = true
      check_fields(rejections, block.generates, Schema.GENERATES_FIELDS, name)
      if not is_literal_count(block.generates.max) then
        reject(rejections, {
          reason = "unbounded_generator",
          message = string.format("stage '%s' generates work and must declare a literal integer 'max'", name),
          stage = name,
        })
      end
    end
    if type(block["generated-work"]) == "table" then
      check_fields(rejections, block["generated-work"], Schema.GENERATED_WORK_FIELDS, name)
    end
    if block["generated-work"] ~= nil and block.entry ~= true then
      reject(rejections, {
        reason = "misplaced_run_budget",
        message = string.format("stage '%s' declares the Run budget but is not the entry stage", name),
        stage = name,
        field = "generated-work",
      })
    end
    local outcomes = is_array(block.outcomes) and block.outcomes or {}
    for _, outcome in ipairs(outcomes) do
      if type(outcome) == "table" and outcome["back-edge"] == true then
        local resolver = type(outcome.resolver) == "string" and outcome.resolver or Schema.DEFAULT_RESOLVER
        if Schema.AUTOMATIC_RESOLVERS[resolver] == true then
          has_automatic_generator = true
        end
      end
    end
  end

  if #entries == 0 then
    reject(rejections, {
      reason = "missing_entry",
      message = string.format("workflow '%s' declares no entry stage", workflow),
    })
  elseif #entries > 1 then
    reject(rejections, {
      reason = "ambiguous_entry",
      message = string.format(
        "workflow '%s' declares %d entry stages: %s",
        workflow,
        #entries,
        table.concat(sorted(entries), ", ")
      ),
    })
  end

  local generated_work_max
  if #entries == 1 then
    local budget = stages[entries[1]]["generated-work"]
    if type(budget) == "table" and is_literal_count(budget.max) then
      generated_work_max = budget.max
    elseif has_automatic_generator then
      reject(rejections, {
        reason = "unbounded_run",
        message = string.format(
          "entry stage '%s' must declare a literal positive 'generated-work.max' for this workflow",
          entries[1]
        ),
        stage = entries[1],
        field = "generated-work",
      })
    end
  end

  local edges = build_edges(stages)
  local reaches_stage = reverse(edges)
  local back_edges = #entries == 1 and classify_back_edges(edges, entries[1]) or {}

  for _, name in ipairs(names) do
    local block = stages[name]
    local outcomes = is_array(block.outcomes) and block.outcomes or {}
    local seen_names = {}
    local by_name = {}

    for _, outcome in ipairs(outcomes) do
      if type(outcome) == "table" and type(outcome.name) == "string" then
        by_name[outcome.name] = outcome
      end
    end

    for index, outcome in ipairs(outcomes) do
      if type(outcome) ~= "table" then
        reject(rejections, {
          reason = "wrong_field_type",
          message = string.format("stage '%s' outcome %d must be a table", name, index),
          stage = name,
          field = "outcomes",
        })
      else
        local label = type(outcome.name) == "string" and outcome.name or tostring(index)
        check_fields(rejections, outcome, Schema.OUTCOME_FIELDS, name, label)

        if type(outcome.name) ~= "string" or outcome.name == "" then
          reject(rejections, {
            reason = "unnamed_outcome",
            message = string.format("stage '%s' outcome %d has no name", name, index),
            stage = name,
          })
        elseif seen_names[outcome.name] then
          reject(rejections, {
            reason = "duplicate_outcome",
            message = string.format("stage '%s' declares outcome '%s' more than once", name, outcome.name),
            stage = name,
            outcome = outcome.name,
          })
        else
          seen_names[outcome.name] = true
        end

        local terminal = outcome.terminal == true
        if not terminal and type(outcome.to) ~= "string" then
          reject(rejections, {
            reason = "unmapped_outcome",
            message = string.format("stage '%s' outcome '%s' names no target and is not terminal", name, label),
            stage = name,
            outcome = label,
          })
        elseif not terminal and stages[outcome.to] == nil then
          reject(rejections, {
            reason = "unresolved_target",
            message = string.format(
              "stage '%s' outcome '%s' targets '%s', which is not a stage of workflow '%s'",
              name,
              label,
              outcome.to,
              workflow
            ),
            stage = name,
            outcome = label,
            target = outcome.to,
          })
        elseif not terminal then
          local resolver = type(outcome.resolver) == "string" and outcome.resolver or Schema.DEFAULT_RESOLVER
          local automatic = Schema.AUTOMATIC_RESOLVERS[resolver] == true
          local declared = outcome["back-edge"] == true
          local returns_to_ancestor = back_edges[name] ~= nil and back_edges[name][outcome.to] == true

          if returns_to_ancestor and not declared then
            reject(rejections, {
              reason = "undeclared_back_edge",
              message = string.format(
                "stage '%s' outcome '%s' returns to '%s' and must declare back-edge: true",
                name,
                label,
                outcome.to
              ),
              stage = name,
              outcome = label,
              target = outcome.to,
            })
          end

          if declared and automatic then
            if not is_literal_count(outcome["max-iterations"]) then
              reject(rejections, {
                reason = "unbounded_generator",
                message = string.format(
                  "stage '%s' outcome '%s' is a back edge resolved by '%s' and must declare a "
                    .. "literal integer 'max-iterations'",
                  name,
                  label,
                  resolver
                ),
                stage = name,
                outcome = label,
              })
            end

            local exhausted = outcome["on-exhausted"]
            if type(exhausted) ~= "string" then
              reject(rejections, {
                reason = "missing_exhaustion_outcome",
                message = string.format(
                  "stage '%s' outcome '%s' is bounded but declares no 'on-exhausted' outcome, so "
                    .. "the last iteration has nowhere to go",
                  name,
                  label
                ),
                stage = name,
                outcome = label,
              })
            elseif by_name[exhausted] == nil then
              reject(rejections, {
                reason = "unresolved_target",
                message = string.format(
                  "stage '%s' outcome '%s' exhausts to '%s', which is not an outcome of this stage",
                  name,
                  label,
                  exhausted
                ),
                stage = name,
                outcome = label,
                target = exhausted,
              })
            else
              -- The loop is every stage the back edge can re-enter and return from: reachable from
              -- the target, and able to reach the source. Exhausting into it means the bound
              -- counts to its limit and hands control back to the same cycle.
              local forward = reachable_from(edges, { outcome.to })
              local backward = reachable_from(reaches_stage, { name })
              local escape = by_name[exhausted]
              local escape_to = escape.terminal ~= true and escape.to or nil
              if type(escape_to) == "string" and forward[escape_to] and backward[escape_to] then
                reject(rejections, {
                  reason = "exhaustion_inside_loop",
                  message = string.format(
                    "stage '%s' outcome '%s' exhausts to '%s', which is inside the loop it exits",
                    name,
                    label,
                    escape_to
                  ),
                  stage = name,
                  outcome = label,
                  target = escape_to,
                })
              end
            end
          end
        end
      end
    end
  end

  local terminal_stages = {}
  for _, name in ipairs(names) do
    local outcomes = is_array(stages[name].outcomes) and stages[name].outcomes or {}
    for _, outcome in ipairs(outcomes) do
      if type(outcome) == "table" and outcome.terminal == true then
        terminal_stages[#terminal_stages + 1] = name
        break
      end
    end
  end

  local reaches_terminal = reachable_from(reaches_stage, terminal_stages)
  for _, name in ipairs(names) do
    if not reaches_terminal[name] then
      reject(rejections, {
        reason = "no_reachable_terminal",
        message = string.format("stage '%s' can never reach a terminal outcome", name),
        stage = name,
      })
    end
  end

  if #entries == 1 then
    local reached = reachable_from(edges, { entries[1] })
    for _, name in ipairs(names) do
      if not reached[name] then
        reject(rejections, {
          reason = "unreachable_stage",
          message = string.format(
            "stage '%s' claims workflow '%s' but entry '%s' cannot reach it",
            name,
            workflow,
            entries[1]
          ),
          stage = name,
        })
      end
    end
  end

  return {
    ok = #rejections == 0,
    rejections = rejections,
    generated_work_max = #rejections == 0 and generated_work_max or nil,
  }
end

return M
