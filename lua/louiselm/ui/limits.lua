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
---@return string[] lines
function M.render(state)
  local lines = {
    "# Account limits",
    "",
    "Agent: " .. state.agent,
    "Status: " .. state.status:gsub("_", " "),
  }
  if state.updated_at ~= nil then
    lines[#lines + 1] = "Updated: " .. os.date("%Y-%m-%d %H:%M:%S %Z", state.updated_at)
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
        lines[#lines + 1] = "Reached: " .. bucket.reached_type
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
  nvim.api.nvim_set_current_buf(buffer)
  return buffer
end

return M
