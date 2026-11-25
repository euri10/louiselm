---Explicit owner of the optional phone-control channel and Session observations.

---@diagnostic disable-next-line: undefined-global -- Neovim supplies the event loop and paths.
local nvim = vim
local Config = require("louiselm.config")
local Controller = require("louiselm.phone_control.controller")
local Transport = require("louiselm.phone_control.transport")
local Registry = require("louiselm.session.registry")
local Paths = require("louiselm.paths")
local M = {}
local Owner = {}
Owner.__index = Owner

---Return this connection's identities for one exact live Session.
---@param session louiselm.session.Session Registry-owned Session object.
---@return table? identity Current controller and subject IDs.
---@return string? error_code Unavailable or stale subject.
function Owner:subject(session)
  if self.disposed or self.controller.controller_id == nil then
    return nil, "unavailable"
  end
  local subject = self.controller.sessions[session]
  if subject == nil or subject.id == nil then
    return nil, "stale_subject"
  end
  local state = session:inspect()
  if state.status == "disposed" or session.owner.sessions[state.id] ~= session then
    return nil, "stale_subject"
  end
  return { controller_id = self.controller.controller_id, subject_id = subject.id }
end

---Register an exact operator-owned Run; this does not Start or authorize it.
---@param executor louiselm.workflow.BeadExecutor
---@return boolean tracked
---@return string? error_message
function Owner:track_run(executor)
  if self.disposed then
    return false, "phone controller is disposed"
  end
  return self.controller:track_run(executor)
end

---Remove only this exact Run owner.
---@param executor louiselm.workflow.BeadExecutor
function Owner:untrack_run(executor)
  self.controller:untrack_run(executor)
end

---Release the channel, observer, pending verifier, and Session subscriptions.
---@return boolean disposed
---@return string? error_message Cancellation failure.
function Owner:dispose()
  if self.disposed then
    return true
  end
  self.disposed = true
  self.unsubscribe()
  local stopped, stop_error = self.transport:dispose()
  local released, release_error = self.controller:dispose()
  return stopped and released, stop_error or release_error
end

---Start the optional controller; omission does not connect or observe Sessions.
---@param config? table Validated LouiseLM setup configuration.
---@return table? owner Nil when disabled or unavailable.
---@return string? error_message Setup failure, if enabled.
function M.start(config)
  if config == nil or not Config.enabled(config, "phone_control") then
    return nil
  end
  local service = config.capture and config.capture.service or Config.schema.fields.capture.fields.service.default
  ---@cast service string[] -- The validated capture schema defines this default as an argv array.
  local controller = Controller.new({ service = service })
  local unsubscribe = Registry.observe_sessions(function(session, present)
    if present then
      controller:track(session)
    else
      controller:untrack(session)
    end
  end)
  local capture_state = nvim.env.LOUISELM_CAPTURE_STATE_DIR
  local workflow = capture_state and capture_state ~= "" and nvim.fs.joinpath(capture_state, "louiselm", "workflow")
    or nvim.fs.joinpath(Paths.state(), "workflow")
  local transport, err = Transport.connect({
    controller = controller,
    socket_path = nvim.fs.joinpath(workflow, "phone-control.sock"),
    capability_path = nvim.fs.joinpath(workflow, "operator-capability"),
  })
  if transport == nil then
    unsubscribe()
    controller:dispose()
    return nil, err
  end
  local owner =
    setmetatable({ controller = controller, transport = transport, unsubscribe = unsubscribe, disposed = false }, Owner)
  return owner
end

return M
