---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local M = {}

---@param value number
---@return string
local function percent(value)
  if value > 0 and value < 1 then
    return "<1%"
  end
  return string.format("%g%%", value)
end

---@param minutes integer
---@return string
local function duration(minutes)
  if minutes % 1440 == 0 then
    return string.format("%gd", minutes / 1440)
  end
  if minutes % 60 == 0 then
    return string.format("%gh", minutes / 60)
  end
  return string.format("%gm", minutes)
end

---@param timestamp integer
---@param now integer
---@return string
local function relative_reset(timestamp, now)
  local seconds = math.max(0, timestamp - now)
  if seconds < 3600 then
    return string.format("%dm", math.max(1, math.ceil(seconds / 60)))
  end
  if seconds < 86400 then
    return string.format("%dh", math.ceil(seconds / 3600))
  end
  return string.format("%dd", math.ceil(seconds / 86400))
end

---@param timestamp integer
---@param now integer
---@return string
local function relative_age(timestamp, now)
  local seconds = math.max(0, now - timestamp)
  if seconds < 60 then
    return "just now"
  end
  if seconds < 3600 then
    return string.format("%dm ago", math.floor(seconds / 60))
  end
  if seconds < 86400 then
    return string.format("%dh ago", math.floor(seconds / 3600))
  end
  return string.format("%dd ago", math.floor(seconds / 86400))
end

---@param value number
---@return string
local function remaining_percent(value)
  if value > 0 and value < 1 then
    return "<1%"
  end
  return string.format("%.0f%%", math.max(0, value))
end

---@param bucket louiselm.session.LimitBucket
---@return integer rank
local function bucket_rank(bucket)
  if bucket.reached_type ~= nil then
    return 3
  end
  local rank = 0
  for _, window in ipairs(bucket.windows) do
    local remaining = 100 - window.used_percent
    if remaining <= 10 then
      rank = math.max(rank, 2)
    elseif remaining <= 20 then
      rank = math.max(rank, 1)
    end
  end
  return rank
end

---@param snapshot louiselm.session.LimitsSnapshot
---@return louiselm.session.LimitBucket?
local function summary_bucket(snapshot)
  local default
  local urgent
  local urgent_rank = 0
  for _, bucket in ipairs(snapshot.buckets) do
    if bucket.id == snapshot.default_bucket_id then
      default = bucket
    else
      local rank = bucket_rank(bucket)
      if rank > urgent_rank then
        urgent = bucket
        urgent_rank = rank
      end
    end
  end
  return urgent or default
end

