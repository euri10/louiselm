---Asynchronous access to the native owner's payload-free action journal.

---@diagnostic disable-next-line: undefined-global -- Neovim owns processes and scheduling.
local nvim = vim
local M = {}
local Journal = {}
Journal.__index = Journal

local function keys(value, expected)
  if type(value) ~= "table" then
    return false
  end
  local count = 0
  for name in pairs(value) do
    if not expected[name] then
      return false
    end
    count = count + 1
  end
  local wanted = 0
  for _ in pairs(expected) do
    wanted = wanted + 1
  end
  return count == wanted
end

local function digest(value)
  return type(value) == "string" and #value == 64 and value:match("^[0-9a-f]+$") ~= nil
end

local function outcome_name(outcome)
  if outcome == "unknown" then
    return "unknown"
  end
  if type(outcome) ~= "table" then
    return nil
  end
  for _, name in ipairs({ "accepted", "dispatched", "completed", "rejected", "fenced_no_effect" }) do
    local item = outcome[name]
    if keys(outcome, { [name] = true }) and keys(item, { evidence = true }) and digest(item.evidence) then
      return name
    end
  end
  return nil
end

local function parse(output)
  if type(output) ~= "string" or #output > 1024 or output:find("\\", 1, true) then
    return nil
  end
  local seen = {}
  for name in output:gmatch('"([a-z_]+)"%s*:') do
    if seen[name] then
      return nil
    end
    seen[name] = true
  end
  if not output:match('"v":1[,}]') then
    return nil
  end
  local ok, value = pcall(nvim.json.decode, output)
  if not ok or type(value) ~= "table" or value.v ~= 1 then
    return nil
  end
  if value.type == "begun" or value.type == "recorded" or value.type == "uncertain" then
    return keys(value, { v = true, type = true }) and value or nil
  end
  if value.type == "fenced" then
    return keys(value, { v = true, type = true, evidence = true }) and digest(value.evidence) and value or nil
  end
  if value.type == "error" then
    local valid = { malformed = true, conflict = true, unavailable = true }
    return keys(value, { v = true, type = true, code = true }) and valid[value.code] and value or nil
  end
  if value.type == "action_phases" then
    if
      not keys(value, {
        v = true,
        type = true,
        outcome = true,
        accepted_evidence = true,
        dispatched_evidence = true,
        completed_evidence = true,
      })
    then
      return nil
    end
    local name = outcome_name(value.outcome)
    for _, field in ipairs({ "accepted_evidence", "dispatched_evidence", "completed_evidence" }) do
      if value[field] ~= "" and not digest(value[field]) then
        return nil
      end
    end
    if name == nil then
      return nil
    elseif name == "accepted" or name == "dispatched" or name == "completed" then
      if value[name .. "_evidence"] ~= value.outcome[name].evidence then
        return nil
      end
      if name == "accepted" and (value.dispatched_evidence ~= "" or value.completed_evidence ~= "") then
        return nil
      elseif name == "dispatched" and value.completed_evidence ~= "" then
        return nil
      end
    elseif name == "unknown" then
      if value.completed_evidence ~= "" then
        return nil
      end
    elseif value.accepted_evidence ~= "" or value.dispatched_evidence ~= "" or value.completed_evidence ~= "" then
      return nil
    end
    return value
  end
  if value.type ~= "existing" or not keys(value, { v = true, type = true, outcome = true }) then
    return nil
  end
  return outcome_name(value.outcome) and value or nil
end

local function environment()
  local env = {}
  for _, name in ipairs({
    "PATH",
    "HOME",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "XDG_CACHE_HOME",
    "LANG",
    "LOUISELM_CAPTURE_CONFIG_DIR",
    "LOUISELM_CAPTURE_DATA_DIR",
    "LOUISELM_CAPTURE_STATE_DIR",
  }) do
    if nvim.env[name] ~= nil then
      env[name] = nvim.env[name]
    end
  end
  return env
end

---Run one closed owner-journal request; failures never imply non-delivery.
---@param message table Payload-free, versioned owner-journal command.
---@param callback fun(result: table) Called once on the main loop unless disposed.
function Journal:run(message, callback)
  local function unavailable()
    if not self.disposed then
      callback({ v = 1, type = "error", code = "unavailable" })
    end
  end
  if self.disposed then
    return
  end
  local encoded, input = pcall(nvim.json.encode, message)
  if not encoded or #input + 1 > 262144 then
    nvim.schedule(unavailable)
    return
  end
  local argv = nvim.list_extend(nvim.deepcopy(self.service), { "phone-control", "owner-journal" })
  local process
  local spawned, result = pcall(nvim.system, argv, {
    stdin = input .. "\n",
    text = true,
    timeout = 5000,
    cwd = nvim.uv.cwd(),
    clear_env = true,
    env = environment(),
  }, function(response)
    nvim.schedule(function()
      self.processes[process] = nil
      if self.disposed then
        return
      end
      local value = response.code == 0 and parse(response.stdout) or nil
      callback(value or { v = 1, type = "error", code = "unavailable" })
    end)
  end)
  if not spawned or result == nil then
    nvim.schedule(unavailable)
    return
  end
  process = result
  self.processes[process] = true
end

---Stop outstanding helpers and suppress their late callbacks.
---@return boolean disposed
---@return string? error_message Cancellation failure.
function Journal:dispose()
  if self.disposed then
    return true
  end
  self.disposed = true
  local failed = false
  for process in pairs(self.processes) do
    local stopped, cause = pcall(process.kill, process, 15)
    if not stopped then
      self.last_error = cause
      failed = true
    end
  end
  self.processes = {}
  return not failed, failed and "could not stop action journal" or nil
end

---Create an explicitly owned journal client; construction performs no I/O.
---@param options {service: string[]} Configured capture-service argv prefix.
---@return table journal
function M.new(options)
  return setmetatable({ service = nvim.deepcopy(options.service), processes = {}, disposed = false }, Journal)
end

return M
