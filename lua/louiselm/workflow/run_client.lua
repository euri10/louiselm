---Asynchronous local client for authoritative durable Run snapshots.

local M = {}
local Client = {}
Client.__index = Client

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

---@class louiselm.workflow.RunView
---@field id string
---@field revision integer
---@field state string
---@field session_id? string
---@field generated_work_ceiling integer
---@field generated_work_consumed integer
---@field generated_work_reserved integer
---@field pending_mutation_ids string[]
---@field triggering_mutation_id? string
---@field park_expires_at_ms integer
---@field resume_operation_id? string
---@field resume_deadline_ms? integer

---@class louiselm.workflow.RunClientOptions
---@field on_error? fun(message: string)
---@field pipe_factory? fun(): table Test seam returning a `vim.uv` pipe-compatible handle.
---@field operator_capability? string Private operator capability; never pass through Agent environment.

---@class louiselm.workflow.RunClient
---@field path string
---@field on_snapshot fun(runs: louiselm.workflow.RunView[])
---@field on_error? fun(message: string)
---@field pipe_factory fun(): table
---@field pipe? table
---@field buffer string
---@field revisions table<string, integer>
---@field disposed boolean
---@field operator_capability? string
---@field next_request_id integer
---@field pending table<string, fun(run: louiselm.workflow.RunView?, error_message?: string)>
---@field dispose fun(self: louiselm.workflow.RunClient): boolean
---@field raise fun(self: louiselm.workflow.RunClient, id: string, expected_revision: integer, ceiling: integer, callback: fun(run: louiselm.workflow.RunView?, error_message?: string)): boolean, string?
---@field resume fun(self: louiselm.workflow.RunClient, id: string, expected_revision: integer, operation_id: string, callback: fun(run: louiselm.workflow.RunView?, error_message?: string)): boolean, string?
---@field finalize_resume fun(self: louiselm.workflow.RunClient, id: string, expected_revision: integer, operation_id: string, succeeded: boolean, callback: fun(run: louiselm.workflow.RunView?, error_message?: string)): boolean, string?

local function valid_run(run)
  return type(run) == "table"
    and type(run.id) == "string"
    and run.id ~= ""
    and type(run.revision) == "number"
    and run.revision >= 1
    and run.revision % 1 == 0
    and type(run.state) == "string"
    and type(run.generated_work_ceiling) == "number"
    and type(run.generated_work_consumed) == "number"
    and type(run.generated_work_reserved) == "number"
    and type(run.pending_mutation_ids) == "table"
    and type(run.park_expires_at_ms) == "number"
end

local function close_pipe(pipe)
  if pipe ~= nil and not pipe:is_closing() then
    pipe:read_stop()
    pipe:close()
  end
end

local function report_error(client, message)
  if client.disposed or client.on_error == nil then
    return
  end
  client.on_error(message)
end

local function fail_pending(client, message)
  local pending = client.pending
  client.pending = {}
  for _, callback in pairs(pending) do
    callback(nil, message)
  end
end

local function request_snapshot(client)
  if client.pipe ~= nil and not client.pipe:is_closing() then
    client.pipe:write('{"type":"snapshot"}\n')
  end
end

local function handle_line(client, line)
  if client.disposed then
    return
  end
  local decoded_ok, message = pcall(nvim.json.decode, line)
  if not decoded_ok or type(message) ~= "table" then
    report_error(client, "Run socket returned invalid JSON")
    return
  end
  if message.type == "snapshot" and type(message.runs) == "table" then
    local revisions = {}
    for _, run in ipairs(message.runs) do
      if not valid_run(run) then
        report_error(client, "Run socket returned an invalid snapshot")
        return
      end
      revisions[run.id] = run.revision
    end
    client.revisions = revisions
    client.on_snapshot(message.runs)
    return
  end
  if message.type == "mutation_result" and type(message.request_id) == "string" and valid_run(message.run) then
    local callback = client.pending[message.request_id]
    if callback ~= nil then
      client.pending[message.request_id] = nil
      callback(message.run)
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
  if
    message.type == "run_changed"
    and type(message.id) == "string"
    and type(message.revision) == "number"
    and message.revision % 1 == 0
  then
    if (client.revisions[message.id] or 0) < message.revision then
      request_snapshot(client)
    end
    return
  end
  report_error(client, "Run socket returned an invalid message")
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

