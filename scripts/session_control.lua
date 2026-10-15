-- Data-only RPC client. Waiting happens here, never in the hosting editor.
---@diagnostic disable-next-line: undefined-global -- Executed by Neovim's -l entrypoint.
local nvim = vim
local MAX_INPUT = 1024 * 1024
local USAGE =
  "usage: session-control start FILE|- | batch FILE|- [--timeout-ms N] | discover [AGENT [SESSION [OPTION]]] | inspect ID [--result] | cancel ID | watch ID [--timeout-ms N]"

---@param value table
local function emit(value)
  io.stdout:write(nvim.json.encode(value), "\n")
  io.stdout:flush()
end

---@param message string
local function fail(message)
  emit({ error = message })
  os.exit(1)
end

---@param path string
---@return table? request
---@return string? error_message
local function read_request(path)
  local stream = io.stdin
  if path ~= "-" then
    local stat = nvim.uv.fs_stat(path)
    if stat == nil or stat.type ~= "file" then
      return nil, "request requires a readable JSON file or '-' for stdin"
    end
    if stat.size > MAX_INPUT then
      return nil, "request exceeds 1 MiB"
    end
    local opened, open_error = io.open(path, "rb")
    if opened == nil then
      return nil, open_error ~= nil and "could not open request file" or "request file is unavailable"
    end
    stream = opened
  end
  local data, read_error = stream:read(MAX_INPUT + 1)
  if path ~= "-" then
    local closed, close_error = stream:close()
    if not closed then
      return nil, close_error ~= nil and "could not close request file" or "request file close failed"
    end
  end
  if read_error ~= nil then
    return nil, "could not read request JSON"
  end
  if data == nil or #data > MAX_INPUT then
    return nil, data == nil and "request JSON is empty" or "request exceeds 1 MiB"
  end
  local decoded, request = pcall(nvim.json.decode, data)
  if not decoded or type(request) ~= "table" or not data:match("^%s*{") then
    return nil, "request must be a JSON object"
  end
  return request
end

