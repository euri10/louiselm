---Native admission of one exact phone-authored Session instruction.

---@diagnostic disable-next-line: undefined-global -- Neovim owns scheduling and SHA-256.
local nvim = vim
local ActionPhases = require("louiselm.phone_control.action_phases")
local M = {}
local Action = {}
Action.__index = Action

---@class louiselm.phone_control.ActionOptions
---@field journal table
---@field generation string? Receiver generation bound to this controller.
---@field session louiselm.session.Session
---@field projection table
---@field current fun(): boolean
---@field authorize fun(done: fun(permit: table))

local function failure(code)
  return { v = 1, type = "error", code = code }
end

local function release(self, permit)
  if permit == nil then
    return true
  end
  if type(permit) ~= "table" or type(permit.release) ~= "function" then
    return false
  end
  if permit.released then
    return true
  end
  permit.released = true
  local ok, cause = pcall(permit.release)
  if not ok then
    self.last_error = cause
  end
  if self.permit == permit then
    self.permit = nil
  end
  return ok
end

---Reserve, authorize and submit one explicit text-only prompt.
---@param identity table Payload-free identity already bound to the signed request and native scope.
---@param text string Exact phone-authored text.
---@param callback fun(result: table) Called at most once on the main loop.
function Action:send(identity, text, callback)
  if self.disposed or self.pending then
    nvim.schedule(function()
      if not self.disposed then
        callback(failure("unavailable"))
      end
    end)
    return
  end
  if type(self.generation) ~= "string" or #self.generation ~= 32 or self.generation:match("^[0-9a-f]+$") == nil then
    nvim.schedule(function()
      if not self.disposed then
        callback(failure("unavailable"))
      end
    end)
    return
  end
  if
    type(identity) ~= "table"
    or identity.action ~= 3
    or type(identity.operation_id) ~= "string"
    or type(identity.payload_hash) ~= "string"
    or type(identity.expected_revision) ~= "number"
    or identity.expected_revision <= 0
    or type(text) ~= "string"
    or text == ""
    or #text > 4095
    or text:find("\0", 1, true) ~= nil
    or nvim.fn.sha256(string.char(1) .. text) ~= identity.payload_hash
  then
    nvim.schedule(function()
      if not self.disposed then
        callback(failure("malformed"))
      end
    end)
    return
  end
  self.pending = true
  local finished, began, authorized = false, false, false
  local reserved, detached, transitioning, reporting, admitted = false, false, false, false, false
  local detach_callback
  local function finish(result)
    if finished or self.disposed then
      return
    end
    finished = true
    self.pending = false
    self.detach_action = nil
    if detached then
      detach_callback(result)
    else
      callback(result)
    end
  end
  local function report(state, code, turn_id)
    reporting = true
    local evidence =
      nvim.fn.sha256("louiselm-phone-owner-" .. state .. "-v1:" .. identity.operation_id .. ":" .. (turn_id or code))
    self.journal:run({
      v = 1,
      type = "report",
      identity = identity,
      outcome = { [state] = { evidence = evidence } },
      at_ms = os.time() * 1000,
    }, function(result)
      if result.type ~= "recorded" then
        if detached and state == "fenced_no_effect" and not self.disposed then
          reporting = false
          local done = detach_callback
          detach_callback = nil
          done(failure("unavailable"))
        else
          finish(failure("unavailable"))
        end
      else
        finish({
          v = 1,
          type = "action_result",
          operation_id = identity.operation_id,
          state = state,
          code = code,
          evidence = evidence,
        })
      end
    end)
  end
  self.detach_action = function(done)
    if detached then
      if detach_callback == nil and reserved and not transitioning and not reporting and not admitted then
        detach_callback = done
        report("fenced_no_effect", "unavailable")
      else
        nvim.schedule(function()
          done(failure("unavailable"))
        end)
      end
      return
    end
    detached, detach_callback = true, done
    if reserved and not transitioning and not reporting and not admitted then
      report("fenced_no_effect", "unavailable")
    elseif began and not reserved and not reporting then
      finish(failure("unavailable"))
    end
  end
  self.journal:run({ v = 2, type = "begin", generation = self.generation, identity = identity }, function(result)
    if began or self.disposed then
      return
    end
    began = true
    if result.type == "existing" then
      finish(result)
      return
    elseif result.type ~= "begun" then
      finish(failure(result.code or "unavailable"))
      return
    end
    reserved = true
    if detached then
      report("fenced_no_effect", "unavailable")
      return
    end
    self.authorize(function(permit)
      if authorized or self.disposed or detached then
        release(self, permit)
        return
      end
      authorized = true
      self.permit = permit
      if type(permit) ~= "table" or type(permit.valid) ~= "function" or type(permit.release) ~= "function" then
        self.permit = nil
        if type(permit) == "table" and type(permit.release) == "function" and not release(self, permit) then
          finish(failure("unavailable"))
        else
          report("fenced_no_effect", "unavailable")
        end
        return
      end
      local state = self.session:inspect()
      local page = self.projection:page(0, 1, identity.expected_revision)
      local current = self.current()
        and state.status ~= "disposed"
        and self.session.owner.sessions[state.id] == self.session
      local code
      if not permit.valid() then
        code = "expired"
      elseif not current or page == nil then
        code = "stale_subject"
      elseif state.status ~= "ready" then
        code = "busy"
      end
      if detached or finished then
        release(self, permit)
        return
      end
      if code ~= nil then
        if release(self, permit) then
          report("fenced_no_effect", code)
        else
          finish(failure("unavailable"))
        end
        return
      end
      local phases = ActionPhases.new(self.session, self.journal, identity)
      self.phases[#self.phases + 1] = phases
      transitioning = true
      local turn_id, prompt_error = self.session:prompt(text, function(result, err)
        phases:completed(err == nil and result ~= nil)
      end, nil, text)
      transitioning = false
      admitted = turn_id ~= nil
      local released = release(self, permit)
      if turn_id == nil then
        phases:dispose()
        table.remove(self.phases)
      end
      if turn_id == nil and not released then
        finish(failure("unavailable"))
      elseif turn_id == nil then
        self.last_error = prompt_error
        report("fenced_no_effect", "unavailable")
      else
        phases:admitted(turn_id, function(evidence)
          if evidence == nil or not released then
            finish(failure("unavailable"))
          else
            finish({
              v = 1,
              type = "action_result",
              operation_id = identity.operation_id,
              state = "accepted",
              code = "",
              evidence = evidence,
            })
          end
        end)
      end
    end)
  end)
end

---Retry unconfirmed native phase reports when storage becomes available.
function Action:retry_phases()
  for _, phases in ipairs(self.phases) do
    phases:retry()
  end
end

---Fence an unadmitted action after its controller connection is lost.
---@param callback fun(result: table) Called after durable owner evidence or an unavailable refusal.
function Action:detach(callback)
  if self.detach_action ~= nil then
    self.detach_action(callback)
  else
    nvim.schedule(function()
      callback(failure("unavailable"))
    end)
  end
end

---Suppress pending replies without cancelling already admitted Session work.
function Action:dispose()
  if self.disposed then
    return
  end
  self.disposed = true
  release(self, self.permit)
  for _, phases in ipairs(self.phases) do
    phases:dispose()
  end
end

---Create a Session-bound action owner with asynchronous journal and authority callbacks.
---@param options louiselm.phone_control.ActionOptions
---@return table action
function M.new(options)
  return setmetatable({
    journal = options.journal,
    generation = options.generation,
    session = options.session,
    projection = options.projection,
    current = options.current,
    authorize = options.authorize,
    phases = {},
    pending = false,
    disposed = false,
  }, Action)
end

return M