---Read the daemon-managed operator capability asynchronously without exposing it to Agent config.
---@param path string Capability file path.
---@param callback fun(capability: string?, error_message?: string) Called on Neovim's main loop.
---@param uv? table Testable libuv boundary.
---@return boolean started
---@return string? error_message
function M.read_operator_capability(path, callback, uv)
  if type(path) ~= "string" or path == "" then
    return false, "operator capability path must be a non-empty string"
  end
  if type(callback) ~= "function" then
    return false, "operator capability callback must be a function"
  end
  uv = uv or nvim.uv
  local finished = false
  local function finish(capability, error_message)
    if finished then
      return
    end
    finished = true
    nvim.schedule(function()
      callback(capability, error_message)
    end)
  end
  uv.fs_open(path, "r", 384, function(open_error, descriptor)
    if open_error ~= nil or descriptor == nil then
      finish(nil, "could not open operator capability")
      return
    end
    uv.fs_fstat(descriptor, function(stat_error, stat)
      if
        stat_error ~= nil
        or type(stat) ~= "table"
        or type(stat.size) ~= "number"
        or stat.size < 1
        or stat.size > 128
      then
        uv.fs_close(descriptor, function() end)
        finish(nil, "operator capability file is invalid")
        return
      end
      uv.fs_read(descriptor, stat.size, 0, function(read_error, value)
        uv.fs_close(descriptor, function() end)
        if
          read_error ~= nil
          or type(value) ~= "string"
          or value:match(
              "^[0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]%-[0-9a-f][0-9a-f][0-9a-f][0-9a-f]%-[0-9a-f][0-9a-f][0-9a-f][0-9a-f]%-[0-9a-f][0-9a-f][0-9a-f][0-9a-f]%-[0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]$"
            )
            == nil
        then
          finish(nil, "operator capability file is invalid")
          return
        end
        finish(value)
      end)
    end)
  end)
  return true
end

---Connect to the local Run socket. Every connection waits for its authoritative initial snapshot.
---@param path string Unix-domain socket path.
---@param on_snapshot fun(runs: louiselm.workflow.RunView[]) Called on Neovim's main loop.
---@param options? louiselm.workflow.RunClientOptions
---@return louiselm.workflow.RunClient? client
---@return string? error_message
function M.connect(path, on_snapshot, options)
  if type(path) ~= "string" or path == "" then
    return nil, "Run socket path must be a non-empty string"
  end
  if type(on_snapshot) ~= "function" then
    return nil, "Run snapshot callback must be a function"
  end
  options = options or {}
  if type(options) ~= "table" then
    return nil, "Run client options must be a table"
  end
  if options.on_error ~= nil and type(options.on_error) ~= "function" then
    return nil, "Run client on_error must be a function"
  end
  if options.pipe_factory ~= nil and type(options.pipe_factory) ~= "function" then
    return nil, "Run client pipe_factory must be a function"
  end
  if
    options.operator_capability ~= nil
    and (type(options.operator_capability) ~= "string" or options.operator_capability == "")
  then
    return nil, "Run client operator_capability must be a non-empty string"
  end
  local client = setmetatable({
    path = path,
    on_snapshot = on_snapshot,
    on_error = options.on_error,
    pipe_factory = options.pipe_factory or function()
      return nvim.uv.new_pipe(false)
    end,
    buffer = "",
    revisions = {},
    disposed = false,
    operator_capability = options.operator_capability,
    next_request_id = 1,
    pending = {},
  }, Client)
  local pipe = client.pipe_factory()
  if pipe == nil then
    return nil, "could not create Run socket handle"
  end
  client.pipe = pipe
  pipe:connect(path, function(error_message)
    if client.disposed then
      close_pipe(pipe)
      return
    end
    if error_message ~= nil then
      nvim.schedule(function()
        report_error(client, error_message)
      end)
      close_pipe(pipe)
      return
    end
    pipe:read_start(function(read_error, chunk)
      if client.disposed then
        return
      end
      if read_error ~= nil then
        nvim.schedule(function()
          fail_pending(client, read_error)
          report_error(client, read_error)
        end)
      elseif chunk ~= nil then
        consume(client, chunk)
      else
        close_pipe(pipe)
        nvim.schedule(function()
          fail_pending(client, "Run socket disconnected")
          report_error(client, "Run socket disconnected")
        end)
      end
    end)
  end)
  return client
