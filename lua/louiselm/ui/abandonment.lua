---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local M = {}

---@class louiselm.ui.AbandonedSession
---@field agent string Configured Agent name.
---@field acp_session_id? string Agent-side Session identifier.
---@field recoverable boolean Whether ACP session/load is available.
---@field turn_active boolean Whether a turn was interrupted by exit.
---@field staged louiselm.ui.StagedContext Staged context lost at exit.

---@param value unknown
---@return louiselm.ui.AbandonedSession[]? sessions
local function normalize_sessions(value)
  if type(value) ~= "table" then
    return nil
  end
  local sessions = {}
  for index, item in ipairs(value) do
    if
      type(item) ~= "table"
      or type(item.agent) ~= "string"
      or item.agent == ""
      or (item.acp_session_id ~= nil and type(item.acp_session_id) ~= "string")
      or type(item.recoverable) ~= "boolean"
      or type(item.turn_active) ~= "boolean"
      or type(item.staged) ~= "table"
      or type(item.staged.contexts) ~= "number"
      or item.staged.contexts < 0
      or item.staged.contexts % 1 ~= 0
      or type(item.staged.pending_skill) ~= "boolean"
      or type(item.staged.queued_prompt) ~= "boolean"
    then
      return nil
    end
    sessions[index] = {
      agent = item.agent,
      acp_session_id = item.acp_session_id,
      recoverable = item.recoverable,
      turn_active = item.turn_active,
      staged = {
        contexts = item.staged.contexts,
        pending_skill = item.staged.pending_skill,
        queued_prompt = item.staged.queued_prompt,
      },
    }
  end
  if #sessions ~= #value then
    return nil
  end
  return sessions
end

---@param path string
---@param data string
---@return boolean written
---@return string? error_message
local function write_private(path, data)
  local directory = nvim.fs.dirname(path)
  if nvim.fn.mkdir(directory, "p", 448) == 0 and nvim.fn.isdirectory(directory) ~= 1 then
    return false, "could not create abandonment state directory"
  end
  local file, temporary_or_error = nvim.uv.fs_mkstemp(path .. ".tmp-XXXXXX")
  if file == nil then
    return false, "could not create temporary abandonment breadcrumb: " .. tostring(temporary_or_error)
  end
  local temporary = temporary_or_error
  local written, write_error = nvim.uv.fs_write(file, data, 0)
  if written ~= #data then
    nvim.uv.fs_close(file)
    nvim.uv.fs_unlink(temporary)
    return false, "could not write abandonment breadcrumb: " .. tostring(write_error or "short write")
  end
  local synced, sync_error = nvim.uv.fs_fsync(file)
  if not synced then
    nvim.uv.fs_close(file)
    nvim.uv.fs_unlink(temporary)
    return false, "could not sync abandonment breadcrumb: " .. tostring(sync_error)
  end
  local closed, close_error = nvim.uv.fs_close(file)
  if not closed then
    nvim.uv.fs_unlink(temporary)
    return false, "could not close abandonment breadcrumb: " .. tostring(close_error)
  end
  local renamed, rename_error = nvim.uv.fs_rename(temporary, path)
  if not renamed then
    nvim.uv.fs_unlink(temporary)
    return false, "could not publish abandonment breadcrumb: " .. tostring(rename_error)
  end
  return true
end

---Persist the live Sessions ended by editor exit.
---@param path string Breadcrumb JSON path.
---@param sessions unknown Exit snapshot after UI Staged context is composed.
---@return boolean written
---@return string? error_message Validation or filesystem failure.
function M.write(path, sessions)
  if type(path) ~= "string" or path == "" then
    return false, "abandonment path must be a non-empty string"
  end
  local normalized = normalize_sessions(sessions)
  if normalized == nil then
    return false, "abandonment sessions are malformed"
  end
  local encoded = nvim.json.encode({ version = 1, recorded_at = os.time(), sessions = normalized })
  return write_private(nvim.fs.normalize(path), encoded)
end

---@param session louiselm.ui.AbandonedSession
---@return string label
local function session_label(session)
  if session.acp_session_id ~= nil and session.acp_session_id ~= "" then
    return string.format("%s (%s)", session.agent, session.acp_session_id)
  end
  return session.agent
end

---@param sessions louiselm.ui.AbandonedSession[]
---@return string message
local function recovery_message(sessions)
  local recoverable = {}
  local lost = {}
  for _, session in ipairs(sessions) do
    local destination = session.recoverable and recoverable or lost
    destination[#destination + 1] = session_label(session)
  end
  local count = #sessions
  local message = string.format("louiselm: previous exit ended %d live Session%s", count, count == 1 and "" or "s")
  if #recoverable > 0 then
    message = message .. string.format("; resume %s with :LouiselmResume", table.concat(recoverable, ", "))
  end
  if #lost > 0 then
    message = message .. string.format("; %s could not be recovered", table.concat(lost, ", "))
  end
  return message
end

---Read and delete one abandonment breadcrumb, returning its truthful recovery prompt.
---@param path string Breadcrumb JSON path.
---@return string? message Nil when no breadcrumb exists.
---@return string? error_message Read or validation failure; the corrupt breadcrumb is still cleared.
function M.consume(path)
  if type(path) ~= "string" or path == "" then
    return nil, "abandonment path must be a non-empty string"
  end
  path = nvim.fs.normalize(path)
  local stat = nvim.uv.fs_stat(path)
  if stat == nil then
    return nil
  end
  if stat.type ~= "file" then
    return nil, "abandonment breadcrumb is not a regular file"
  end
  local file, open_error = nvim.uv.fs_open(path, "r", 384)
  if file == nil then
    return nil, "could not read abandonment breadcrumb: " .. tostring(open_error)
  end
  local content, read_error = nvim.uv.fs_read(file, stat.size, 0)
  local closed, close_error = nvim.uv.fs_close(file)
  nvim.uv.fs_unlink(path)
  if content == nil then
    return nil, "could not read abandonment breadcrumb: " .. tostring(read_error)
  end
  if not closed then
    return nil, "could not close abandonment breadcrumb: " .. tostring(close_error)
  end
  local decoded_ok, record = pcall(nvim.json.decode, content)
  local sessions = decoded_ok
      and type(record) == "table"
      and record.version == 1
      and normalize_sessions(record.sessions)
    or nil
  if sessions == nil or #sessions == 0 then
    return nil, "abandonment breadcrumb is malformed"
  end
  return recovery_message(sessions)
end

return M
