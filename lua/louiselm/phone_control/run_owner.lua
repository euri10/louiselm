---Explicit live Run ownership, independent of the durable Run projection.
---@diagnostic disable-next-line: undefined-global -- Neovim supplies table helpers.
local nvim = vim
local Service = require("louiselm.workflow.service")
local M = {}
local Owner = {}
Owner.__index = Owner

local statuses = {
  starting = true,
  ready = true,
  admitting = true,
  configuring = true,
  preparing = true,
  prompting = true,
  running = true,
  waiting_permission = true,
  cancelling = true,
  error = true,
}
local phases = {
  authorizing = true,
  preparing = true,
  working = true,
  verifying = true,
  awaiting_promotion = true,
  promoting = true,
}
local run_statuses = { new = true, active = true, held = true, completed = true, failed = true }

local function hex_id(value)
  return type(value) == "string" and #value == 32 and value ~= string.rep("0", 32) and value:match("^[0-9a-f]+$")
end

local function run_id(value)
  return type(value) == "string"
    and #value == 36
    and value:match("^[0-9a-f]+%-[0-9a-f]+%-[0-9a-f]+%-[0-9a-f]+%-[0-9a-f]+$") ~= nil
    and value:sub(9, 9) == "-"
    and value:sub(14, 14) == "-"
    and value:sub(19, 19) == "-"
    and value:sub(24, 24) == "-"
end

M.valid_run_id = run_id

---Register one exact operator-owned controller, without starting it.
---@param executor louiselm.workflow.BeadExecutor
---@return boolean tracked
---@return string? error_message
function Owner:track(executor)
  if type(executor) ~= "table" or type(executor.options) ~= "table" or type(executor.run) ~= "table" then
    return false, "invalid Run owner"
  end
  local options, run = executor.options, executor.run
  if
    type(options.envelope) ~= "table"
    or not run_id(run.id)
    or options.envelope.run_id ~= run.id
    or type(options.agent_id) ~= "string"
    or options.agent_id == ""
    or not Service.validate_prepared(options.prepared)
    or options.prepared == nil
    or type(options.bead_ids) ~= "table"
    or not nvim.islist(options.bead_ids)
    or #options.bead_ids < 1
    or #options.bead_ids > 4096
    or type(run.workers) ~= "table"
    or not run_statuses[executor.status]
  then
    return false, "invalid registered Run owner"
  end
  if self.runs[run.id] ~= nil then
    return false, "Run already tracked"
  end
  for _, bead_id in ipairs(options.bead_ids) do
    if type(bead_id) ~= "string" or bead_id == "" or #bead_id > 512 or bead_id:find("\0", 1, true) then
      return false, "invalid registered Bead identity"
    end
  end
  self.runs[run.id] = {
    executor = executor,
    run = run,
    prepared = nvim.deepcopy(options.prepared),
    agent_id = options.agent_id,
    bead_ids = nvim.deepcopy(options.bead_ids),
  }
  return true
end

---Release only the exact registered object, never a replacement with the same UUID.
---@param executor louiselm.workflow.BeadExecutor
function Owner:untrack(executor)
  for id, owner in pairs(self.runs) do
    if owner.executor == executor then
      self.runs[id] = nil
      return
    end
  end
end

---Return bounded data from this live incarnation; no Session is resolved by ACP ID.
---@param id string Canonical Run UUID.
---@param controller_id string Current receiver-issued connection identity.
---@param subject fun(session: louiselm.session.Session): table? Resolve an exact tracked object.
---@return table? snapshot Nil when absent or invalidated.
---@return string? error_code Invalidated or unavailable ownership.
function Owner:snapshot(id, controller_id, subject)
  local saved = self.runs[id]
  if saved == nil then
    return nil
  end
  local executor, run, options = saved.executor, saved.run, saved.executor.options
  if
    not hex_id(controller_id)
    or type(subject) ~= "function"
    or executor.run ~= run
    or run.id ~= id
    or type(options) ~= "table"
    or type(options.envelope) ~= "table"
    or options.envelope.run_id ~= id
    or options.agent_id ~= saved.agent_id
    or not nvim.deep_equal(options.prepared, saved.prepared)
    or not nvim.deep_equal(options.bead_ids, saved.bead_ids)
    or not run_statuses[executor.status]
    or type(run.workers) ~= "table"
    or #run.workers > 4096
  then
    return nil, "stale_subject"
  end
  local phase, bead_index, bead_id = "none", 0, ""
  local progress = executor.current_progress
  if progress ~= nil and executor.status == "active" then
    if
      type(progress) ~= "table"
      or not phases[progress.phase]
      or type(progress.index) ~= "number"
      or progress.index % 1 ~= 0
      or progress.index < 1
      or progress.index > #saved.bead_ids
      or progress.total ~= #saved.bead_ids
      or progress.bead_id ~= saved.bead_ids[progress.index]
    then
      return nil, "unavailable"
    end
    phase, bead_index, bead_id = progress.phase, progress.index, progress.bead_id
  end
  local sessions, truncated = {}, false
  for _, session in ipairs(run.workers) do
    if type(session) == "table" and session.owner_run == run and type(session.inspect) == "function" then
      local state = session:inspect()
      if
        type(state) == "table"
        and type(session.owner) == "table"
        and type(session.owner.sessions) == "table"
        and session.owner.sessions[state.id] == session
        and state.status ~= "disposed"
      then
        local identity = subject(session)
        if type(identity) == "table" and identity.controller_id == controller_id and hex_id(identity.subject_id) then
          if state.agent ~= saved.agent_id or not statuses[state.status] then
            return nil, "unavailable"
          end
          if #sessions == 50 then
            truncated = true
            break
          end
          sessions[#sessions + 1] = { subject_id = identity.subject_id, agent = state.agent, status = state.status }
        end
      end
    end
  end
  return {
    run_id = id,
    registration_id = saved.prepared.registration_id,
    registration_revision = saved.prepared.revision,
    admitted = executor.phone_admitted == true,
    status = executor.status,
    phase = phase,
    bead_index = bead_index,
    bead_total = #saved.bead_ids,
    bead_id = bead_id,
    sessions = sessions,
    sessions_truncated = truncated,
  }
end

---Create an independent owner; callers own and release all registrations.
---@return table owner
function M.new()
  return setmetatable({ runs = {} }, Owner)
end

return M
