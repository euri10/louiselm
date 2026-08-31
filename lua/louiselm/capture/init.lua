---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

---@class louiselm.capture.Config
---@field recorder? string[] Recorder argv containing one `{output}` placeholder.
---@field service? string[] Capture-service argv prefix.

---@class louiselm.capture.Result
---@field id string Capture UUID.
---@field outcome string Ingestion outcome.

---@class louiselm.capture.Instance
---@field private recorder string[]
---@field private service string[]
---@field private recording? table
local Capture = {}
Capture.__index = Capture

local M = {}
local DEFAULT_RECORDER = { "pw-record", "{output}" }
local DEFAULT_SERVICE = { "louiselm-capture" }

---@param value unknown
---@param label string
---@param require_output boolean
---@return string[]? command
---@return string? error_message
local function validate_command(value, label, require_output)
  if type(value) ~= "table" or #value == 0 then
    return nil, label .. " must be a non-empty argv array"
  end
  for index = 1, #value do
    if value[index] == nil then
      return nil, label .. " must be a dense array of strings"
    end
  end
  local output_count = 0
  for key, argument in pairs(value) do
    if type(key) ~= "number" or key < 1 or key % 1 ~= 0 or key > #value or type(argument) ~= "string" then
      return nil, label .. " must be a dense array of strings"
    end
    if argument == "{output}" then
      output_count = output_count + 1
    end
  end
  if value[1] == "" then
    return nil, label .. " must start with an executable"
  end
  if require_output and output_count ~= 1 then
    return nil, label .. " must contain {output} exactly once"
  end
  return value
end

---@param value unknown
---@return string
local function process_error(value)
  if type(value) ~= "string" then
    return "process failed"
  end
  local message = value:gsub("^%s+", ""):gsub("%s+$", "")
  if message == "" then
    return "process failed"
  end
  return message:sub(1, 300)
end

---@return string
local function capture_id()
  local seed = table.concat({ tostring(nvim.uv.hrtime()), tostring(nvim.fn.getpid()), nvim.fn.tempname() }, ":")
  local hex = nvim.fn.sha256(seed)
  local variant = string.format("%x", 8 + (tonumber(hex:sub(17, 17), 16) % 4))
  return table.concat({
    hex:sub(1, 8),
    hex:sub(9, 12),
    "4" .. hex:sub(14, 16),
    variant .. hex:sub(18, 20),
    hex:sub(21, 32),
  }, "-")
end

---@param command string[]
---@param output string
---@return string[]
local function recorder_command(command, output)
  local result = {}
  for index, argument in ipairs(command) do
    result[index] = argument == "{output}" and output or argument
  end
  return result
end

---@param callback? fun(result?: unknown, error_message?: string)
---@param result? unknown
---@param error_message? string
local function complete(callback, result, error_message)
  if callback ~= nil then
    callback(result, error_message)
  end
end

