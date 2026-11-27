---Reconnectable owner-only local channel for a phone-control controller.

---@diagnostic disable-next-line: undefined-global -- Neovim owns the event loop.
local nvim = vim
local RunClient = require("louiselm.workflow.run_client")
local ProcessIdentity = require("louiselm.phone_control.process_identity")
local BootClock = require("louiselm.phone_control.boot_clock")
local M = {}
local Transport = {}
Transport.__index = Transport
local MAX_LINE = 262144

local function attached_identity(line)
  local inner = line:match("^%s*%{(.*)%}%s*$")
  if inner == nil then
    return nil
  end
  local fields, seen, id = nvim.split(inner, ",", { plain = true }), {}, nil
  if #fields ~= 3 then
    return nil
  end
  for _, field in ipairs(fields) do
    local name, value = field:match('^%s*"([a-z_]+)"%s*:%s*(.-)%s*$')
    if name == nil or seen[name] then
      return nil
    end
    seen[name] = true
    if name == "v" and value ~= "1" then
      return nil
    end
    if name == "type" and value ~= '"attached"' then
      return nil
    end
    if name == "controller_id" then
      id = value:match('^"([0-9a-f]+)"$')
      if id == nil or #id ~= 32 then
        return nil
      end
    elseif name ~= "v" and name ~= "type" then
      return nil
    end
  end
  return seen.v and seen.type and id or nil
end

local connect

local function disconnect(self, pipe, read_only)
  if self.disposed or (pipe ~= nil and self.pipe ~= pipe) then
    return
  end
  self.generation = self.generation + 1
  local detached, detach_error = self.controller:detach()
  if self.candidate ~= nil then
    self.candidate.process:dispose()
    self.candidate.clock:dispose()
    self.candidate = nil
  end
  self.read_only = read_only == true
  self.phase = nil
  self.ready, self.awaiting, self.buffer, self.queued, self.invalid = false, false, "", 0, false
  local current = self.pipe
  self.pipe = nil
  if current ~= nil and not current:is_closing() then
    current:close()
  end
  self.timer:stop()
  if not detached then
    self.error = detach_error
    return
  end
  self.timer:start(self.retry_ms, 0, function()
    nvim.schedule(function()
      connect(self)
    end)
  end)
end

local function handshake_message(line, allowed, kind)
  if line:find("\\", 1, true) then
    return nil
  end
  local seen, count = {}, 0
  for name in line:gmatch('"([a-z_]+)"%s*:') do
    if seen[name] then
      return nil
    end
    seen[name] = true
  end
  local ok, message = pcall(nvim.json.decode, line)
  if
    not ok
    or type(message) ~= "table"
    or message.v ~= 1
    or message.type ~= kind
    or not line:match('"v"%s*:%s*1%s*[,}]')
  then
    return nil
  end
  for name in pairs(message) do
    if not allowed[name] then
      return nil
    end
    count = count + 1
  end
  local expected = 0
  for _ in pairs(allowed) do
    expected = expected + 1
  end
  if count ~= expected then
    return nil
  end
  return message
end

local function hex_id(value)
  return type(value) == "string" and #value == 32 and value:match("^[0-9a-f]+$") ~= nil and value ~= string.rep("0", 32)
end

local function begin_registration(self, pipe, generation, offer)
  if not hex_id(offer.controller_id) or not hex_id(offer.generation) then
    disconnect(self, pipe, true)
    return
  end
  local process = ProcessIdentity.new(offer.receiver_pid, offer.receiver_namespace)
  if process == nil then
    disconnect(self, pipe, true)
    return
  end
  local candidate = {
    process = process,
    clock = BootClock.new(),
    generation = offer.generation,
    controller_id = offer.controller_id,
    pipe = pipe,
  }
  self.candidate, self.phase = candidate, "opening"
  local function current()
    return not self.disposed and self.pipe == pipe and self.generation == generation and self.candidate == candidate
  end
  process:open(function(opened)
    if not current() then
      return
    end
    if not opened then
      disconnect(self, pipe, true)
      return
    end
    candidate.clock:open(function(clock_opened)
      if not current() then
        return
      end
      if not clock_opened then
        disconnect(self, pipe, true)
        return
      end
      local bytes = nvim.uv.random(16)
      if bytes == nil then
        disconnect(self, pipe, true)
        return
      end
      candidate.nonce = bytes:gsub(".", function(byte)
        return string.format("%02x", byte:byte())
      end)
      if not hex_id(candidate.nonce) then
        disconnect(self, pipe, true)
        return
      end
      self.phase = "registering"
      pipe:write(nvim.json.encode({
        v = 1,
        type = "register",
        controller_id = candidate.controller_id,
        nonce = candidate.nonce,
      }) .. "\n", function(err)
        if err ~= nil then
          nvim.schedule(function()
            disconnect(self, pipe, true)
          end)
        end
      end)
    end)
  end)
end

