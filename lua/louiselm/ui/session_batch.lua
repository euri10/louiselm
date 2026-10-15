-- Manifest expansion only: the existing Session controller owns every child.
local Control = require("louiselm.ui.session_control")
local M = {}
local SHARED = { agent = true, cwd = true, options = true, timeout_ms = true, context = true, parent_turn_id = true }
local TASK = { id = true, name = true, prompt = true, issue = true }

local function fields(value, allowed)
  if type(value) ~= "table" then
    return "must be an object"
  end
  for key in pairs(value) do
    if not allowed[key] then
      return "unknown field " .. tostring(key)
    end
  end
end

---Validate the entire manifest before any RPC or child creation.
---@param manifest unknown Decoded JSON object.
---@return louiselm.SessionControlRequest[]? requests Ordered starts; no claims or new registry.
---@return string? error_message Invalid data, including an invalid later task.
function M.expand(manifest)
  if type(manifest) ~= "table" then
    return nil, "batch must be an object"
  end
  for key in pairs(manifest) do
    if not SHARED[key] and key ~= "tasks" and key ~= "branch" then
      return nil, "batch has unknown field " .. tostring(key)
    end
  end
  if
    manifest.branch ~= nil
    and (
      type(manifest.branch) ~= "string"
      or #manifest.branch == 0
      or #manifest.branch > 256
      or manifest.branch:find("[%z\r\n]")
    )
  then
    return nil, "branch must be a non-empty single line of at most 256 bytes"
  end
  local tasks = manifest.tasks
  if type(tasks) ~= "table" or #tasks < 1 or #tasks > 16 then
    return nil, "tasks must be a dense array of 1 to 16 entries"
  end
  local count = 0
  for key in pairs(tasks) do
    if type(key) ~= "number" or key % 1 ~= 0 or key < 1 or key > #tasks then
      return nil, "tasks must be a dense array of 1 to 16 entries"
    end
    count = count + 1
  end
  if count ~= #tasks then
    return nil, "tasks must be a dense array of 1 to 16 entries"
  end
  local requests, seen = {}, {}
  for index, task in ipairs(tasks) do
    local err = fields(task, TASK)
    if err then
      return nil, "task " .. index .. ": " .. err
    end
    if
      task.issue ~= nil
      and (type(task.issue) ~= "string" or #task.issue == 0 or #task.issue > 128 or task.issue:find("[%z\r\n]"))
    then
      return nil, "task " .. index .. ": issue must be a non-empty single line of at most 128 bytes"
    end
    local request = { action = "start", id = task.id, name = task.name, prompt = task.prompt }
    for key in pairs(SHARED) do
      request[key] = manifest[key]
    end
    local validation_error = Control.validate(request)
    if validation_error then
      return nil, "task " .. index .. ": " .. validation_error
    end
    if seen[request.id] then
      return nil, "duplicate task id " .. request.id
    end
    seen[request.id] = true
    requests[#requests + 1] = request
  end
  return requests
end

return M
