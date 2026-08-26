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
  { mode = "n", lhs = "<leader>lc", rhs = "<cmd>LouiselmChat<cr>", desc = "LouiSelm chat" },
  { mode = "n", lhs = "<leader>ln", rhs = "<cmd>LouiselmNewSession<cr>", desc = "LouiSelm new session" },
  { mode = "n", lhs = "<leader>lr", rhs = "<cmd>LouiselmResume<cr>", desc = "LouiSelm resume session" },
  { mode = "n", lhs = "<leader>lR", rhs = "<cmd>LouiselmResume!<cr>", desc = "LouiSelm resume any session" },
  { mode = "n", lhs = "<leader>ls", rhs = "<cmd>LouiselmSwitchSession<cr>", desc = "LouiSelm switch session" },
  { mode = "n", lhs = "<leader>lN", rhs = "<cmd>LouiselmRenameSession<cr>", desc = "LouiSelm rename session" },
  { mode = "n", lhs = "<leader>li", rhs = "<cmd>LouiselmSessionId<cr>", desc = "LouiSelm copy session ID" },
  { mode = "n", lhs = "<leader>lI", rhs = "<cmd>LouiselmInbox<cr>", desc = "LouiSelm inbox" },
  { mode = "n", lhs = "<leader>lm", rhs = "<cmd>LouiselmToMarkdown<cr>", desc = "LouiSelm export markdown" },
  { mode = "n", lhs = "<leader>lx", rhs = "<cmd>LouiselmCloseSession<cr>", desc = "LouiSelm close session" },
  { mode = "n", lhs = "<leader>lC", rhs = "<cmd>LouiselmCancel<cr>", desc = "LouiSelm cancel turn" },
  { mode = "n", lhs = "<leader>lo", rhs = "<cmd>LouiselmSessionOptions<cr>", desc = "LouiSelm session options" },
  { mode = "n", lhs = "<leader>lp", rhs = "<cmd>LouiselmPermissions<cr>", desc = "LouiSelm permissions" },
  { mode = "n", lhs = "<leader>lk", rhs = "<cmd>LouiselmPickSkill<cr>", desc = "LouiSelm pick skill" },
  { mode = "n", lhs = "<leader>lf", rhs = "<cmd>LouiselmPickFile<cr>", desc = "LouiSelm pick file" },
  { mode = "n", lhs = "<leader>lb", rhs = "<cmd>LouiselmMentionBuffer<cr>", desc = "LouiSelm mention buffer" },
  { mode = "x", lhs = "<leader>ls", rhs = "<cmd>LouiselmSendSelection<cr>", desc = "LouiSelm send selection" },
  { mode = "n", lhs = "<leader>ll", rhs = "<cmd>LouiselmInline<cr>", desc = "LouiSelm inline edit" },
  { mode = "x", lhs = "<leader>ll", rhs = "<cmd>LouiselmInline<cr>", desc = "LouiSelm inline edit" },
}

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
