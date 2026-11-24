---Operator review of an exact Bead promotion preview.
---@diagnostic disable-next-line: undefined-global -- Neovim runtime API.
local nvim = vim
local M = {}

---Focus a scrollable saved-result review until choice or cancellation.
---@param preview table Result of `louiselm-control promotion preview --json`.
---@param decide fun(accepted: boolean?, error_message?: string) Main-loop choice; nil leaves saved work pending.
---@return fun() cancel Releases the UI without making a decision; idempotent.
function M.prompt(preview, decide)
  local lines = {
    "Promote " .. preview.selection.bead_id .. " into run/" .. preview.selection.run_id .. "?",
    "a: Accept exact preview | r: Discard saved result | Esc: Close | q: Stop Run",
    "Approval digest: " .. preview.approval_digest,
    "Saved work remains pending until acceptance or explicit discard.",
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
  local finished = false
  local function cancel(wiping)
    if finished then
      return false
    end
    finished = true
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
    if cancel(wiping) then
      decide(accepted, err)
    end
  end
  nvim.api.nvim_create_autocmd("BufWipeout", {
    buffer = buffer,
    once = true,
    callback = function()
      settle(nil, "operator closed promotion review; saved result remains pending", true)
    end,
  })
  nvim.keymap.set("n", "a", function()
    settle(true)
  end, { buffer = buffer, desc = "Accept exact promotion preview" })
  nvim.keymap.set("n", "r", function()
    settle(false)
  end, { buffer = buffer, desc = "Discard saved result" })
  nvim.keymap.set("n", "<Esc>", function()
    settle(nil, "operator closed promotion review; saved result remains pending")
  end, { buffer = buffer, desc = "Close promotion review" })
  nvim.keymap.set("n", "q", function()
    settle(nil, "operator stopped promotion review")
  end, { buffer = buffer, desc = "Stop Run without promoting" })
  return function()
    cancel()
  end
end

return M