end

local function valid_mutation(id, expected_revision, callback)
  if type(id) ~= "string" or id == "" then
    return false, "Run id must be a non-empty string"
  end
  if type(expected_revision) ~= "number" or expected_revision < 1 or expected_revision % 1 ~= 0 then
    return false, "expected Run revision must be a positive integer"
  end
  if type(callback) ~= "function" then
    return false, "Run mutation callback must be a function"
  end
  return true
end

local function mutate(client, message, callback)
  if client.disposed or client.pipe == nil or client.pipe:is_closing() then
    return false, "Run client is not connected"
  end
  if client.operator_capability == nil then
    return false, "Run client has no operator capability"
  end
  local request_id = tostring(client.next_request_id)
  client.next_request_id = client.next_request_id + 1
  message.request_id = request_id
  message.capability = client.operator_capability
  client.pending[request_id] = callback
  local encoded_ok, encoded = pcall(nvim.json.encode, message)
  if not encoded_ok then
    client.pending[request_id] = nil
    return false, "could not encode Run mutation"
  end
  local write_ok, write_error = pcall(client.pipe.write, client.pipe, encoded .. "\n")
  if not write_ok then
    client.pending[request_id] = nil
    return false, tostring(write_error)
  end
  return true
end

---Raise a Parked Run's generated-work ceiling using operator authority.
function Client:raise(id, expected_revision, ceiling, callback)
  local valid, validation_error = valid_mutation(id, expected_revision, callback)
  if not valid then
    return false, validation_error
  end
  if type(ceiling) ~= "number" or ceiling < 1 or ceiling % 1 ~= 0 then
    return false, "generated-work ceiling must be a positive integer"
  end
  return mutate(self, { type = "raise", id = id, expected_revision = expected_revision, ceiling = ceiling }, callback)
end

---Begin a warm or cold operator resume operation.
function Client:resume(id, expected_revision, operation_id, callback)
  local valid, validation_error = valid_mutation(id, expected_revision, callback)
  if not valid then
    return false, validation_error
  end
  if type(operation_id) ~= "string" or operation_id == "" then
    return false, "resume operation id must be a non-empty string"
  end
  return mutate(
    self,
    { type = "resume", id = id, expected_revision = expected_revision, operation_id = operation_id },
    callback
  )
end

---Finalize an asynchronous cold resume without extending its lease.
function Client:finalize_resume(id, expected_revision, operation_id, succeeded, callback)
  local valid, validation_error = valid_mutation(id, expected_revision, callback)
  if not valid then
    return false, validation_error
  end
  if type(operation_id) ~= "string" or operation_id == "" then
    return false, "resume operation id must be a non-empty string"
  end
  if type(succeeded) ~= "boolean" then
    return false, "resume result must be a boolean"
  end
  return mutate(self, {
    type = "finalize_resume",
    id = id,
    expected_revision = expected_revision,
    operation_id = operation_id,
    succeeded = succeeded,
  }, callback)
end

---Dispose the socket and make queued or late callbacks inert.
---@param self louiselm.workflow.RunClient
---@return boolean disposed
function Client:dispose()
  if self.disposed then
    return true
  end
  self.disposed = true
  fail_pending(self, "Run client is disposed")
  close_pipe(self.pipe)
  self.pipe = nil
  return true
end

return M
