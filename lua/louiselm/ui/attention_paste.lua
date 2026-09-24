---Own the shared vim.paste activity dispatcher used by Attention controllers.

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

---@class louiselm.ui.AttentionPaste
---@field controllers table<louiselm.ui.Attention, integer> Active controllers and their subscription generation.
---@field generation integer Number of paste calls observed by this dispatcher.
---@field previous? fun(lines: string[], phase: -1|1|2|3): boolean Previously installed paste handler.
---@field dispatcher? fun(lines: string[], phase: -1|1|2|3): boolean Shared wrapper installed while owned.
---@field disposed boolean Whether the owner has released its handlers.
---@field subscribe fun(self: louiselm.ui.AttentionPaste, controller: louiselm.ui.Attention): boolean, string? Register one controller.
---@field unsubscribe fun(self: louiselm.ui.AttentionPaste, controller: louiselm.ui.Attention): boolean Release one controller.
---@field dispose fun(self: louiselm.ui.AttentionPaste): boolean Release all controllers and restore the predecessor when safe.

local M = {}
local Dispatcher = {}
Dispatcher.__index = Dispatcher

local function deliver_activity(self, generation)
  if self.disposed then
    return
  end
  for controller, subscribed_generation in pairs(self.controllers) do
    if self.disposed then
      return
    end
    if
      self.controllers[controller] == subscribed_generation
      and subscribed_generation < generation
      and not controller.disposed
    then
      controller:activity()
    end
  end
end

local function install(self)
  local current = nvim.paste
  if self.previous == nil then
    if type(current) ~= "function" then
      return false, "Neovim vim.paste handler is unavailable"
    end
    local previous = current
    self.previous = previous
    self.dispatcher = function(lines, phase)
      if not self.disposed and next(self.controllers) ~= nil then
        self.generation = self.generation + 1
        local generation = self.generation
        nvim.schedule(function()
          deliver_activity(self, generation)
        end)
      end
      return previous(lines, phase)
    end
  elseif current ~= self.previous and current ~= self.dispatcher then
    -- A later owner has replaced the hook. Preserve it and avoid adding a layer.
    return true
  end

  if current ~= self.dispatcher then
    nvim.paste = self.dispatcher
  end
  return true
end

---Create an explicit owner for the shared Attention paste handler.
---@return louiselm.ui.AttentionPaste dispatcher
function M.new()
  return setmetatable({ controllers = {}, generation = 0, disposed = false }, Dispatcher)
end

---Register an Attention controller to receive paste activity.
---@param self louiselm.ui.AttentionPaste
---@param controller louiselm.ui.Attention
---@return boolean registered
---@return string? error_message
function Dispatcher:subscribe(controller)
  if self.disposed then
    return false, "Attention paste dispatcher is disposed"
  end
  if self.controllers[controller] ~= nil then
    return true
  end
  local installed, error_message = install(self)
  if not installed then
    return false, error_message
  end
  self.controllers[controller] = self.generation
  return true
end

---Release one Attention controller without affecting other subscribers.
---@param self louiselm.ui.AttentionPaste
---@param controller louiselm.ui.Attention
---@return boolean released
function Dispatcher:unsubscribe(controller)
  self.controllers[controller] = nil
  if next(self.controllers) == nil and self.dispatcher ~= nil and nvim.paste == self.dispatcher then
    nvim.paste = self.previous
  end
  return true
end

---Dispose the owner and restore the predecessor only while this dispatcher owns vim.paste.
---@param self louiselm.ui.AttentionPaste
---@return boolean disposed
function Dispatcher:dispose()
  if self.disposed then
    return true
  end
  self.disposed = true
  self.controllers = {}
  if self.dispatcher ~= nil and nvim.paste == self.dispatcher then
    nvim.paste = self.previous
  end
  return true
end

return M
