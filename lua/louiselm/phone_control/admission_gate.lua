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

---Consume the one-use gate and check the original peer and boot deadline.
---@return boolean valid
local function check(self, session, text)
  if self.used then
    return false
  end
  self.used = true
  if
    self.session ~= session
    or rawget(self.projection, "revision") ~= self.expected_revision
    or type(text) ~= "string"
    or #text < 1
    or #text > 4095
    or text:find("\0", 1, true) ~= nil
    or nvim.fn.sha256(string.char(1) .. text) ~= self.payload_hash
  then
    return false
  end
  if
    self.gate ~= nil
    and (not write(self, self.gate, self.gate_witness) or not ProcessIdentity.alive(self.guard_process))
  then
    return false
  end
  if not write(self, self.controller, self.controller_witness) or not ProcessIdentity.alive(self.receiver_process) then
    return false
  end
  local now = BootClock.sample(self.clock)
  return now ~= nil and BootClock.within(self.started, now, self.remaining_ms)
end

---Bind a one-use gate to the registered receiver connection itself.
---The clock starts before the fresh receiver round trip. The receiver cuts this
---same socket before authority invalidation becomes durable.
---@param options table Exact operation, connection, Session and clock.
---@return table? gate
---@return string? error_message
function M.registered(options)
  if
    type(options) ~= "table"
    or type(options.controller) ~= "userdata"
    or type(options.receiver_process) ~= "table"
    or type(options.clock) ~= "table"
    or type(options.started) ~= "table"
    or type(options.session) ~= "table"
    or type(options.projection) ~= "table"
    or type(options.expected_revision) ~= "number"
    or options.expected_revision <= 0
    or options.expected_revision > 9007199254740991
    or options.expected_revision % 1 ~= 0
    or type(options.remaining_ms) ~= "number"
    or options.remaining_ms <= 0
    or options.remaining_ms > 5000
    or options.remaining_ms % 1 ~= 0
    or not hex_id(options.controller_id)
    or not hex_id(options.operation_id)
    or not hex_id(options.gate_id)
    or type(options.payload_hash) ~= "string"
    or #options.payload_hash ~= 64
    or options.payload_hash:match("^[0-9a-f]+$") == nil
  then
    return nil, "unavailable"
  end
  local witness = nvim.json.encode({
    v = 2,
    type = "gate_use",
    controller_id = options.controller_id,
    operation_id = options.operation_id,
    gate_id = options.gate_id,
  }) .. "\n"
  return setmetatable({
    controller = options.controller,
    receiver_process = options.receiver_process,
    session = options.session,
    projection = options.projection,
    expected_revision = options.expected_revision,
    payload_hash = options.payload_hash,
    clock = options.clock,
    started = { namespace = options.started.namespace, centiseconds = options.started.centiseconds },
    remaining_ms = options.remaining_ms,
    controller_witness = witness,
    used = false,
  }, Gate),
    nil
end

---Consume only a gate created by this module, without invoking caller callbacks.
---@param gate table
---@param session table Exact native Session bound during registration.
---@param text string Exact phone text bound to the signed payload.
---@return boolean valid
function M.check(gate, session, text)
  return type(gate) == "table" and getmetatable(gate) == Gate and check(gate, session, text)
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
    or type(options.session) ~= "table"
    or type(options.projection) ~= "table"
    or type(options.expected_revision) ~= "number"
    or options.expected_revision <= 0
    or options.expected_revision > 9007199254740991
    or options.expected_revision % 1 ~= 0
    or type(options.remaining_ms) ~= "number"
    or options.remaining_ms <= 0
    or options.remaining_ms > 5000
    or options.remaining_ms % 1 ~= 0
    or not hex_id(options.controller_id)
    or not hex_id(options.operation_id)
    or not hex_id(options.gate_id)
    or type(options.payload_hash) ~= "string"
    or #options.payload_hash ~= 64
    or options.payload_hash:match("^[0-9a-f]+$") == nil
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
    session = options.session,
    projection = options.projection,
    expected_revision = options.expected_revision,
    payload_hash = options.payload_hash,
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
