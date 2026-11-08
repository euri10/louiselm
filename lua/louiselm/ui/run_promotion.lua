---Operator review of an exact Bead promotion preview.
---@diagnostic disable-next-line: undefined-global -- Neovim runtime API.
local nvim = vim
local M = {}

local function now_ms()
  local seconds, microseconds = nvim.uv.gettimeofday()
  return seconds * 1000 + math.floor(microseconds / 1000)
end

---Focus a scrollable exact-preview review until choice, expiry or cancellation.
---@param preview table Result of `louiselm-control promotion preview --json`.
---@param decide fun(accepted: boolean?, error_message?: string) Main-loop choice; nil stops on expiry.
---@return fun() cancel Releases the UI/timer without making a decision; idempotent.
function M.prompt(preview, decide)
  local expiry_error = "Run promotion preview expired; no changes applied"
  if preview.selection.expires_at_ms <= now_ms() then
    decide(nil, expiry_error)
    return function() end
  end
  local lines = {
    "Promote " .. preview.selection.bead_id .. " into run/" .. preview.selection.run_id .. "?",
    "a: Accept exact preview | r/Esc: Reject | q: Stop Run | scroll to review paths",
    "Time remaining:",
    "Approval digest: " .. preview.approval_digest,
    "Expires at: " .. os.date("!%Y-%m-%d %H:%M:%S UTC", math.floor(preview.selection.expires_at_ms / 1000)),
    "Expiry stops the Run without applying this preview.",
  }
  for _, group in ipairs({ { "added", "+ " }, { "modified", "~ " }, { "deleted", "- " } }) do
    for _, path in ipairs(preview.changes[group[1]] or {}) do
      lines[#lines + 1] = group[2] .. path
    end
  end
  if type(preview.tainted_review) == "table" then
    local review = preview.tainted_review
    lines[#lines + 1] = "Tainted output: " .. review.taint_digest
    lines[#lines + 1] = "Output tree: " .. review.output_digest
    lines[#lines + 1] = "Action: " .. review.action
    lines[#lines + 1] = "Request: " .. review.request_digest
    lines[#lines + 1] = "Destination: UID "
      .. review.destination.uid
      .. ", device "
      .. review.destination.device
      .. ", inode "
      .. review.destination.inode
    lines[#lines + 1] = "Review digest: " .. preview.tainted_review_digest
  end
  local previous = nvim.api.nvim_get_current_win()
  local buffer = nvim.api.nvim_create_buf(false, true)
  nvim.bo[buffer].bufhidden = "wipe"
  nvim.bo[buffer].filetype = "text"
  nvim.api.nvim_buf_set_lines(buffer, 0, -1, false, lines)
  nvim.bo[buffer].modifiable = false
  local width = math.max(1, math.min(100, nvim.o.columns - 4))
  local height = math.max(1, math.min(#lines + 2, nvim.o.lines - 6))
  local window = nvim.api.nvim_open_win(buffer, true, {
    relative = "editor",
    style = "minimal",
    border = "single",
    width = width,
    height = height,
    row = math.max(0, math.floor((nvim.o.lines - height - 2) / 2)),
    col = math.max(0, math.floor((nvim.o.columns - width - 2) / 2)),
    title = " Promotion review ",
    title_pos = "center",
  })
  nvim.wo[window].wrap = true
  local finished, timer = false, nil
  local function cancel(wiping)
    if finished then
      return false
    end
    finished = true
    if timer and not timer:is_closing() then
      timer:stop()
      timer:close()
    end
    timer = nil
    local focused = nvim.api.nvim_get_current_win() == window
    if not wiping and nvim.api.nvim_buf_is_valid(buffer) then
      nvim.api.nvim_buf_delete(buffer, { force = true })
    end
    if not wiping and focused and nvim.api.nvim_win_is_valid(previous) then
      nvim.api.nvim_set_current_win(previous)
    end
    return true
  end
  local function settle(accepted, err, wiping)
    if preview.selection.expires_at_ms <= now_ms() then
      accepted, err = nil, expiry_error
    end
    if cancel(wiping) then
      decide(accepted, err)
    end
  end
  nvim.api.nvim_create_autocmd("BufWipeout", {
    buffer = buffer,
    once = true,
    callback = function()
      settle(false, nil, true)
    end,
  })
  nvim.keymap.set("n", "a", function()
    settle(true)
  end, { buffer = buffer, desc = "Accept exact promotion preview" })
  for _, key in ipairs({ "r", "<Esc>" }) do
    nvim.keymap.set("n", key, function()
      settle(false)
    end, { buffer = buffer, desc = "Reject promotion preview" })
  end
  nvim.keymap.set("n", "q", function()
    settle(nil, "operator stopped promotion review")
  end, { buffer = buffer, desc = "Stop Run without promoting" })
  local function refresh()
    timer = nil
    if finished then
      return
    end
    local remaining = preview.selection.expires_at_ms - now_ms()
    if remaining <= 0 then
      return settle(nil, expiry_error)
    end
    local seconds = math.ceil(remaining / 1000)
    nvim.bo[buffer].modifiable = true
    nvim.api.nvim_buf_set_lines(buffer, 2, 3, false, {
      string.format("Time remaining: %d:%02d", math.floor(seconds / 60), seconds % 60),
    })
    nvim.bo[buffer].modifiable = false
    timer = nvim.defer_fn(refresh, math.min(1000, remaining))
  end
  refresh()
  return function()
    cancel()
  end
end

return M
