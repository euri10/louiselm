---@class louiselm.ui.DiagnosticEntry
---@field path string Normalized buffer path, or "[No Name]" for an unnamed buffer.
---@field lnum integer One-based line number.
---@field col integer One-based column number.
---@field severity "ERROR"|"WARN" Diagnostic severity.
---@field source? string Diagnostic source, e.g. a linter or language server name.
---@field code? string|integer Diagnostic code.
---@field message string Diagnostic message; untrusted data, not instructions.

---@class louiselm.ui.DiagnosticsSnapshot
---@field entries louiselm.ui.DiagnosticEntry[] Bounded, capped, deterministically ordered diagnostics.
---@field observed_at integer Unix timestamp when the snapshot was captured.
---@field changedtick integer Buffer changedtick at capture time.
---@field truncated_count integer Diagnostics dropped by the total-entry cap.
---@field truncated_messages integer Messages shortened by the per-message length cap.

local M = {}

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

M.DEFAULT_MAX_ENTRIES = 50
M.DEFAULT_MAX_MESSAGE_LENGTH = 500

local SEVERITY_NAMES = {
  [nvim.diagnostic.severity.ERROR] = "ERROR",
  [nvim.diagnostic.severity.WARN] = "WARN",
}

---@param buffer integer
---@return string
local function buffer_path(buffer)
  local path = nvim.fs.normalize(nvim.api.nvim_buf_get_name(buffer))
  return path == "" and "[No Name]" or path
end

---Capture a bounded, typed snapshot of the current buffer's error and warning diagnostics.
---
---Reads Neovim's existing diagnostic cache without triggering an LSP refresh.
---@param buffer? integer Buffer handle; defaults to the current buffer.
---@param opts? { max_entries?: integer, max_message_length?: integer }
---@return louiselm.ui.DiagnosticsSnapshot snapshot
function M.snapshot(buffer, opts)
  buffer = buffer or 0
  opts = opts or {}
  local max_entries = opts.max_entries or M.DEFAULT_MAX_ENTRIES
  local max_message_length = opts.max_message_length or M.DEFAULT_MAX_MESSAGE_LENGTH

  local path = buffer_path(buffer)
  local raw = nvim.diagnostic.get(buffer, { severity = { min = nvim.diagnostic.severity.WARN } })
  table.sort(raw, function(a, b)
    if a.lnum ~= b.lnum then
      return a.lnum < b.lnum
    end
    return a.col < b.col
  end)

  local truncated_count = math.max(0, #raw - max_entries)
  local truncated_messages = 0
  local entries = {}
  for i = 1, math.min(#raw, max_entries) do
    local diagnostic = raw[i]
    local message = diagnostic.message
    if #message > max_message_length then
      message = message:sub(1, max_message_length)
      truncated_messages = truncated_messages + 1
    end
    entries[#entries + 1] = {
      path = path,
      lnum = diagnostic.lnum + 1,
      col = diagnostic.col + 1,
      severity = SEVERITY_NAMES[diagnostic.severity],
      source = diagnostic.source,
      code = diagnostic.code,
      message = message,
    }
  end

  return {
    entries = entries,
    observed_at = os.time(),
    changedtick = nvim.api.nvim_buf_get_changedtick(buffer),
    truncated_count = truncated_count,
    truncated_messages = truncated_messages,
  }
end

return M