local arguments = {}
for _, value in ipairs(arg) do
  arguments[#arguments + 1] = value
end
if arguments[1] == "--" then
  table.remove(arguments, 1)
end
local rpc_worker = arguments[1] == "--rpc-worker"
if rpc_worker then
  table.remove(arguments, 1)
end
local action, target = arguments[1], arguments[2]
if action ~= "discover" and (target == nil or target == "") then
  return fail(USAGE)
end
local request
local watch_timeout = 60000
local batch_requests
local is_file = action == "start" or action == "batch"
if is_file and (#arguments == 2 or (action == "batch" and #arguments == 4 and arguments[3] == "--timeout-ms")) then
  local request_error
  request, request_error = read_request(target)
  if request == nil then
    return fail(request_error or "request JSON is unavailable")
  end
  if action == "start" then
    if request.action ~= nil and request.action ~= "start" then
      return fail("start request action must be 'start'")
    end
    request.action = "start"
  else
    -- Only this disposable client loads the installed pure manifest validator.
    nvim.opt.runtimepath:prepend(nvim.fs.dirname(nvim.fs.dirname(nvim.fs.abspath(arg[0]))))
    batch_requests, request_error = require("louiselm.ui.session_batch").expand(request)
    if not batch_requests then
      return fail(request_error or "invalid batch manifest")
    end
    if not rpc_worker and request.branch then
      local branch = nvim
        .system({ "git", "-C", request.cwd, "symbolic-ref", "--quiet", "--short", "HEAD" }, { text = true })
        :wait(5000)
      if branch.code ~= 0 or branch.stdout:gsub("\n$", "") ~= request.branch then
        return fail("branch assertion failed; no Sessions started and no branch switched")
      end
    end
  end
elseif action == "discover" and #arguments <= 4 then
  request = { action = "discover", agent = target, session_id = arguments[3], option_id = arguments[4] }
elseif action == "inspect" and (#arguments == 2 or (#arguments == 3 and arguments[3] == "--result")) then
  request = { action = "inspect", id = target, result = arguments[3] ~= nil and true or nil }
elseif action == "cancel" and #arguments == 2 then
  request = { action = "cancel", id = target }
elseif action == "watch" and (#arguments == 2 or (#arguments == 4 and arguments[3] == "--timeout-ms")) then
  request = { action = "inspect", id = target }
else
  return fail(USAGE)
end
if action == "watch" or action == "batch" then
  if arguments[4] ~= nil then
    local timeout = arguments[4]:match("^%d+$") and tonumber(arguments[4]) or nil
    if timeout == nil or timeout < 1 or timeout > 60000 then
      return fail(action .. " timeout must be an integer from 1 to 60000 milliseconds")
    end
    watch_timeout = math.floor(timeout)
  end
end

local socket = nvim.env.NVIM
if socket == nil or socket == "" then
  fail("NVIM must name the hosting editor's RPC socket")
end

if not rpc_worker then
  -- rpcrequest does not service deadline callbacks while the remote editor is
  -- blocked. Bound this disposable RPC client from a separate Neovim process.
  local command = {
    nvim.v.progpath,
    "--headless",
    "--noplugin",
    "-i",
    "NONE",
    "-u",
    "NONE",
    "-l",
    arg[0],
    "--",
    "--rpc-worker",
  }
  nvim.list_extend(command, is_file and { action, "-" } or arguments)
  if action == "batch" then
    nvim.list_extend(command, { "--timeout-ms", tostring(watch_timeout) })
  end
  local partial, output_error, emitted = "", nil, false
  local received_ids = {}
  local process = nvim.system(command, {
    stdin = is_file and nvim.json.encode(request) or nil,
    -- vim.system otherwise replaces NVIM with this client's own address.
    env = { NVIM = socket },
    stdout = function(err, data)
      if err ~= nil then
        output_error = err
      elseif data ~= nil then
        partial = partial .. data
        while true do
          local newline = partial:find("\n", 1, true)
          if newline == nil then
            break
          end
          local line = partial:sub(1, newline)
          local forward = true
          if action == "batch" then
            local decoded, record = pcall(nvim.json.decode, line)
            if decoded and type(record) == "table" and type(record.id) == "string" then
              received_ids[record.id] = true
            else
              output_error = decoded and type(record) == "table" and record.error or "invalid worker response"
              forward = false
            end
          end
          if forward then
            io.stdout:write(line)
            io.stdout:flush()
            emitted = true
          end
          partial = partial:sub(newline + 1)
        end
      end
    end,
    stderr = false,
  })
  local result = process:wait((action == "watch" or action == "batch") and watch_timeout or 5000)
  if batch_requests then
    local missing = false
    for index, child in ipairs(batch_requests) do
      if not received_ids[child.id] then
        missing = true
        emit({
          id = child.id,
          issue = request.tasks[index].issue,
          outcome = "unknown",
          error = result.code == 124
              and "batch client deadline reached; inspect this original operation ID before retrying; child unchanged"
            or "batch response missing; inspect this original operation ID before retrying; child unchanged",
        })
      end
    end
    os.exit((missing or output_error ~= nil or partial ~= "" or result.code ~= 0) and 1 or 0)
  end
  if result.code == 124 then
    emit({
      id = request.id,
      outcome = "unknown",
      error = action == "watch" and "watch deadline reached; current state unknown; child unchanged"
        or "editor request timed out; outcome unknown; inspect this operation ID before retrying",
    })
    os.exit(1)
  end
  if output_error ~= nil or not emitted or partial ~= "" then
    fail("Session control client failed before returning a complete response")
  end
  os.exit(result.code == 0 and 0 or 1)
end

local connected, channel = pcall(nvim.fn.sockconnect, "pipe", socket, { rpc = true })
if not connected or type(channel) ~= "number" or channel <= 0 then
  fail("could not connect to the editor in NVIM")
end

-- Fixed source: every caller-controlled byte travels in the RPC argument list.
local remote = [[
local loaded, api = pcall(require, "louiselm")
if not loaded or type(api) ~= "table" or type(api.session_control) ~= "function" then
  return { error = "configured Session controls are unavailable; update LouiseLM in the editor" }
end
local snapshot, err = api.session_control(...)
return { snapshot = snapshot, error = err }
]]

---@param input table
---@return table snapshot
local function inspect_or_apply(input)
  local received, reply = pcall(nvim.rpcrequest, channel, "nvim_exec_lua", remote, { input })
  if not received then
    return {
      id = input.id,
      outcome = "unknown",
      error = "editor request failed; inspect this operation ID before retrying",
    }
  end
  if type(reply) ~= "table" then
    return { id = input.id, outcome = "unknown", error = "editor returned an invalid Session control response" }
  end
  if type(reply.error) == "string" then
    return { id = input.id, error = reply.error }
  end
  if type(reply.snapshot) ~= "table" then
    return { id = input.id, outcome = "unknown", error = "editor returned no Session control snapshot" }
  end
  return reply.snapshot
end

local terminal = { completed = true, failed = true, cancelled = true, timed_out = true }
if batch_requests then
  local pending, failed = {}, false
  local deadline = nvim.uv.hrtime() / 1000000 + watch_timeout
  local function observe(index, snapshot)
    if snapshot.error or terminal[snapshot.phase] or snapshot.admission == "dispatched" then
      snapshot.issue = request.tasks[index].issue
      emit(snapshot)
      pending[index] = nil
      failed = failed
        or snapshot.error ~= nil
        or snapshot.phase == "failed"
        or snapshot.phase == "timed_out"
        or snapshot.phase == "cancelled"
    else
      pending[index] = snapshot
    end
  end
  for index, child in ipairs(batch_requests) do
    observe(index, inspect_or_apply(child))
  end
  while next(pending) do
    for index, child in ipairs(batch_requests) do
      if pending[index] then
        observe(index, inspect_or_apply({ action = "inspect", id = child.id }))
      end
    end
    local remaining = math.ceil(deadline - nvim.uv.hrtime() / 1000000)
    if remaining <= 0 then
      for index, child in ipairs(batch_requests) do
        if pending[index] then
          emit({
            id = child.id,
            issue = request.tasks[index].issue,
            outcome = "unknown",
            error = "batch dispatch deadline reached; inspect this original operation ID; child unchanged",
          })
        end
      end
      failed = true
      break
    end
    if next(pending) then
      nvim.wait(math.min(250, remaining), function()
        return false
      end, math.min(250, remaining))
    end
  end
  nvim.fn.chanclose(channel)
  os.exit(failed and 1 or 0)
end

if action ~= "watch" then
  local snapshot = inspect_or_apply(request)
  emit(snapshot)
  nvim.fn.chanclose(channel)
  os.exit(snapshot.error and 1 or 0)
end

local deadline = nvim.uv.hrtime() / 1000000 + watch_timeout
local revision
while true do
  local remaining = math.ceil(deadline - nvim.uv.hrtime() / 1000000)
  if remaining <= 0 then
    emit({
      id = request.id,
      outcome = "unknown",
      error = "watch deadline reached; current state unknown; child unchanged",
    })
    os.exit(1)
  end
  local snapshot = inspect_or_apply(request)
  if snapshot.error then
    emit(snapshot)
    os.exit(1)
  end
  if snapshot.revision ~= revision then
    emit(snapshot)
    revision = snapshot.revision
  end
  if terminal[snapshot.phase] then
    nvim.fn.chanclose(channel)
    os.exit((snapshot.phase == "failed" or snapshot.phase == "timed_out") and 1 or 0)
  end
  remaining = math.ceil(deadline - nvim.uv.hrtime() / 1000000)
  if remaining > 0 then
    nvim.wait(math.min(250, remaining), function()
      return false
    end, math.min(250, remaining))
  end
end
