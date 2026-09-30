local PrivateFile = require("louiselm.private_file")

---@class louiselm.routing.ComparisonRoute
---@field agent string Configured Agent.
---@field provider string Resolved Provider, not the Model manufacturer.
---@field model string Advertised Model selection.
---@field model_option_id string ID of the advertised Model option.
---@field options table<string, string|boolean> Complete supported option tuple, including the Model option.

---@class louiselm.routing.ComparisonWorkload
---@field kind "main"|"reader" Main submission or selected-content reader.
---@field id string Exact workload identifier used by admission.

---@class louiselm.routing.ComparisonCheck
---@field id string Acceptance check identifier.
---@field baseline "pass"|"fail"|"pending"
---@field candidate "pass"|"fail"|"pending"

---@class louiselm.routing.ComparisonFixture
---@field id string Explicitly selected fixture identifier.
---@field source string Payload-free provenance of the selected input.
---@field digest string Digest of the selected fixture or evidence.
---@field checks louiselm.routing.ComparisonCheck[] At least one result beyond operational completion.
---@field human_review_required boolean Whether this fixture needs human assessment.
---@field human? { baseline: "pass"|"fail"|"pending", candidate: "pass"|"fail"|"pending", reviewer: string } Explicit human result when required.

---@class louiselm.routing.ComparisonReport
---@field version integer Exactly 1.
---@field id string Selected report identifier.
---@field policy_revision string Applicable policy/evidence version.
---@field workload louiselm.routing.ComparisonWorkload
---@field baseline louiselm.routing.ComparisonRoute
---@field candidate louiselm.routing.ComparisonRoute
---@field fixtures louiselm.routing.ComparisonFixture[] Selected evidence and acceptance results.

---@class louiselm.routing.ComparisonEconomics
---@field kind "estimated"|"measured" Estimate or observation; never a factual usage row.
---@field metric "api_cost"|"quota"|"latency"
---@field value number Nonnegative quantity.
---@field unit string ISO currency for API cost, otherwise the named unit.
---@field provenance string Operator-supplied source of this figure.

---@class louiselm.routing.ApiForQuotaAllowance
---@field max_extra_cost { value: number, currency: string } Maximum additional API spend in the named currency.
---@field provenance string Explicit operator allowance source.

---@class louiselm.routing.ComparisonDecisionInput
---@field action "approve"|"reject" Explicit operator decision.
---@field report louiselm.routing.ComparisonReport Selected existing report, never inferred from feedback.
---@field economics? louiselm.routing.ComparisonEconomics[] Optional planning figures, kept apart from factual usage.
---@field api_for_quota? louiselm.routing.ApiForQuotaAllowance Separate permission for additional API spending.

---@class louiselm.routing.QualificationResult
---@field revision integer Persistent approval revision for admission revalidation.
---@field report_id string
---@field policy_revision string
---@field report louiselm.routing.ComparisonReport Validated, detached comparison and provenance.
---@field economics? louiselm.routing.ComparisonEconomics[]
---@field api_for_quota? louiselm.routing.ApiForQuotaAllowance

---@class louiselm.routing.Qualification
---@field path string
---@field read fun(path: string, callback: fun(state?: table, error_message?: string))
---@field write fun(path: string, state: table, callback: fun(ok: boolean, error_message?: string))
---@field busy boolean
---@field decide fun(self: louiselm.routing.Qualification, input: unknown, callback: fun(result?: louiselm.routing.QualificationResult, error_message?: string)) Persist an explicit decision before publishing it.
---@field lookup fun(self: louiselm.routing.Qualification, scope: unknown, expected_revision: integer?, callback: fun(result?: louiselm.routing.QualificationResult, error_message?: string)) Return only a matching approved report at the expected revision.
---@field revision fun(self: louiselm.routing.Qualification, callback: fun(revision?: integer, error_message?: string)) Read the durable revision.

local M = {}
local Qualification = {}
Qualification.__index = Qualification
local VERSION = 1

