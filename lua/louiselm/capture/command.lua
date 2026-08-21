---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local Capture = require("louiselm.capture")

local M = {}
local configured = assert(Capture.new())

---@param message string
---@param level integer
local function notify(message, level)
  nvim.notify("louiselm: " .. message, level)
end

---@param name string
---@param lines string[]
---@return integer buffer
local function open_buffer(name, lines)
  local existing = nvim.fn.bufnr(name)
  if existing ~= -1 and nvim.api.nvim_buf_is_valid(existing) then
    nvim.api.nvim_buf_delete(existing, { force = true })
  end
  local buffer = nvim.api.nvim_create_buf(false, true)
  nvim.api.nvim_buf_set_name(buffer, name)
  nvim.bo[buffer].buftype = "nofile"
  nvim.bo[buffer].bufhidden = "wipe"
  nvim.bo[buffer].swapfile = false
  nvim.bo[buffer].filetype = "markdown"
  nvim.api.nvim_buf_set_lines(buffer, 0, -1, false, lines)
  nvim.bo[buffer].modifiable = false
  nvim.api.nvim_set_current_buf(buffer)
  nvim.wo.wrap = false
  return buffer
end

---@param buffer integer
local function highlight_qr(buffer)
  local namespace = nvim.api.nvim_create_namespace("louiselm_capture_qr")
  nvim.api.nvim_set_hl(0, "LouiselmCaptureQr", { fg = "#000000", bg = "#ffffff" })
  nvim.wo.list = false
  for line = 0, nvim.api.nvim_buf_line_count(buffer) - 1 do
    nvim.api.nvim_buf_add_highlight(buffer, namespace, "LouiselmCaptureQr", line, 0, -1)
  end
end

---@param error_message? string
local function report_error(error_message)
  if error_message ~= nil then
    notify(error_message, nvim.log.levels.ERROR)
  end
end

---@param captures table[]
---@return string[]
local function inbox_lines(captures)
  local lines = { "# LouiseLM capture inbox", "" }
  if #captures == 0 then
    lines[#lines + 1] = "No captures yet. Use `:LouiselmCapture` to start recording."
    return lines
  end
  for _, capture in ipairs(captures) do
    local record = capture.record or {}
    local transcription = (capture.state or {}).transcription or {}
    lines[#lines + 1] = string.format("## %s", record.id or "unknown capture")
    lines[#lines + 1] = string.format(
      "- %s · %s · %s ms",
      transcription.status or "unknown",
      record.source or "unknown",
      tostring(record.duration_ms or "?")
    )
    if capture.audio_path ~= nil then
      lines[#lines + 1] = "- audio: `" .. capture.audio_path .. "`"
    end
    if capture.transcript ~= nil and capture.transcript.text ~= nil then
      lines[#lines + 1] = ""
      lines[#lines + 1] = capture.transcript.text
    elseif transcription.last_error ~= nil then
      lines[#lines + 1] = "- transcription error: " .. transcription.last_error
    end
    lines[#lines + 1] = ""
  end
  return lines
end

---Publish validated capture configuration without starting processes.
---@param config? table Full LouiseLM configuration, or nil for defaults.
---@return boolean configured_ok
---@return string? error_message
function M.configure(config)
  local capture_config = type(config) == "table" and config.capture or nil
  local value, error_message = Capture.new(capture_config)
  if value == nil then
    return false, error_message
  end
  configured = value
  return true
end

---Register the speech-capture workflow commands.
---@return boolean registered
function M.register()
  nvim.api.nvim_create_user_command("LouiselmCapture", function()
    if configured:is_recording() then
      local stopping, error_message = configured:stop()
      if stopping then
        notify("capture stopped; durable ingestion is running", nvim.log.levels.INFO)
      else
        report_error(error_message)
      end
      return
    end
    local id, error_message = configured:start(function(result, completion_error)
      if completion_error ~= nil then
        report_error(completion_error)
      else
        notify("capture " .. result.id .. " is durable", nvim.log.levels.INFO)
      end
    end)
    if id == nil then
      report_error(error_message)
    else
      notify("recording capture " .. id .. "; run :LouiselmCapture again to stop", nvim.log.levels.INFO)
    end
  end, { desc = "Start or stop a durable speech capture", force = true })

  nvim.api.nvim_create_user_command("LouiselmInbox", function()
    local started, error_message = configured:list(function(captures, list_error)
      if captures == nil then
        report_error(list_error)
        return
      end
      open_buffer("louiselm://capture-inbox", inbox_lines(captures))
    end)
    if not started then
      report_error(error_message)
    end
  end, { desc = "Inspect durable captures and transcripts", force = true })

  local function show_status()
    local started, error_message = configured:status(function(status, status_error)
      if status == nil then
        report_error(status_error)
        return
      end
      open_buffer("louiselm://capture-status", {
        "# LouiseLM capture status",
        "",
        "```json",
        nvim.json.encode(status),
        "```",
      })
    end)
    if not started then
      report_error(error_message)
    end
  end

  nvim.api.nvim_create_user_command("LouiselmCaptureSetup", show_status, {
    desc = "Verify capture service setup",
    force = true,
  })
  nvim.api.nvim_create_user_command("LouiselmCaptureStatus", show_status, {
    desc = "Inspect capture and paired-device status",
    force = true,
  })

  nvim.api.nvim_create_user_command("LouiselmCapturePair", function()
    local started, error_message = configured:pair(function(output, pair_error)
      if output == nil then
        report_error(pair_error)
        return
      end
      local buffer = open_buffer("louiselm://capture-pairing", nvim.split(output, "\n", { plain = true }))
      highlight_qr(buffer)
    end)
    if not started then
      report_error(error_message)
    end
  end, { desc = "Create a one-time Android pairing QR", force = true })

  nvim.api.nvim_create_user_command("LouiselmCaptureRevoke", function(arguments)
    local started, error_message = configured:revoke(arguments.args, function(_, revoke_error)
      if revoke_error ~= nil then
        report_error(revoke_error)
      else
        notify("device " .. arguments.args .. " revoked", nvim.log.levels.INFO)
      end
    end)
    if not started then
      report_error(error_message)
    end
  end, { nargs = 1, desc = "Revoke an Android capture device", force = true })

  nvim.api.nvim_create_user_command("LouiselmCaptureRetry", function(arguments)
    local started, error_message = configured:retry(arguments.args, function(_, retry_error)
      if retry_error ~= nil then
        report_error(retry_error)
      else
        notify("capture " .. arguments.args .. " queued for transcription", nvim.log.levels.INFO)
      end
    end)
    if not started then
      report_error(error_message)
    end
  end, { nargs = 1, desc = "Retry capture transcription", force = true })

  return true
end

return M
