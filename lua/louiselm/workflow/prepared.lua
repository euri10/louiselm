---Load a published selection for an inactive operator controller.
---@diagnostic disable-next-line: undefined-global -- Neovim supplies the event loop.
local nvim = vim
local Operator = require("louiselm.workflow.operator")
local M = {}

local function closed(value, fields)
  if type(value) ~= "table" then
    return false
  end
  for key in pairs(value) do
    if not nvim.tbl_contains(fields, key) then
      return false
    end
  end
  return true
end

local function private_id(value)
  return type(value) == "string" and #value == 32 and value ~= string.rep("0", 32) and value:match("^[0-9a-f]+$") ~= nil
end

local function run_id(value)
  return type(value) == "string"
    and #value == 36
    and value:match("^[0-9a-f]+%-[0-9a-f]+%-[0-9a-f]+%-[0-9a-f]+%-[0-9a-f]+$") ~= nil
    and value:sub(9, 9) == "-"
    and value:sub(14, 14) == "-"
    and value:sub(19, 19) == "-"
    and value:sub(24, 24) == "-"
end

---Load one current published revision and construct a controller without starting it.
---The callback owns the returned controller. Later execution must recheck this
---identity, the source and the native broker/rehearsal authority.
---@param registration_id string Exact private registration identity.
---@param revision integer Exact current publication revision.
---@param callback fun(controller: louiselm.workflow.BeadExecutor?, identity: table?, error_message: string?, internal_error: string?) Main-loop callback; internal errors may contain private data.
---@param options? louiselm.workflow.OperatorBeadOptions Explicit test seams.
---@return table? pending Cancel the pending owner request with `pending:cancel()`.
---@return string? error_message Invalid input or process-start refusal.
---@return string? internal_error Private process-start detail, if any.
function M.open_registered(registration_id, revision, callback, options)
  if options == nil then
    options = {}
  end
  if
    not private_id(registration_id)
    or type(revision) ~= "number"
    or revision < 1
    or revision > 9007199254740991
    or revision % 1 ~= 0
    or type(callback) ~= "function"
    or not closed(options, { "system", "session_api", "on_promotion" })
    or (options.system ~= nil and type(options.system) ~= "function")
    or (options.on_promotion ~= nil and type(options.on_promotion) ~= "function")
  then
    return nil, "invalid registered Run request"
  end
  local active = true
  ---@type { kill: fun(self: table, signal: integer) }?
  local job
  local pending = {}
  function pending:cancel()
    if not active then
      return true
    end
    active = false
    local current = job
    if current == nil then
      return false, "registered Run process is unavailable"
    end
    local ok, kill_error = pcall(current.kill, current, 15)
    if not ok then
      return false, "could not cancel registered Run request: " .. tostring(kill_error)
    end
    return true
  end
  local command = {
    "louiselm-capture",
    "--require-interface=1",
    "phone-control",
    "open-prepared",
    "--registration-id",
    registration_id,
    "--revision",
    tostring(revision),
  }
  local stdout, stderr, problem = {}, {}, nil
  local function stream(parts, limit)
    local size = 0
    return function(read_error, data)
      if not active or problem then
        return
      end
      if read_error ~= nil then
        problem = "stream read failed: " .. tostring(read_error)
      elseif data ~= nil then
        size = size + #data
        if size > limit then
          problem = "subprocess output exceeded its limit"
        else
          parts[#parts + 1] = data
        end
      end
      if problem and job then
        local killed, kill_error = pcall(job.kill, job, 15)
        if not killed then
          problem = problem .. "; cancellation failed: " .. tostring(kill_error)
        end
      end
    end
  end
  local started, process = pcall(
    options.system or nvim.system,
    command,
    { text = true, timeout = 5000, stdout = stream(stdout, 262145), stderr = stream(stderr, 8192) },
    function(result)
      nvim.schedule(function()
        if not active then
          return
        end
        active = false
        if problem then
          callback(nil, nil, "registered Run response is invalid", problem)
          return
        end
        if type(result) ~= "table" or result.code ~= 0 then
          local unsupported = table.concat(stderr):find("phone-control request refused: unsupported", 1, true) ~= nil
          callback(nil, nil, unsupported and "unsupported" or "registered Run is unavailable")
          return
        end
        local decoded, response = pcall(nvim.json.decode, table.concat(stdout))
        if not decoded then
          callback(nil, nil, "registered Run response is invalid", tostring(response))
          return
        end
        local binding = decoded and type(response) == "table" and response.binding or nil
        if type(binding) ~= "table" then
          callback(nil, nil, "registered Run response is invalid")
          return
        end
        ---@cast binding table<string, any>
        if
          not closed(response, { "v", "type", "registration_id", "revision", "binding", "selection" })
          or response.v ~= 1
          or response.type ~= "prepared_waiting"
          or response.registration_id ~= registration_id
          or response.revision ~= revision
          or not closed(binding, { "run_id", "project_id", "source_id", "checkout_id", "agents" })
          or not run_id(binding.run_id)
          or not private_id(binding.project_id)
          or not private_id(binding.source_id)
          or not private_id(binding.checkout_id)
          or type(binding.agents) ~= "table"
          or not nvim.islist(binding.agents)
          or #binding.agents ~= 1
          or type(binding.agents[1]) ~= "string"
          or type(response.selection) ~= "string"
          or #response.selection > 65536
        then
          callback(nil, nil, "registered Run response is invalid")
          return
        end
        local parsed, selection = pcall(nvim.json.decode, response.selection)
        if not parsed then
          callback(nil, nil, "registered Run selection is invalid", tostring(selection))
          return
        end
        if
          type(selection) ~= "table"
          or type(selection.envelope) ~= "table"
          or selection.envelope.run_id ~= binding.run_id
          or type(selection.manifest) ~= "table"
          or type(selection.manifest.agent) ~= "table"
          or selection.manifest.agent.id ~= binding.agents[1]
        then
          callback(nil, nil, "registered Run identity is invalid")
          return
        end
        local constructed, controller, err = pcall(Operator.new, selection, options)
        if not constructed or not controller then
          callback(
            nil,
            nil,
            constructed and err or "registered Run selection is invalid",
            not constructed and tostring(controller) or nil
          )
          return
        end
        callback(controller, { registration_id = registration_id, revision = revision, run_id = binding.run_id }, nil)
      end)
    end
  )
  if not started or type(process) ~= "table" or type(process.kill) ~= "function" then
    active = false
    return nil, "could not start registered Run request", not started and tostring(process) or nil
  end
  job = process
  return pending
end

return M