---@param state louiselm.session.LimitsState
---@param now? integer
---@return string? text
---@return string? highlight_group
function M.summary(state, now)
  if state.status == "unsupported" or state.status == "not_observed" then
    return nil, nil
  end
  if state.snapshot == nil then
    if state.status == "loading" then
      return "limits loading", "LouiselmStatusActive"
    end
    return "limits unavailable", "LouiselmStatusError"
  end
  if state.status == "unlimited" or state.snapshot.unlimited then
    return "limits unlimited", "LouiselmStatusReady"
  end
  if state.status == "empty" or #state.snapshot.buckets == 0 then
    return "limits no data", "LouiselmStatusWarning"
  end
  local bucket = summary_bucket(state.snapshot)
  if bucket == nil then
    return nil, nil
  end
  local windows = {}
  for _, window in ipairs(bucket.windows) do
    windows[#windows + 1] = window
  end
  table.sort(windows, function(left, right)
    return left.duration_mins < right.duration_mins
  end)
  local label = (bucket.label or bucket.id):gsub("[%c]", " ")
  local fields = { "limits" }
  if bucket.id ~= state.snapshot.default_bucket_id or label:lower() ~= state.agent:lower() then
    fields[1] = fields[1] .. " " .. label
  end
  for index = 1, math.min(2, #windows) do
    local window = windows[index]
    fields[#fields + 1] = remaining_percent(100 - window.used_percent)
      .. "/"
      .. duration(window.duration_mins)
      .. " ↻"
      .. relative_reset(window.resets_at, now or os.time())
  end
  if #windows > 2 then
    fields[#fields + 1] = "+" .. (#windows - 2)
  end
  if state.status == "stale" or state.status == "loading" then
    fields[#fields + 1] = state.status
  end
  local rank = bucket_rank(bucket)
  local group = rank >= 2 and "LouiselmStatusError" or rank == 1 and "LouiselmStatusWarning" or "LouiselmStatusReady"
  if state.status == "stale" or state.status == "loading" then
    group = "LouiselmStatusWarning"
  end
  return table.concat(fields, " "), group
end

---@class louiselm.ui.LimitsAlert
---@field message string
---@field level "warning"|"error"

---@param state louiselm.session.LimitsState
---@param seen table<string, integer> Highest emitted threshold rank by window/reset cycle.
---@param now? integer
---@return louiselm.ui.LimitsAlert[] alerts
---@return table<string, integer> next_seen
function M.threshold_alerts(state, seen, now)
  if state.status ~= "fresh" or state.snapshot == nil then
    return {}, seen
  end
  local alerts = {}
  local next_seen = {}
  for _, bucket in ipairs(state.snapshot.buckets) do
    local reached_window
    if bucket.reached_type ~= nil then
      for _, window in ipairs(bucket.windows) do
        if reached_window == nil or window.used_percent > reached_window.used_percent then
          reached_window = window
        end
      end
    end
    for _, window in ipairs(bucket.windows) do
      local key = bucket.id .. "\0" .. window.duration_mins .. "\0" .. window.resets_at
      local previous_rank = seen[key] or 0
      local remaining = 100 - window.used_percent
      local rank = window == reached_window and 3 or remaining <= 10 and 2 or remaining <= 20 and 1 or 0
      next_seen[key] = math.max(previous_rank, rank)
      if rank > previous_rank and rank > 0 then
        local label = (bucket.label or bucket.id):gsub("[%c]", " ")
        local prefix = state.agent:gsub("[%c]", " ") .. " " .. label .. " " .. duration(window.duration_mins)
        local reset = "; resets in " .. relative_reset(window.resets_at, now or os.time())
        local message
        if rank == 3 then
          message = prefix .. " limit reached (" .. bucket.reached_type .. ")" .. reset
        else
          message = prefix .. " has " .. remaining_percent(remaining) .. " remaining" .. reset
        end
        alerts[#alerts + 1] = { message = message, level = rank >= 2 and "error" or "warning" }
      end
    end
  end
  return alerts, next_seen
end

---@param timestamp integer
---@return string
local function reset_time(timestamp)
  local seconds = timestamp - os.time()
  local relative
  if seconds <= 0 then
    relative = "reset time passed"
  elseif seconds < 3600 then
    relative = string.format("in %dm", math.max(1, math.ceil(seconds / 60)))
  elseif seconds < 86400 then
    relative = string.format("in %dh", math.ceil(seconds / 3600))
  else
    relative = string.format("in %dd", math.ceil(seconds / 86400))
  end
  return relative .. " (" .. os.date("%Y-%m-%d %H:%M %Z", timestamp) .. ")"
end

---@param state louiselm.session.LimitsState
---@param now? integer Current Unix time for deterministic rendering.
---@return string[] lines
function M.render(state, now)
  local lines = {
    "# Account limits",
    "",
    "Agent: " .. state.agent,
    "Status: " .. state.status:gsub("_", " "),
  }
  if state.updated_at ~= nil then
    lines[#lines + 1] = "Updated: "
      .. os.date("%Y-%m-%d %H:%M:%S %Z", state.updated_at)
      .. " ("
      .. relative_age(state.updated_at, now or os.time())
      .. ")"
  end
  if state.error ~= nil then
    lines[#lines + 1] = "Note: " .. state.error
  end

  local snapshot = state.snapshot
  if snapshot == nil then
    return lines
  end
  if snapshot.unlimited then
    lines[#lines + 1] = "Account limits: unlimited"
  end
  if #snapshot.buckets == 0 then
    lines[#lines + 1] = "Metered buckets: none"
  else
    lines[#lines + 1] = ""
    lines[#lines + 1] = "## Buckets"
    for _, bucket in ipairs(snapshot.buckets) do
      local name = bucket.label or bucket.id
      local suffix = bucket.label ~= nil and " (" .. bucket.id .. ")" or ""
      if bucket.id == snapshot.default_bucket_id then
        suffix = suffix .. " [default]"
      end
      lines[#lines + 1] = ""
      lines[#lines + 1] = "### " .. name .. suffix
      for _, window in ipairs(bucket.windows) do
        local remaining = math.max(0, 100 - window.used_percent)
        lines[#lines + 1] = percent(remaining) .. " left · " .. percent(window.used_percent) .. " used"
        lines[#lines + 1] = "Window: "
          .. duration(window.duration_mins)
          .. " · Resets: "
          .. reset_time(window.resets_at)
      end
      if bucket.reached_type ~= nil then
        local reached = "Reached: " .. bucket.reached_type
        if snapshot.reset_credits ~= nil and snapshot.reset_credits.available_count > 0 then
          reached = reached .. " · Reset credits available: " .. snapshot.reset_credits.available_count
        end
        lines[#lines + 1] = reached
      end
      if bucket.plan_type ~= nil then
        lines[#lines + 1] = "Plan: " .. bucket.plan_type
      end
      if bucket.credits ~= nil then
        if bucket.credits.unlimited then
          lines[#lines + 1] = "Credits: unlimited"
        elseif bucket.credits.balance ~= nil then
          lines[#lines + 1] = "Credits: " .. tostring(bucket.credits.balance)
        end
      end
    end
  end
  if snapshot.reset_credits ~= nil then
    lines[#lines + 1] = ""
    lines[#lines + 1] = "Reset credits: " .. snapshot.reset_credits.available_count
    for _, credit in ipairs(snapshot.reset_credits.credits or {}) do
      local label = credit.title or credit.id or "reset credit"
      if credit.expires_at ~= nil then
        label = label .. " · Expires: " .. reset_time(credit.expires_at)
      end
      lines[#lines + 1] = "- " .. label
      if credit.description ~= nil then
        lines[#lines + 1] = "  " .. credit.description
      end
    end
  end
  return lines
end

---Replace a read-only limits buffer with the current state.
---@param buffer integer
---@param state louiselm.session.LimitsState
---@return boolean updated False when the buffer no longer exists.
function M.update(buffer, state)
  if not nvim.api.nvim_buf_is_valid(buffer) then
    return false
  end
  nvim.api.nvim_set_option_value("modifiable", true, { buf = buffer })
  nvim.api.nvim_buf_set_lines(buffer, 0, -1, false, M.render(state))
  nvim.api.nvim_set_option_value("modifiable", false, { buf = buffer })
  return true
end

---Create and focus a read-only account-limits detail buffer.
---@param agent_name string Configured Agent name.
---@param state louiselm.session.LimitsState Current observation state.
---@return integer buffer
function M.open(agent_name, state)
  local buffer = nvim.api.nvim_create_buf(false, true)
  nvim.api.nvim_buf_set_name(buffer, "louiselm://limits/" .. agent_name)
  nvim.api.nvim_set_option_value("buftype", "nofile", { buf = buffer })
  nvim.api.nvim_set_option_value("bufhidden", "wipe", { buf = buffer })
  nvim.api.nvim_set_option_value("swapfile", false, { buf = buffer })
  nvim.api.nvim_set_option_value("filetype", "louiselm-limits", { buf = buffer })
  M.update(buffer, state)
  local lines = nvim.api.nvim_buf_line_count(buffer)
  local width = math.min(100, math.max(1, nvim.o.columns - 4))
  local height = math.min(lines, math.max(1, nvim.o.lines - 4))
  nvim.api.nvim_open_win(buffer, true, {
    relative = "editor",
    row = 1,
    col = 2,
    width = width,
    height = height,
    style = "minimal",
    border = "rounded",
    title = " Account limits ",
    title_pos = "center",
  })
  local function close()
    if nvim.api.nvim_buf_is_valid(buffer) then
      nvim.api.nvim_buf_delete(buffer, { force = true })
    end
  end
  nvim.keymap.set("n", "q", close, { buffer = buffer, silent = true, nowait = true, desc = "Close account limits" })
  nvim.keymap.set("n", "<Esc>", close, { buffer = buffer, silent = true, nowait = true, desc = "Close account limits" })
  return buffer
end

return M