---@param arguments string[]
---@param decode_json boolean
---@param callback fun(result?: unknown, error_message?: string)
---@return boolean started
---@return string? error_message
function Capture:run_service(arguments, decode_json, callback)
  local command = nvim.deepcopy(self.service)
  for _, argument in ipairs(arguments) do
    command[#command + 1] = argument
  end
  local call_ok, handle_or_error = pcall(nvim.system, command, { text = true }, function(result)
    nvim.schedule(function()
      if result.code ~= 0 then
        complete(callback, nil, process_error(result.stderr))
        return
      end
      if not decode_json then
        complete(callback, result.stdout or "")
        return
      end
      local decoded_ok, decoded = pcall(nvim.json.decode, result.stdout or "")
      if not decoded_ok then
        complete(callback, nil, "capture service returned malformed JSON")
        return
      end
      complete(callback, decoded)
    end)
  end)
  if not call_ok then
    return false, tostring(handle_or_error)
  end
  return handle_or_error ~= nil, handle_or_error == nil and "capture service did not start" or nil
end

---@param entry table
---@param result table
local function recorder_exited(entry, result)
  local self = entry.owner
  if self.recording ~= entry then
    return
  end
  self.recording = nil
  -- `pw-record` exits with code 1, rather than signal 2, after SIGINT.
  local stopped_normally = entry.stopping and result.signal == 0 and result.code == 1 and result.stderr == ""
  local succeeded = result.code == 0 or (entry.stopping and result.signal == 2) or stopped_normally
  if not succeeded then
    complete(entry.callback, nil, "recorder failed: " .. process_error(result.stderr))
    return
  end
  if nvim.uv.fs_stat(entry.output) == nil then
    complete(entry.callback, nil, "recorder produced no audio file")
    return
  end

  local duration_ms = math.max(1, math.floor((nvim.uv.hrtime() - entry.started_hrtime) / 1000000))
  local arguments = {
    "ingest-local",
    "--file",
    entry.output,
    "--id",
    entry.id,
    "--recorded-at-ms",
    tostring(entry.recorded_at_ms),
    "--duration-ms",
    tostring(duration_ms),
    "--mime",
    "audio/wav",
  }
  local started, service_error = self:run_service(arguments, true, function(value, error_message)
    if error_message == nil then
      nvim.uv.fs_unlink(entry.output)
    end
    complete(entry.callback, value, error_message)
  end)
  if not started then
    complete(entry.callback, nil, service_error)
  end
end

---Create an isolated capture controller without starting processes.
---@param config? louiselm.capture.Config
---@return louiselm.capture.Instance? capture
---@return string? error_message
function M.new(config)
  config = config or {}
  if type(config) ~= "table" then
    return nil, "capture configuration must be a table"
  end
  for key in pairs(config) do
    if key ~= "recorder" and key ~= "service" then
      return nil, "unknown capture configuration key: " .. tostring(key)
    end
  end
  local recorder, recorder_error = validate_command(config.recorder or DEFAULT_RECORDER, "capture recorder", true)
  if recorder == nil then
    return nil, recorder_error
  end
  local service, service_error = validate_command(config.service or DEFAULT_SERVICE, "capture service", false)
  if service == nil then
    return nil, service_error
  end
  local capture = setmetatable({
    recorder = nvim.deepcopy(recorder),
    service = nvim.deepcopy(service),
  }, Capture)
  return capture, nil
end

---Start recording into a private temporary WAV file.
---@param callback? fun(result?: louiselm.capture.Result, error_message?: string) Terminal ingestion result.
---@return string? id Capture UUID, or nil when recording cannot start.
---@return string? error_message Immediate failure.
function Capture:start(callback)
  if self.recording ~= nil then
    return nil, "a capture is already recording"
  end
  local directory = nvim.fn.stdpath("state") .. "/louiselm/capture-recordings"
  if nvim.fn.mkdir(directory, "p") == 0 and nvim.fn.isdirectory(directory) == 0 then
    return nil, "capture recording directory could not be created"
  end
  local private, permission_error = nvim.uv.fs_chmod(directory, 448)
  if not private then
    return nil, "capture recording directory could not be made private: " .. tostring(permission_error)
  end
  local id = capture_id()
  local output = directory .. "/" .. id .. ".wav"
  local entry = {
    owner = self,
    id = id,
    output = output,
    recorded_at_ms = (function()
      local seconds, microseconds = nvim.uv.gettimeofday()
      return seconds * 1000 + math.floor(microseconds / 1000)
    end)(),
    started_hrtime = nvim.uv.hrtime(),
    callback = callback,
    stopping = false,
  }
  local call_ok, handle_or_error = pcall(
    nvim.system,
    recorder_command(self.recorder, output),
    { text = true },
    function(result)
      nvim.schedule(function()
        recorder_exited(entry, result)
      end)
    end
  )
  if not call_ok or handle_or_error == nil then
    return nil, call_ok and "recorder did not start" or tostring(handle_or_error)
  end
  entry.handle = handle_or_error
  self.recording = entry
  return id
end

---Stop the active recording; ingestion begins after the recorder exits.
---@param callback? fun(result?: louiselm.capture.Result, error_message?: string) Overrides the start callback.
---@return boolean stopping
---@return string? error_message
function Capture:stop(callback)
  local entry = self.recording
  if entry == nil then
    return false, "no capture is recording"
  end
  if entry.stopping then
    return false, "capture is already stopping"
  end
  if callback ~= nil then
    entry.callback = callback
  end
  entry.stopping = true
  local killed, kill_error = pcall(entry.handle.kill, entry.handle, 2)
  if not killed then
    entry.stopping = false
    return false, tostring(kill_error)
  end
  return true
end

---Whether this controller currently owns a live recorder process.
---@return boolean recording
function Capture:is_recording()
  return self.recording ~= nil
end

---List durable captures and their transcription state.
---@param callback fun(captures?: table[], error_message?: string)
---@return boolean started
---@return string? error_message
function Capture:list(callback)
  return self:run_service({ "list" }, true, callback)
end

---Read service and pairing status.
---@param callback fun(status?: table, error_message?: string)
---@return boolean started
---@return string? error_message
function Capture:status(callback)
  return self:run_service({ "status" }, true, callback)
end

---Create a one-time Android pairing offer.
---@param callback fun(output?: string, error_message?: string)
---@return boolean started
---@return string? error_message
function Capture:pair(callback)
  return self:run_service({ "pair" }, false, callback)
end

---Revoke a paired Android device credential.
---@param id string Device UUID shown by `status`.
---@param callback fun(output?: string, error_message?: string)
---@return boolean started
---@return string? error_message
function Capture:revoke(id, callback)
  return self:run_service({ "revoke-device", id }, false, callback)
end

---Retry a capture whose transcription needs operator action.
---@param id string
---@param callback fun(output?: string, error_message?: string)
---@return boolean started
---@return string? error_message
function Capture:retry(id, callback)
  return self:run_service({ "retry", id }, false, callback)
end

return M
