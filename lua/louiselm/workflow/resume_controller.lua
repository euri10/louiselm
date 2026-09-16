---Operator-only orchestration for revisioned warm and cold Run resume.

local M = {}
local Controller = {}
Controller.__index = Controller

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

---@class louiselm.workflow.ResumeControllerOptions
---@field client louiselm.workflow.RunClient
---@field find_run fun(id: string): louiselm.workflow.Run?
---@field load_cold fun(run: louiselm.workflow.RunView, callback: fun(worker: louiselm.workflow.RunWorker?, error_message?: string))
---@field operation_id? fun(callback: fun(id: string?, error_message?: string)) Testable asynchronous UUID source.

---@class louiselm.workflow.ResumeController
---@field client louiselm.workflow.RunClient
---@field find_run fun(id: string): louiselm.workflow.Run?
---@field load_cold fun(run: louiselm.workflow.RunView, callback: fun(worker: louiselm.workflow.RunWorker?, error_message?: string))
---@field operation_id fun(callback: fun(id: string?, error_message?: string))
---@field disposed boolean
---@field raise fun(self: louiselm.workflow.ResumeController, run: louiselm.workflow.RunView, ceiling: integer, callback: fun(result: louiselm.workflow.RunView?, error_message?: string)): boolean, string?
---@field resume fun(self: louiselm.workflow.ResumeController, run: louiselm.workflow.RunView, callback: fun(result: louiselm.workflow.RunView?, error_message?: string)): boolean, string?
---@field dispose fun(self: louiselm.workflow.ResumeController): boolean

local function random_operation_id(callback)
  nvim.uv.random(16, nil, function(error_message, bytes)
    nvim.schedule(function()
      if error_message ~= nil or type(bytes) ~= "string" or #bytes ~= 16 then
        callback(nil, "could not create resume operation id")
        return
      end
      local hex = bytes:gsub(".", function(character)
        return string.format("%02x", string.byte(character))
      end)
      callback(table.concat({ hex:sub(1, 8), hex:sub(9, 12), hex:sub(13, 16), hex:sub(17, 20), hex:sub(21) }, "-"))
    end)
  end)
end

---Create an explicit owner for operator resume attempts.
---@param options louiselm.workflow.ResumeControllerOptions
---@return louiselm.workflow.ResumeController? controller
---@return string? error_message
function M.new(options)
  if
    type(options) ~= "table"
    or type(options.client) ~= "table"
    or type(options.find_run) ~= "function"
    or type(options.load_cold) ~= "function"
    or (options.operation_id ~= nil and type(options.operation_id) ~= "function")
  then
    return nil, "resume controller requires client, find_run, and load_cold"
  end
  local controller = setmetatable({
    client = options.client,
    find_run = options.find_run,
    load_cold = options.load_cold,
    operation_id = options.operation_id or random_operation_id,
    disposed = false,
  }, Controller)
  ---@cast controller louiselm.workflow.ResumeController
  return controller
end

---Raise a Parked Run ceiling as a separate operator act.
---@param self louiselm.workflow.ResumeController
---@param run louiselm.workflow.RunView
---@param ceiling integer
---@param callback fun(result: louiselm.workflow.RunView?, error_message?: string)
---@return boolean started
---@return string? error_message
function Controller:raise(run, ceiling, callback)
  if self.disposed then
    return false, "resume controller is disposed"
  end
  return self.client:raise(run.id, run.revision, ceiling, callback)
end

local function accept_local_resume(controller, run)
  local live = controller.find_run(run.id)
  if live == nil then
    return true
  end
  return live:accept_resume()
end

---Resume a warm Run directly or reconstruct and finalize one cold Run.
---@param self louiselm.workflow.ResumeController
---@param run louiselm.workflow.RunView
---@param callback fun(result: louiselm.workflow.RunView?, error_message?: string)
---@return boolean started
---@return string? error_message
function Controller:resume(run, callback)
  if self.disposed then
    return false, "resume controller is disposed"
  end
  if type(callback) ~= "function" then
    return false, "resume callback must be a function"
  end
  self.operation_id(function(operation_id, operation_error)
    if self.disposed then
      return
    end
    if operation_id == nil then
      callback(nil, operation_error)
      return
    end
    local started, start_error = self.client:resume(run.id, run.revision, operation_id, function(resuming, resume_error)
      if self.disposed or resuming == nil then
        if not self.disposed then
          callback(nil, resume_error)
        end
        return
      end
      if resuming.state == "active" then
        local accepted, accept_error = accept_local_resume(self, resuming)
        callback(accepted and resuming or nil, accept_error)
        return
      end
      if resuming.state ~= "resuming" then
        callback(nil, "resume service returned an invalid state")
        return
      end
      local function finalize(worker, load_error, retained)
        if self.disposed then
          if worker ~= nil and not retained then
            worker:dispose()
          end
          return
        end
        local finalize_started, finalize_error = self.client:finalize_resume(
          resuming.id,
          resuming.revision,
          operation_id,
          worker ~= nil,
          function(final, service_error)
            if self.disposed then
              if worker ~= nil and not retained then
                worker:dispose()
              end
              return
            end
            if final == nil then
              if worker ~= nil and not retained then
                worker:dispose()
              end
              callback(nil, service_error)
              return
            end
            if worker ~= nil then
              local accepted, accept_error = accept_local_resume(self, final)
              callback(accepted and final or nil, accept_error)
            else
              callback(nil, load_error or "cold Run reconstruction failed")
            end
          end
        )
        if not finalize_started then
          if worker ~= nil and not retained then
            worker:dispose()
          end
          callback(nil, finalize_error)
        end
      end
      local live = self.find_run(run.id)
      local retained = live ~= nil and live.status == "parked" and #live.workers == 1 and live.workers[1] or nil
      if retained ~= nil and retained:inspect().status ~= "disposed" then
        if retained:inspect().status == "ready" then
          finalize(retained, nil, true)
        else
          finalize(nil, "retained Run Session is not ready to resume")
        end
      else
        self.load_cold(resuming, finalize)
      end
    end)
    if not started then
      callback(nil, start_error)
    end
  end)
  return true
end

---Dispose orchestration so late reconstruction results cannot revive a Run.
---@param self louiselm.workflow.ResumeController
---@return boolean disposed
function Controller:dispose()
  self.disposed = true
  return true
end

return M
