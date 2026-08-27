---Reconcile authoritative Run snapshots into local Park lifecycle and presentation.

local M = {}
local Observer = {}
Observer.__index = Observer

---@class louiselm.workflow.ParkPresentation
---@field id string
---@field revision integer
---@field state "parked"|"cold_parked"
---@field ceiling integer
---@field consumed integer
---@field reserved integer
---@field pending_mutation_ids string[]
---@field triggering_mutation_id? string
---@field expires_at_ms integer

---@class louiselm.workflow.ParkObserverOptions
---@field find_run fun(id: string): louiselm.workflow.Run?
---@field on_park fun(presentation: louiselm.workflow.ParkPresentation)

---@class louiselm.workflow.ParkObserver
---@field find_run fun(id: string): louiselm.workflow.Run?
---@field on_park fun(presentation: louiselm.workflow.ParkPresentation)
---@field revisions table<string, integer>
---@field disposed boolean
---@field observe fun(self: louiselm.workflow.ParkObserver, runs: louiselm.workflow.RunView[]): boolean, string?
---@field dispose fun(self: louiselm.workflow.ParkObserver): boolean

local function is_non_negative_integer(value)
  return type(value) == "number" and value >= 0 and value % 1 == 0
end

local function presentation(run)
  if
    type(run.id) ~= "string"
    or run.id == ""
    or not is_non_negative_integer(run.revision)
    or not is_non_negative_integer(run.generated_work_ceiling)
    or not is_non_negative_integer(run.generated_work_consumed)
    or not is_non_negative_integer(run.generated_work_reserved)
    or type(run.pending_mutation_ids) ~= "table"
    or not is_non_negative_integer(run.park_expires_at_ms)
  then
    return nil, "Park snapshot is malformed"
  end
  local pending = {}
  for index, mutation_id in ipairs(run.pending_mutation_ids) do
    if type(mutation_id) ~= "string" or mutation_id == "" then
      return nil, "Park snapshot is malformed"
    end
    pending[index] = mutation_id
  end
  if
    run.triggering_mutation_id ~= nil
    and (type(run.triggering_mutation_id) ~= "string" or run.triggering_mutation_id == "")
  then
    return nil, "Park snapshot is malformed"
  end
  return {
    id = run.id,
    revision = run.revision,
    state = run.state,
    ceiling = run.generated_work_ceiling,
    consumed = run.generated_work_consumed,
    reserved = run.generated_work_reserved,
    pending_mutation_ids = pending,
    triggering_mutation_id = run.triggering_mutation_id,
    expires_at_ms = run.park_expires_at_ms,
  }
end

---Create an explicit owner for Park reconciliation state.
---@param options louiselm.workflow.ParkObserverOptions
---@return louiselm.workflow.ParkObserver? observer
---@return string? error_message
function M.new(options)
  if type(options) ~= "table" or type(options.find_run) ~= "function" or type(options.on_park) ~= "function" then
    return nil, "Park observer requires find_run and on_park callbacks"
  end
  local observer = setmetatable({
    find_run = options.find_run,
    on_park = options.on_park,
    revisions = {},
    disposed = false,
  }, Observer)
  ---@cast observer louiselm.workflow.ParkObserver
  return observer
end

---Apply only newer authoritative Park snapshots. The caller owns the main-loop boundary.
---@param self louiselm.workflow.ParkObserver
---@param runs louiselm.workflow.RunView[]
---@return boolean observed
---@return string? error_message
function Observer:observe(runs)
  if self.disposed then
    return false, "Park observer is disposed"
  end
  if type(runs) ~= "table" then
    return false, "Run snapshot must be an array"
  end
  for _, run in ipairs(runs) do
    if type(run) ~= "table" or type(run.id) ~= "string" or not is_non_negative_integer(run.revision) then
      return false, "Run snapshot is malformed"
    end
    local previous = self.revisions[run.id] or 0
    self.revisions[run.id] = math.max(previous, run.revision)
    if run.revision > previous and (run.state == "parked" or run.state == "cold_parked") then
      local value, value_error = presentation(run)
      if value == nil then
        return false, value_error
      end
      local live = self.find_run(run.id)
      if live ~= nil then
        local parked, park_error = live:accept_park()
        if not parked then
          return false, park_error
        end
      end
      self.on_park(value)
    end
  end
  return true
end

---Dispose the observer so late snapshots cannot affect local lifecycle or presentation.
---@param self louiselm.workflow.ParkObserver
---@return boolean disposed
function Observer:dispose()
  self.disposed = true
  return true
end

return M