---@return table
local function nvim()
  ---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
  return vim
end

local function nonempty(value)
  return type(value) == "string" and value ~= ""
end

local function exact(value, keys)
  if type(value) ~= "table" then
    return false
  end
  for key in pairs(value) do
    if not keys[key] then
      return false
    end
  end
  return true
end

local function array(value, minimum)
  if type(value) ~= "table" or #value < minimum then
    return false
  end
  local count = 0
  for key in pairs(value) do
    if type(key) ~= "number" or key % 1 ~= 0 or key < 1 or key > #value then
      return false
    end
    count = count + 1
  end
  return count == #value
end

local function status(value)
  return value == "pass" or value == "fail" or value == "pending"
end

local function route(value)
  if
    not exact(value, { agent = true, provider = true, model = true, model_option_id = true, options = true })
    or not nonempty(value.agent)
    or not nonempty(value.provider)
    or not nonempty(value.model)
    or not nonempty(value.model_option_id)
    or type(value.options) ~= "table"
    or value.options[value.model_option_id] ~= value.model
  then
    return false
  end
  for key, option in pairs(value.options) do
    if not nonempty(key) or (type(option) ~= "string" and type(option) ~= "boolean") then
      return false
    end
  end
  return true
end

local function workload(value)
  return exact(value, { kind = true, id = true })
    and (value.kind == "main" or value.kind == "reader")
    and nonempty(value.id)
end

local function fixture(value)
  if
    not exact(
      value,
      { id = true, source = true, digest = true, checks = true, human_review_required = true, human = true }
    )
    or not nonempty(value.id)
    or not nonempty(value.source)
    or type(value.digest) ~= "string"
    or #value.digest ~= 71
    or not value.digest:match("^sha256:[0-9a-f]+$")
    or not array(value.checks, 1)
    or type(value.human_review_required) ~= "boolean"
  then
    return false
  end
  for _, check in ipairs(value.checks) do
    if
      not exact(check, { id = true, baseline = true, candidate = true })
      or not nonempty(check.id)
      or not status(check.baseline)
      or not status(check.candidate)
    then
      return false
    end
  end
  if value.human_review_required then
    if value.human == nil then
      return true
    end
    return exact(value.human, { baseline = true, candidate = true, reviewer = true })
      and status(value.human.baseline)
      and status(value.human.candidate)
      and nonempty(value.human.reviewer)
  end
  return value.human == nil
end

local function report(value)
  if
    not exact(value, {
      version = true,
      id = true,
      policy_revision = true,
      workload = true,
      baseline = true,
      candidate = true,
      fixtures = true,
    })
    or value.version ~= VERSION
    or not nonempty(value.id)
    or not nonempty(value.policy_revision)
    or not workload(value.workload)
    or not route(value.baseline)
    or not route(value.candidate)
    or nvim().deep_equal(value.baseline, value.candidate)
    or not array(value.fixtures, 1)
  then
    return false
  end
  local seen = {}
  for _, selected in ipairs(value.fixtures) do
    if not fixture(selected) or seen[selected.id] then
      return false
    end
    seen[selected.id] = true
  end
  return true
end

local function accepted(value)
  for _, selected in ipairs(value.fixtures) do
    for _, check in ipairs(selected.checks) do
      if check.baseline == "pending" or check.candidate ~= "pass" then
        return false
      end
    end
    if
      selected.human_review_required
      and (selected.human == nil or selected.human.baseline == "pending" or selected.human.candidate ~= "pass")
    then
      return false
    end
  end
  return true
end

local function economics(value)
  if value == nil then
    return true
  end
  if not array(value, 1) then
    return false
  end
  for _, entry in ipairs(value) do
    if
      not exact(entry, { kind = true, metric = true, value = true, unit = true, provenance = true })
      or (entry.kind ~= "estimated" and entry.kind ~= "measured")
      or (entry.metric ~= "api_cost" and entry.metric ~= "quota" and entry.metric ~= "latency")
      or type(entry.value) ~= "number"
      or entry.value < 0
      or entry.value == math.huge
      or entry.value ~= entry.value
      or not nonempty(entry.unit)
      or not nonempty(entry.provenance)
      or (entry.metric == "api_cost" and not entry.unit:match("^[A-Z][A-Z][A-Z]$"))
    then
      return false
    end
  end
  return true