local function handle_line(self, pipe, generation, line)
  if self.disposed or self.invalid or self.pipe ~= pipe or self.generation ~= generation then
    return
  end
  if not self.ready then
    if self.phase == "registering" then
      local response = handshake_message(
        line,
        { v = true, type = true, controller_id = true, generation = true, nonce = true },
        "registered"
      )
      local candidate = self.candidate
      if
        response == nil
        or candidate == nil
        or response.controller_id ~= candidate.controller_id
        or response.generation ~= candidate.generation
        or response.nonce ~= candidate.nonce
        or not ProcessIdentity.alive(candidate.process)
        or not self.controller:attach(candidate.controller_id, candidate)
      then
        disconnect(self, pipe, true)
        return
      end
      self.ready, self.phase = true, nil
      return
    end
    if self.phase ~= nil then
      disconnect(self, pipe, true)
      return
    end
    local id = attached_identity(line)
    if id ~= nil and self.controller:attach(id) then
      self.ready = true
      return
    end
    local offer = handshake_message(
      line,
      { v = true, type = true, controller_id = true, generation = true, receiver_pid = true, receiver_namespace = true },
      "registration_offer"
    )
    if offer == nil or self.read_only then
      disconnect(self, pipe, true)
      return
    end
    begin_registration(self, pipe, generation, offer)
    return
  end
  if self.awaiting then
    disconnect(self, pipe)
    return
  end
  self.awaiting = true
  self.controller:read(line, function(result)
    if self.disposed or self.pipe ~= pipe or self.generation ~= generation then
      return
    end
    self.awaiting = false
    local encoded = nvim.json.encode(result) .. "\n"
    pipe:write(encoded, function(err)
      if err ~= nil then
        nvim.schedule(function()
          disconnect(self, pipe)
        end)
      end
    end)
  end)
end

connect = function(self)
  if self.disposed or self.pipe ~= nil then
    return
  end
  local generation = self.generation
  local started = RunClient.read_operator_capability(self.capability_path, function(capability)
    if self.disposed or self.generation ~= generation then
      return
    end
    if capability == nil then
      disconnect(self)
      return
    end
    local pipe = nvim.uv.new_pipe(false)
    if pipe == nil then
      disconnect(self)
      return
    end
    self.pipe = pipe
    pipe:connect(self.socket_path, function(err)
      nvim.schedule(function()
        if self.disposed or self.pipe ~= pipe or self.generation ~= generation then
          return
        end
        if err ~= nil then
          disconnect(self, pipe)
          return
        end
        pipe:read_start(function(read_error, data)
          if read_error ~= nil or data == nil then
            nvim.schedule(function()
              disconnect(self, pipe)
            end)
            return
          end
          if self.invalid or self.pipe ~= pipe or self.generation ~= generation then
            return
          end
          local position = 1
          while position <= #data do
            local ending = data:find("\n", position, true)
            local length = ending and ending - position + 1 or #data - position + 1
            if #self.buffer + length > MAX_LINE or (ending ~= nil and self.queued >= 2) then
              self.invalid = true
              nvim.schedule(function()
                disconnect(self, pipe)
              end)
              return
            end
            if ending == nil then
              self.buffer = self.buffer .. data:sub(position)
              return
            end
            local line = self.buffer .. data:sub(position, ending - 1)
            self.buffer = ""
            self.queued = self.queued + 1
            position = ending + 1
            nvim.schedule(function()
              if self.pipe == pipe and self.generation == generation then
                self.queued = self.queued - 1
              end
              handle_line(self, pipe, generation, line)
            end)
          end
        end)
        pipe:write(
          nvim.json.encode({ v = 1, type = self.read_only and "attach" or "attach_write", capability = capability })
            .. "\n",
          function(write_error)
            if write_error ~= nil then
              nvim.schedule(function()
                disconnect(self, pipe)
              end)
            end
          end
        )
      end)
    end)
  end)
  if not started then
    disconnect(self)
  end
end

---Close the socket and retry timer; late capability and socket callbacks are inert.
---@return boolean disposed
---@return string? error_message Cancellation failure.
function Transport:dispose()
  if self.disposed then
    return true
  end
  self.disposed = true
  self.generation = self.generation + 1
  local detached, detach_error = self.controller:detach()
  if self.candidate ~= nil then
    self.candidate.process:dispose()
    self.candidate.clock:dispose()
    self.candidate = nil
  end
  self.timer:stop()
  self.timer:close()
  if self.pipe ~= nil and not self.pipe:is_closing() then
    self.pipe:close()
  end
  self.pipe = nil
  return detached and self.error == nil, self.error or detach_error
end

---Start an asynchronous owner-only connection and retry after disconnect.
---@param options {controller: table, socket_path: string, capability_path: string, retry_ms?: integer}
---@return table? transport Explicitly disposable owner of socket and retry timer.
---@return string? error_message Failure to allocate the retry timer.
function M.connect(options)
  local timer, timer_error = nvim.uv.new_timer()
  if timer == nil then
    return nil, timer_error or "could not create phone-control retry timer"
  end
  local self = setmetatable({
    controller = options.controller,
    socket_path = options.socket_path,
    capability_path = options.capability_path,
    retry_ms = options.retry_ms or 1000,
    timer = timer,
    buffer = "",
    generation = 0,
    ready = false,
    awaiting = false,
    queued = 0,
    invalid = false,
    disposed = false,
  }, Transport)
  connect(self)
  return self
end

return M
