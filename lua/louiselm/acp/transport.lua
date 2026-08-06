local Config = require("louiselm.agent.config")
local Protocol = require("louiselm.acp.protocol")

---@class louiselm.acp.ProcessHandle
---@field write fun(self: louiselm.acp.ProcessHandle, data: string|nil)
---@field kill fun(self: louiselm.acp.ProcessHandle, signal: string)
---@field is_closing fun(self: louiselm.acp.ProcessHandle): boolean

---@class louiselm.acp.TransportOptions
---@field cwd? string Working directory for the agent process.
---@field on_message? fun(message: louiselm.acp.JsonRpcMessage) Called for each decoded message.
---@field on_error? fun(message: string) Called for protocol, stream, or process errors.
---@field on_stderr? fun(message: string) Called for agent stderr chunks.
---@field on_exit? fun(result: louiselm.agent.ProcessResult) Called once after process exit.

---@class louiselm.acp.Transport
---@field handle louiselm.acp.ProcessHandle
---@field buffer string
---@field closed boolean
---@field options louiselm.acp.TransportOptions
---@field send fun(self: louiselm.acp.Transport, message: louiselm.acp.JsonRpcMessage): boolean, string?
---@field close fun(self: louiselm.acp.Transport): boolean, string?
---@field is_open fun(self: louiselm.acp.Transport): boolean

local M = {}
local Transport = {}
Transport.__index = Transport

---@param definition unknown
---@return louiselm.agent.Definition? normalized
---@return string? error_message
local function normalize_definition(definition)
  local normalized, errors = Config.normalize({ agent = definition })
  if normalized == nil then
    local first_error = errors[1]
    return nil, first_error.path .. ": " .. first_error.message
  end
  return normalized.agent
end

---@param self louiselm.acp.Transport
---@param message string
local function report_error(self, message)
  local on_error = self.options.on_error
  if on_error ~= nil then
    on_error(message)
  end
end

---@param self louiselm.acp.Transport
---@param chunk string
local function process_stdout(self, chunk)
  self.buffer = self.buffer .. chunk
  local start = 1
  while true do
    local newline = self.buffer:find("\n", start, true)
    if newline == nil then
      self.buffer = self.buffer:sub(start)
      return
    end

    local line = self.buffer:sub(start, newline - 1):gsub("\r$", "")
    if line == "" then
      report_error(self, "empty ACP message line")
    else
      local message, decode_error = Protocol.decode(line)
      if message == nil then
        report_error(self, decode_error or "invalid ACP message")
      else
        local on_message = self.options.on_message
        if on_message ~= nil then
          on_message(message)
        end
      end
    end
    start = newline + 1
  end
end

---@param self louiselm.acp.Transport
---@param err string?
---@param data string?
local function on_stdout(self, err, data)
  if err ~= nil then
    report_error(self, err)
    return
  end
  if data ~= nil and data ~= "" then
    process_stdout(self, data)
  end
end

---@param self louiselm.acp.Transport
---@param err string?
---@param data string?
local function on_stderr(self, err, data)
  if err ~= nil then
    report_error(self, err)
    return
  end
  if data ~= nil and data ~= "" then
    local callback = self.options.on_stderr
    if callback ~= nil then
      callback(data)
    end
  end
end

---@param self louiselm.acp.Transport
---@param result louiselm.agent.ProcessResult
local function on_exit(self, result)
  if self.closed then
    return
  end
  self.closed = true
  if self.buffer ~= "" then
    report_error(self, "incomplete ACP message at process exit")
    self.buffer = ""
  end
  local callback = self.options.on_exit
  if callback ~= nil then
    callback(result)
  end
end

---Start an ACP agent and connect its JSON-RPC stream.
---@param definition louiselm.agent.Definition Normalized or valid agent definition.
---@param options? louiselm.acp.TransportOptions Transport callbacks and process options.
---@return louiselm.acp.Transport? transport
---@return string? error_message Validation or launch error.
function M.start(definition, options)
  local normalized, validation_error = normalize_definition(definition)
  if normalized == nil then
    return nil, validation_error
  end
  local transport = setmetatable({
    buffer = "",
    closed = false,
    options = options or {},
  }, Transport)

  local command = { normalized.command }
  for _, argument in ipairs(normalized.args) do
    command[#command + 1] = argument
  end
  local process_options = {
    stdin = true,
    stdout = function(err, data)
      on_stdout(transport, err, data)
    end,
    stderr = function(err, data)
      on_stderr(transport, err, data)
    end,
    text = true,
  }
  if normalized.env ~= nil then
    process_options.env = normalized.env
  end
  if transport.options.cwd ~= nil then
    process_options.cwd = transport.options.cwd
  end

  ---@diagnostic disable-next-line: undefined-global -- vim.system is Neovim's stable process API.
  local call_ok, handle_or_error = pcall(vim.system, command, process_options, function(result)
    on_exit(transport, result)
  end)
  if not call_ok then
    return nil, tostring(handle_or_error)
  end
  ---@cast handle_or_error louiselm.acp.ProcessHandle
  transport.handle = handle_or_error
  return transport
end

---Send one JSON-RPC message over the ACP stdio stream.
---@param self louiselm.acp.Transport
---@param message louiselm.acp.JsonRpcMessage Message to send.
---@return boolean sent
---@return string? error_message
function Transport:send(message)
  if self.closed then
    return false, "ACP transport is closed"
  end
  local encoded, encode_error = Protocol.encode(message)
  if encoded == nil then
    return false, encode_error
  end
  local call_ok, write_error = pcall(self.handle.write, self.handle, encoded .. "\n")
  if not call_ok then
    return false, tostring(write_error)
  end
  return true
end

---Close the agent's stdin and terminate the process.
---@param self louiselm.acp.Transport
---@return boolean closed
---@return string? error_message
function Transport:close()
  if self.closed then
    return true
  end
  self.closed = true
  local write_ok, write_error = pcall(self.handle.write, self.handle, nil)
  if not write_ok then
    return false, tostring(write_error)
  end
  local kill_ok, kill_error = pcall(self.handle.kill, self.handle, "sigterm")
  if not kill_ok then
    return false, tostring(kill_error)
  end
  return true
end

---Return whether the ACP process can still receive messages.
---@param self louiselm.acp.Transport
---@return boolean open
function Transport:is_open()
  if self.closed then
    return false
  end
  local call_ok, closing = pcall(self.handle.is_closing, self.handle)
  return call_ok and not closing
end

return M
