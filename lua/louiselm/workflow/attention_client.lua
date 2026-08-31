---Asynchronous client for the owner-only typed Attention socket.

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local M = {}
local Client = {}
Client.__index = Client

---@class louiselm.workflow.AttentionClientOptions
---@field operator_capability string Shared owner capability for mutations.
---@field on_error? fun(message: string)
---@field pipe_factory? fun(): table Test seam returning a `vim.uv` pipe-compatible handle.

---@class louiselm.workflow.AttentionClient
---@field path string
---@field operator_capability string
---@field on_snapshot fun(snapshot: table)
---@field on_error? fun(message: string)
---@field pipe_factory fun(): table
---@field pipe? table
---@field buffer string
---@field pending table<string, fun(snapshot: table?, error_message?: string)>
---@field next_request_id integer
---@field disposed boolean
---@field ready boolean
---@field dispose fun(self: louiselm.workflow.AttentionClient): boolean
---@field upsert fun(self: louiselm.workflow.AttentionClient, attention: table, callback: fun(snapshot: table?, error_message?: string)): boolean, string?
---@field set_eligible fun(self: louiselm.workflow.AttentionClient, key: table, eligible: boolean, callback: fun(snapshot: table?, error_message?: string)): boolean, string?
---@field clear fun(self: louiselm.workflow.AttentionClient, key: table, callback: fun(snapshot: table?, error_message?: string)): boolean, string?
---@field clear_session fun(self: louiselm.workflow.AttentionClient, session_id: string, callback: fun(snapshot: table?, error_message?: string)): boolean, string?

local function close_pipe(pipe)
  if pipe ~= nil and not pipe:is_closing() then
    pipe:read_stop()
    pipe:close()
  end
end

local function report_error(client, message)
  if not client.disposed and client.on_error ~= nil then
    client.on_error(message)
  end
end

local function fail_pending(client, message)
  local pending = client.pending
  client.pending = {}
  for _, callback in pairs(pending) do
    callback(nil, message)
  end
end

local function request_snapshot(client)
  if client.ready and client.pipe ~= nil and not client.pipe:is_closing() then
    client.pipe:write('{"type":"snapshot"}\n')
  end
end

local function valid_snapshot(snapshot)
  if type(snapshot) ~= "table" or type(snapshot.generation) ~= "number" or snapshot.generation % 1 ~= 0 then
    return false
  end
  return type(snapshot.items) == "table"
end

local function handle_line(client, line)
  if client.disposed then
    return
  end
  local decoded_ok, message = pcall(nvim.json.decode, line)
  if not decoded_ok or type(message) ~= "table" then
    report_error(client, "Attention socket returned invalid JSON")
    return
  end
  if message.type == "snapshot" and valid_snapshot(message.snapshot) then
    client.ready = true
    client.on_snapshot(message.snapshot)
    return
  end
  if
    message.type == "mutation_result"
    and type(message.request_id) == "string"
    and valid_snapshot(message.snapshot)
  then
    client.ready = true
    client.on_snapshot(message.snapshot)
    local callback = client.pending[message.request_id]
    if callback ~= nil then
      client.pending[message.request_id] = nil
      callback(message.snapshot)
    end
    return
  end
  if
    message.type == "mutation_error"
    and type(message.request_id) == "string"
    and type(message.message) == "string"
  then
    local callback = client.pending[message.request_id]
    if callback ~= nil then
      client.pending[message.request_id] = nil
      callback(nil, message.message)
    end
    return
  end
  if message.type == "attention_changed" and type(message.generation) == "number" then
    request_snapshot(client)
    return
  end
  report_error(client, "Attention socket returned an invalid message")
end

local function consume(client, chunk)
  client.buffer = client.buffer .. chunk
  while true do
    local newline = client.buffer:find("\n", 1, true)
    if newline == nil then
      return
    end
    local line = client.buffer:sub(1, newline - 1)
    client.buffer = client.buffer:sub(newline + 1)
    nvim.schedule(function()
      handle_line(client, line)
    end)
  end
end

local function send(client, message, callback)
  if client.disposed or not client.ready or client.pipe == nil or client.pipe:is_closing() then
    return false, "Attention socket is not connected"
  end
  local request_id = tostring(client.next_request_id)
  client.next_request_id = client.next_request_id + 1
  message.request_id = request_id
  message.capability = client.operator_capability
  client.pending[request_id] = callback
  local encoded_ok, encoded = pcall(nvim.json.encode, message)
  if not encoded_ok then
    client.pending[request_id] = nil
    return false, "Attention request could not be encoded"
  end
  local write_ok, write_error = pcall(client.pipe.write, client.pipe, encoded .. "\n")
  if not write_ok then
    client.pending[request_id] = nil
    return false, "Attention request could not be sent: " .. tostring(write_error)
  end
  return true
