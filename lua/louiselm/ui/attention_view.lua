---Read-only presentation of capture-service's durable Attention state.
---@diagnostic disable-next-line: undefined-global -- Neovim runtime.
local nvim = vim
local Client = require("louiselm.workflow.attention_client")
local M = {}
local View = {}
View.__index = View

---@class louiselm.ui.AttentionView
---@field buffer integer Owned scratch buffer.
---@field client? louiselm.workflow.AttentionClient Read-only subscription.
---@field disposed boolean
---@field dispose fun(self: louiselm.ui.AttentionView): boolean

local labels = {
  turn_ready = "Turn ready",
  permission_required = "Permission required",
  run_parked = "Run parked",
  session_failed = "Session failed",
  skill_approval_pending = "Skill approval pending",
  skill_unverified = "Posture unverified",
}

local codes = {
  admission_required = true,
  root_trust_failed = true,
  signature_invalid = true,
  witness_missing = true,
  native_supply_uncertain = true,
  runtime_drift = true,
  isolation_failed = true,
  broker_unavailable = true,
  audit_persistence_unavailable = true,
  provider_disclosure_missing = true,
  evidence_missing = true,
  evidence_invalidated = true,
  unknown_failure = true,
}

local function identifier(value)
  return type(value) == "string" and #value > 0 and #value <= 256 and value:find("[%s%c]") == nil
end

local function write(view, lines)
  if view.disposed or not nvim.api.nvim_buf_is_valid(view.buffer) then
    return
  end
  nvim.bo[view.buffer].modifiable = true
  nvim.api.nvim_buf_set_lines(view.buffer, 0, -1, false, lines)
  nvim.bo[view.buffer].modifiable = false
end

local function render(view, snapshot)
  local lines = { "Attention (read-only)", "Approval, waivers and Resume remain operator actions.", "" }
  for _, item in ipairs(snapshot.items) do
    if
      type(item) ~= "table"
      or labels[item.kind] == nil
      or (item.subject_kind ~= "session" and item.subject_kind ~= "run")
      or not identifier(item.subject_id)
      or not identifier(item.source_operation_id)
      or (item.code ~= nil and codes[item.code] ~= true)
      or (item.kind == "skill_approval_pending" and item.code ~= "admission_required")
      or (item.kind == "skill_unverified" and (item.code == nil or item.code == "admission_required"))
      or (item.linked_run_id ~= nil and item.linked_run_id ~= nvim.NIL and not identifier(item.linked_run_id))
    then
      write(view, { "Attention unavailable: invalid durable item" })
      return
    end
    lines[#lines + 1] = labels[item.kind] .. (item.code and (" — " .. item.code) or "")
    lines[#lines + 1] = "  " .. item.subject_kind .. ": " .. item.subject_id
    lines[#lines + 1] = "  operation: " .. item.source_operation_id
    if type(item.linked_run_id) == "string" then
      lines[#lines + 1] = "  Run: " .. item.linked_run_id
    end
  end
  if #snapshot.items == 0 then
    lines[#lines + 1] = "No unresolved Attention items."
  end
  write(view, lines)
end

---Open a live, read-only scratch view; transport errors appear in the buffer.
---@param socket_path? string Override the configured local state socket for tests.
---@return louiselm.ui.AttentionView? view
---@return string? error_message Invalid socket options or failed pipe allocation.
function M.open(socket_path)
  if socket_path == nil then
    local root = nvim.env.LOUISELM_CAPTURE_STATE_DIR
    if root == nil or root == "" then
      root = nvim.env.XDG_STATE_HOME
    end
    if root == nil or root == "" then
      root = nvim.fs.joinpath(nvim.fn.expand("~"), ".local", "state")
    end
    socket_path = nvim.fs.joinpath(root, "louiselm", "workflow", "attention.sock")
  end
  local view = setmetatable({ buffer = nvim.api.nvim_create_buf(false, true), disposed = false }, View)
  nvim.bo[view.buffer].bufhidden = "wipe"
  nvim.bo[view.buffer].swapfile = false
  nvim.bo[view.buffer].filetype = "louiselm-attention"
  write(view, { "Connecting to durable Attention…" })
  local client, err = Client.connect(socket_path, function(snapshot)
    render(view, snapshot)
  end, {
    on_error = function()
      write(view, { "Attention unavailable. Reopen :LouiselmAttention to reconnect." })
    end,
  })
  if client == nil then
    view:dispose()
    return nil, err
  end
  view.client = client
  nvim.api.nvim_create_autocmd("BufWipeout", {
    buffer = view.buffer,
    once = true,
    callback = function()
      view.disposed = true
      client:dispose()
    end,
  })
  nvim.keymap.set("n", "q", function()
    view:dispose()
  end, { buffer = view.buffer, silent = true })
  nvim.cmd("botright split")
  nvim.api.nvim_win_set_buf(0, view.buffer)
  return view
end

---Close the owned view and subscription; queued callbacks become inert.
---@param self louiselm.ui.AttentionView
---@return boolean disposed
function View:dispose()
  if self.disposed then
    return true
  end
  self.disposed = true
  if self.client ~= nil then
    self.client:dispose()
  end
  if nvim.api.nvim_buf_is_valid(self.buffer) then
    nvim.api.nvim_buf_delete(self.buffer, { force = true })
  end
  return true
end

return M
