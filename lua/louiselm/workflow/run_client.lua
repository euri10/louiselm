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

---@class louiselm.workflow.RunClientOptions
---@field on_error? fun(message: string)
---@field pipe_factory? fun(): table Test seam returning a `vim.uv` pipe-compatible handle.

---@class louiselm.workflow.RunClient
---@field path string
---@field on_snapshot fun(runs: louiselm.workflow.RunView[])
---@field on_error? fun(message: string)
---@field pipe_factory fun(): table
---@field pipe? table
---@field buffer string
---@field revisions table<string, integer>
---@field disposed boolean
---@field dispose fun(self: louiselm.workflow.RunClient): boolean

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
          report_error(client, read_error)
        end)
      elseif chunk ~= nil then
        consume(client, chunk)
      else
        close_pipe(pipe)
        nvim.schedule(function()
          report_error(client, "Run socket disconnected")
        end)
      end
    end)
  end)
  return client
end

---Dispose the socket and make queued or late callbacks inert.
---@param self louiselm.workflow.RunClient
---@return boolean disposed
function Client:dispose()
  if self.disposed then
    return true
  end
  self.disposed = true
  close_pipe(self.pipe)
  self.pipe = nil
  return true
end

return M
