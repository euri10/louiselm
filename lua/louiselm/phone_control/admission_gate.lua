---One non-yielding check of a registered phone admission gate.

---@diagnostic disable-next-line: undefined-global -- Neovim owns the socket handles.
local nvim = vim
local BootClock = require("louiselm.phone_control.boot_clock")
local ProcessIdentity = require("louiselm.phone_control.process_identity")
local M = {}
local Gate = {}
Gate.__index = Gate

local function hex_id(value)
  return type(value) == "string" and #value == 32 and value:match("^[0-9a-f]+$") ~= nil
end

local function write(self, stream, bytes)
  local ok, count, cause = pcall(nvim.uv.try_write, stream, bytes)
  if not ok then
    self.error = count
    return false
  end
  if count ~= #bytes then
    self.error = cause or "incomplete witness"
    return false
  end
  return true
end

---Consume this gate and check both original peers and the owner's boot deadline.
---This is not Session admission: the caller must immediately perform the native
---transition without yielding or invoking callbacks, and discard a failed check.
---@return boolean valid
local function check(self)
  if self.used then
    return false
  end
  self.used = true
  if not write(self, self.gate, self.gate_witness) or not ProcessIdentity.alive(self.guard_process) then
    return false
  end
  if not write(self, self.controller, self.controller_witness) or not ProcessIdentity.alive(self.receiver_process) then
    return false
  end
  local now = BootClock.sample(self.clock)
  return now ~= nil and BootClock.within(self.started, now, self.remaining_ms)
end

---Consume only a gate created by this module, without invoking caller callbacks.
---@param gate table
---@return boolean valid
function M.check(gate)
  return type(gate) == "table" and getmetatable(gate) == Gate and check(gate)
end

---Create a one-use check for the exact registered gate and original connection.
---Both process descriptors must already be bound to fresh replies from their
---respective peers; the clock start precedes the fresh receiver round trip.
---The caller owns every handle and closes the gate after the native transition.
---@param options table Exact operation, peers, clock and remaining lease bound.
---@return table? gate
---@return string? error_message
function M.new(options)
  if
    type(options) ~= "table"
    or type(options.gate) ~= "userdata"
    or type(options.controller) ~= "userdata"
    or options.gate == options.controller
    or type(options.guard_process) ~= "table"
    or type(options.receiver_process) ~= "table"
    or type(options.clock) ~= "table"
    or type(options.started) ~= "table"
    or type(options.remaining_ms) ~= "number"
    or options.remaining_ms <= 0
    or options.remaining_ms > 5000
    or options.remaining_ms % 1 ~= 0
    or not hex_id(options.controller_id)
    or not hex_id(options.operation_id)
    or not hex_id(options.gate_id)
  then
    return nil, "unavailable"
  end
  local witness = {
    v = 2,
    type = "gate_use",
    controller_id = options.controller_id,
    operation_id = options.operation_id,
    gate_id = options.gate_id,
  }
  local gate_witness = nvim.json.encode(witness) .. "\n"
  witness.type = "action_witness"
  return setmetatable({
    gate = options.gate,
    controller = options.controller,
    guard_process = options.guard_process,
    receiver_process = options.receiver_process,
    clock = options.clock,
    started = { namespace = options.started.namespace, centiseconds = options.started.centiseconds },
    remaining_ms = options.remaining_ms,
    gate_witness = gate_witness,
    controller_witness = nvim.json.encode(witness) .. "\n",
    used = false,
  }, Gate),
    nil
end

return M
