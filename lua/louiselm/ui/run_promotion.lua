---Operator review of an exact Bead promotion preview.
---@diagnostic disable-next-line: undefined-global -- Neovim runtime API.
local nvim = vim
local M = {}

---Show the changed paths and exact digest, then return the operator's choice.
---@param preview table Result of `louiselm-control promotion preview --json`.
---@param decide fun(accepted: boolean) Called on Neovim's main loop.
function M.prompt(preview, decide)
  local lines = {
    "Promote " .. preview.selection.bead_id .. " into run/" .. preview.selection.run_id .. "?",
    "Approval digest: " .. preview.approval_digest,
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
  nvim.ui.select({ "Reject", "Accept exact preview" }, { prompt = table.concat(lines, "\n") }, function(choice)
    decide(choice == "Accept exact preview")
  end)
end

return M
