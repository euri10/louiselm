---@class louiselm.ui.KeymapDefault
---@field mode string
---@field lhs string
---@field rhs string
---@field desc string

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local M = {}

---@type louiselm.ui.KeymapDefault[]
local DEFAULTS = {
  { mode = "n", lhs = "<leader>lc", rhs = "<cmd>LouiselmChat<cr>", desc = "Louiselm chat" },
  { mode = "n", lhs = "<leader>lr", rhs = "<cmd>LouiselmResume<cr>", desc = "Louiselm resume session" },
  { mode = "n", lhs = "<leader>lR", rhs = "<cmd>LouiselmResume!<cr>", desc = "Louiselm resume any session" },
  { mode = "n", lhs = "<leader>lsn", rhs = "<cmd>LouiselmSessionNew<cr>", desc = "Louiselm session new" },
  { mode = "n", lhs = "<leader>lsw", rhs = "<cmd>LouiselmSessionSwitch<cr>", desc = "Louiselm session switch" },
  { mode = "n", lhs = "<leader>lsr", rhs = "<cmd>LouiselmSessionRename<cr>", desc = "Louiselm session rename" },
  { mode = "n", lhs = "<leader>lsx", rhs = "<cmd>LouiselmSessionClose<cr>", desc = "Louiselm session close" },
  { mode = "n", lhs = "<leader>lsi", rhs = "<cmd>LouiselmSessionId<cr>", desc = "Louiselm session id" },
  { mode = "n", lhs = "<leader>lso", rhs = "<cmd>LouiselmSessionOptions<cr>", desc = "Louiselm session options" },
  { mode = "n", lhs = "<leader>lI", rhs = "<cmd>LouiselmCaptureInbox<cr>", desc = "Louiselm capture inbox" },
  { mode = "n", lhs = "<leader>lm", rhs = "<cmd>LouiselmToMarkdown<cr>", desc = "Louiselm export markdown" },
  { mode = "n", lhs = "<leader>lC", rhs = "<cmd>LouiselmCancel<cr>", desc = "Louiselm cancel turn" },
  { mode = "n", lhs = "<leader>lp", rhs = "<cmd>LouiselmPermissions<cr>", desc = "Louiselm permissions" },
  { mode = "n", lhs = "<leader>lk", rhs = "<cmd>LouiselmPickSkill<cr>", desc = "Louiselm pick skill" },
  { mode = "n", lhs = "<leader>lf", rhs = "<cmd>LouiselmPickFile<cr>", desc = "Louiselm pick file" },
  { mode = "n", lhs = "<leader>lb", rhs = "<cmd>LouiselmMentionBuffer<cr>", desc = "Louiselm mention buffer" },
  { mode = "x", lhs = "<leader>ls", rhs = "<cmd>LouiselmSendSelection<cr>", desc = "Louiselm send selection" },
  { mode = "n", lhs = "<leader>le", rhs = "<cmd>LouiselmInline<cr>", desc = "Louiselm inline edit" },
  { mode = "x", lhs = "<leader>le", rhs = "<cmd>LouiselmInline<cr>", desc = "Louiselm inline edit" },
  { mode = "n", lhs = "<leader>lT", rhs = "<cmd>LouiselmInspectTool<cr>", desc = "Louiselm inspect tool call" },
  { mode = "n", lhs = "<leader>lP", rhs = "<cmd>LouiselmInspectProvenance<cr>", desc = "Louiselm inspect provenance" },
  { mode = "n", lhs = "<leader>lB", rhs = "<cmd>LouiselmInspectBead<cr>", desc = "Louiselm inspect Beads issue" },
  { mode = "n", lhs = "<leader>lH", rhs = "<cmd>LouiselmHandOff<cr>", desc = "Louiselm hand off session" },
  { mode = "n", lhs = "<leader>lF", rhs = "<cmd>LouiselmForensics<cr>", desc = "Louiselm collect forensics" },
  { mode = "n", lhs = "<leader>lV", rhs = "<cmd>LouiselmForensicsView<cr>", desc = "Louiselm view forensics" },
  { mode = "n", lhs = "<leader>lL", rhs = "<cmd>LouiselmLimits<cr>", desc = "Louiselm inspect limits" },
  { mode = "n", lhs = "<leader>ld", rhs = "<cmd>LouiselmDiagnostics<cr>", desc = "Louiselm queue diagnostics" },
  { mode = "n", lhs = "<leader>lz", rhs = "<cmd>LouiselmResumePark<cr>", desc = "Louiselm resume parked run" },
}

---Default mappings installed unless `keymaps` is disabled.
---
---Returned as a copy: the documentation generator reads this table, and a
---caller that edits it would silently change what `setup()` installs.
---@return louiselm.ui.KeymapDefault[] defaults
function M.defaults()
  local copy = {}
  for index, mapping in ipairs(DEFAULTS) do
    copy[index] = { mode = mapping.mode, lhs = mapping.lhs, rhs = mapping.rhs, desc = mapping.desc }
  end
  return copy
end

---@type louiselm.ui.KeymapDefault[]
local installed = {}

---@param left string
---@param right string
---@return boolean
local function rhs_equal(left, right)
  return left:lower() == right:lower()
end

---@param lhs string
---@return string
local function resolved_lhs(lhs)
  local leader = nvim.g.mapleader
  if type(leader) ~= "string" then
    leader = "\\"
  end
  return nvim.api.nvim_replace_termcodes(lhs:gsub("<leader>", leader), true, false, true)
end

---@param mode string
---@param lhs string
---@return table?
local function find_mapping(mode, lhs)
  local target = resolved_lhs(lhs)
  for _, mapping in ipairs(nvim.api.nvim_get_keymap(mode)) do
    if mapping.lhs == target then
      return mapping
    end
  end
  return nil
end

---@param mapping louiselm.ui.KeymapDefault
---@return boolean
local function remove_if_owned(mapping)
  local current = find_mapping(mapping.mode, mapping.lhs)
  if current == nil or not rhs_equal(current.rhs, mapping.rhs) then
    return false
  end
  nvim.keymap.del(mapping.mode, resolved_lhs(mapping.lhs))
  return true
end

local function clear_installed()
  for _, mapping in ipairs(installed) do
    remove_if_owned(mapping)
  end
  installed = {}
end

---@param config table? Validated LouiseLM configuration, or nil to reset.
---@return boolean configured Always true.
function M.configure(config)
  clear_installed()
  if config ~= nil and config.keymaps == false then
    return true
  end

  local skipped = {}
  for _, mapping in ipairs(DEFAULTS) do
    local current = find_mapping(mapping.mode, mapping.lhs)
    if current ~= nil then
      skipped[#skipped + 1] = mapping.lhs
    else
      nvim.keymap.set(mapping.mode, mapping.lhs, mapping.rhs, { silent = true, desc = mapping.desc })
      installed[#installed + 1] = mapping
    end
  end

  if #skipped > 0 then
    table.sort(skipped)
    nvim.notify("louiselm: skipped existing keymap(s): " .. table.concat(skipped, ", "), nvim.log.levels.WARN)
  end
  return true
end

return M