end

---Connect to the local Attention socket.
---@param path string Unix-domain socket path.
---@param on_snapshot fun(snapshot: table) Called on Neovim's main loop.
---@param options? louiselm.workflow.AttentionClientOptions
---@return louiselm.workflow.AttentionClient? client
---@return string? error_message
function M.connect(path, on_snapshot, options)
  if type(path) ~= "string" or path == "" then
    return nil, "Attention socket path must be a non-empty string"
  end
  if type(on_snapshot) ~= "function" then
    return nil, "Attention snapshot callback must be a function"
  end
  options = options or {}
  if type(options) ~= "table" then
    return nil, "Attention client options must be a table"
  end
  if type(options.operator_capability) ~= "string" or options.operator_capability == "" then
    return nil, "Attention client operator capability must be a non-empty string"
  end
  if options.on_error ~= nil and type(options.on_error) ~= "function" then
    return nil, "Attention client on_error must be a function"
  end
  if options.pipe_factory ~= nil and type(options.pipe_factory) ~= "function" then
    return nil, "Attention client pipe_factory must be a function"
  end
  local pipe_factory = options.pipe_factory or function()
    return nvim.uv.new_pipe(false)
  end
  local pipe = pipe_factory()
  if pipe == nil then
    return nil, "could not create Attention socket handle"
  end
  local client = setmetatable({
    path = path,
    operator_capability = options.operator_capability,
    on_snapshot = on_snapshot,
    on_error = options.on_error,
    pipe_factory = pipe_factory,
    pipe = pipe,
    buffer = "",
    pending = {},
    next_request_id = 1,
    disposed = false,
    ready = false,
  }, Client)
  pipe:connect(path, function(connect_error)
    if connect_error ~= nil then
      close_pipe(pipe)
      nvim.schedule(function()
        report_error(client, "could not connect to Attention socket")
      end)
      return
    end
    pipe:read_start(function(read_error, data)
      if read_error ~= nil or data == nil then
        close_pipe(pipe)
        nvim.schedule(function()
          if client.disposed then
            return
          end
          client.ready = false
          fail_pending(client, "Attention socket disconnected")
          report_error(client, "Attention socket disconnected")
        end)
        return
      end
      consume(client, data)
    end)
  end)
  return client
end

---Upsert one validated typed Attention draft.
---@param self louiselm.workflow.AttentionClient
---@param attention table Attention draft matching the socket protocol.
---@param callback fun(snapshot: table?, error_message?: string)
---@return boolean sent
---@return string? error_message
function Client:upsert(attention, callback)
  return send(self, { type = "upsert", attention = attention }, callback)
end

---Change eligibility for one Attention key.
---@param self louiselm.workflow.AttentionClient
---@param key table Attention key matching the socket protocol.
---@param eligible boolean New eligibility.
---@param callback fun(snapshot: table?, error_message?: string)
---@return boolean sent
---@return string? error_message
function Client:set_eligible(key, eligible, callback)
  return send(self, { type = "set_eligible", key = key, eligible = eligible }, callback)
end

---Clear one Attention key.
---@param self louiselm.workflow.AttentionClient
---@param key table Attention key matching the socket protocol.
---@param callback fun(snapshot: table?, error_message?: string)
---@return boolean sent
---@return string? error_message
function Client:clear(key, callback)
  return send(self, { type = "clear", key = key }, callback)
end

---Clear every Attention condition belonging to one Session.
---@param self louiselm.workflow.AttentionClient
---@param session_id string Agent-side Session identifier.
---@param callback fun(snapshot: table?, error_message?: string)
---@return boolean sent
---@return string? error_message
function Client:clear_session(session_id, callback)
  return send(self, { type = "clear_session", session_id = session_id }, callback)
end

---Dispose the transport and make queued or late callbacks inert.
---@param self louiselm.workflow.AttentionClient
---@return boolean disposed
function Client:dispose()
  if self.disposed then
    return true
  end
  self.disposed = true
  self.ready = false
  fail_pending(self, "Attention socket client is disposed")
  close_pipe(self.pipe)
  return true
end

return M