end

local function allowance(value)
  if value == nil then
    return true
  end
  if
    not exact(value, { max_extra_cost = true, provenance = true })
    or not exact(value.max_extra_cost, { value = true, currency = true })
    or not nonempty(value.provenance)
  then
    return false
  end
  local cost = value.max_extra_cost
  return type(cost.value) == "number"
    and cost.value >= 0
    and cost.value < math.huge
    and cost.value == cost.value
    and type(cost.currency) == "string"
    and cost.currency:match("^[A-Z][A-Z][A-Z]$") ~= nil
end

local function valid_decision(value, revision)
  return exact(
    value,
    { action = true, report = true, economics = true, api_for_quota = true, revision = true, decided_at = true }
  ) and (value.action == "approve" or value.action == "reject") and report(value.report) and (value.action ~= "approve" or accepted(
    value.report
  )) and economics(value.economics) and allowance(value.api_for_quota) and value.revision == revision and type(
    value.decided_at
  ) == "number" and value.decided_at % 1 == 0
end

local function empty_state()
  return { version = VERSION, revision = 0, decisions = {} }
end

local function read_state(path)
  local editor = nvim()
  local stat = editor.uv.fs_stat(path)
  if stat == nil then
    return empty_state(), nil
  end
  if stat.type ~= "file" then
    return nil, "comparison approval path is not a regular file"
  end
  local file, open_error = editor.uv.fs_open(path, "r", 384)
  if file == nil then
    return nil, "could not read comparison approvals: " .. tostring(open_error)
  end
  local content, read_error = editor.uv.fs_read(file, stat.size, 0)
  local closed, close_error = editor.uv.fs_close(file)
  if content == nil then
    return nil, "could not read comparison approvals: " .. tostring(read_error)
  end
  if not closed then
    return nil, "could not close comparison approvals: " .. tostring(close_error)
  end
  local decoded_ok, decoded = pcall(editor.json.decode, content)
  if
    not decoded_ok
    or not exact(decoded, { version = true, revision = true, decisions = true })
    or decoded.version ~= VERSION
    or not array(decoded.decisions, 0)
    or decoded.revision ~= #decoded.decisions
  then
    return nil, "comparison approvals have invalid schema"
  end
  for index, decision in ipairs(decoded.decisions) do
    if not valid_decision(decision, index) then
      return nil, "comparison approvals have invalid schema"
    end
  end
  return decoded, nil
end

local function write_state(path, state)
  local editor = nvim()
  local directory = editor.fs.dirname(path)
  if editor.fn.mkdir(directory, "p", 448) == 0 and editor.fn.isdirectory(directory) ~= 1 then
    return false, "could not create comparison approval directory"
  end
  local encoded_ok, content = pcall(editor.json.encode, state)
  if not encoded_ok then
    return false, "could not encode comparison approvals"
  end
  return PrivateFile.write(editor.uv, path, content, "comparison approvals", "replace")
end

local function default_read(path, callback)
  nvim().schedule(function()
    callback(read_state(path))
  end)
end

local function default_write(path, state, callback)
  nvim().schedule(function()
    callback(write_state(path, state))
  end)
end

local function result(decision)
  return nvim().deepcopy({
    revision = decision.revision,
    report_id = decision.report.id,
    policy_revision = decision.report.policy_revision,
    report = decision.report,
    economics = decision.economics,
    api_for_quota = decision.api_for_quota,
  })
end

local function refuse(callback, message)
  nvim().schedule(function()
    callback(nil, message)
  end)
end

---Open the private, payload-free comparison approval record.
---@param path string Persistent JSON path.
---@param io? { read?: fun(path: string, callback: fun(state?: table, error_message?: string)), write?: fun(path: string, state: table, callback: fun(ok: boolean, error_message?: string)) } Callback-shaped filesystem double for tests.
---@return louiselm.routing.Qualification? store
---@return string? error_message
function M.new(path, io)
  if not nonempty(path) then
    return nil, "comparison approval path must be a non-empty string"
  end
  io = io or {}
  return setmetatable({
    path = nvim().fs.normalize(path),
    read = io.read or default_read,
    write = io.write or default_write,
    busy = false,
  }, Qualification),
    nil
end

---Persist one explicit operator approval or rejection of a selected report.
---@param self louiselm.routing.Qualification
---@param input unknown louiselm.routing.ComparisonDecisionInput
---@param callback fun(result?: louiselm.routing.QualificationResult, error_message?: string)
function Qualification:decide(input, callback)
  if
    not exact(input, { action = true, report = true, economics = true, api_for_quota = true })
    or (input.action ~= "approve" and input.action ~= "reject")
  then
    refuse(callback, "comparison decision must explicitly approve or reject")
    return
  end
  if not report(input.report) then
    refuse(callback, "comparison report has invalid schema")
    return
  end
  if input.action == "approve" and not accepted(input.report) then
    refuse(callback, "comparison acceptance is incomplete or failed")
    return
  end
  if not economics(input.economics) or not allowance(input.api_for_quota) then
    refuse(callback, "comparison economics or allowance has invalid schema")
    return
  end
  if self.busy then
    refuse(callback, "comparison approval write is in progress")
    return
  end
  self.busy = true
  self.read(self.path, function(state, read_error)
    if state == nil then
      self.busy = false
      callback(nil, read_error)
      return
    end
    local decision = nvim().deepcopy(input)
    decision.revision = state.revision + 1
    decision.decided_at = os.time()
    state.decisions[#state.decisions + 1] = decision
    state.revision = decision.revision
    self.write(self.path, state, function(written, write_error)
      self.busy = false
      if not written then
        callback(nil, write_error)
        return
      end
      callback(result(decision), nil)
    end)
  end)
end

---Look up a matching approved pair/workload against the current durable revision.
---@param self louiselm.routing.Qualification
---@param scope unknown { workload: louiselm.routing.ComparisonWorkload, baseline: louiselm.routing.ComparisonRoute, candidate: louiselm.routing.ComparisonRoute, policy_revision: string }
---@param expected_revision integer? Revision captured during pending admission.
---@param callback fun(result?: louiselm.routing.QualificationResult, error_message?: string)
function Qualification:lookup(scope, expected_revision, callback)
  if
    not exact(scope, { workload = true, baseline = true, candidate = true, policy_revision = true })
    or not workload(scope.workload)
    or not route(scope.baseline)
    or not route(scope.candidate)
    or not nonempty(scope.policy_revision)
  then
    refuse(callback, "qualification scope has invalid schema")
    return
  end
  self.read(self.path, function(state, read_error)
    if state == nil then
      callback(nil, read_error)
      return
    end
    if expected_revision ~= nil and expected_revision ~= state.revision then
      callback(nil, "approval revision changed")
      return
    end
    for index = #state.decisions, 1, -1 do
      local decision = state.decisions[index]
      local selected = decision.report
      if
        nvim().deep_equal(selected.workload, scope.workload)
        and nvim().deep_equal(selected.baseline, scope.baseline)
        and nvim().deep_equal(selected.candidate, scope.candidate)
        and selected.policy_revision == scope.policy_revision
      then
        if decision.action == "approve" then
          callback(result(decision), nil)
        else
          callback(nil, nil)
        end
        return
      end
    end
    callback(nil, nil)
  end)
end

---Read the current approval revision for admission's final provenance check.
---@param self louiselm.routing.Qualification
---@param callback fun(revision?: integer, error_message?: string)
function Qualification:revision(callback)
  self.read(self.path, function(state, read_error)
    callback(state and state.revision or nil, read_error)
  end)
end

return M
