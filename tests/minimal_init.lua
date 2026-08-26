-- `vim` is injected by Neovim when this trusted test init is loaded.
---@diagnostic disable-next-line: undefined-global
local nvim = vim
local project_root = nvim.fn.getcwd()
-- Persistent stores exercised by tests must never consume or overwrite the
-- developer's real Neovim state (notably the one-shot abandonment breadcrumb).
nvim.env.XDG_STATE_HOME = nvim.fn.tempname()
---@diagnostic disable-next-line: undefined-global
local mini_nvim_path = vim.env.MINI_NVIM_PATH
if mini_nvim_path == nil or mini_nvim_path == "" then
  mini_nvim_path = project_root .. "/.deps/mini.nvim"
end

---@diagnostic disable-next-line: undefined-global
if vim.fn.isdirectory(mini_nvim_path) == 0 then
  error("mini.test is not installed; run ./scripts/install-test-deps or set MINI_NVIM_PATH")
end

---@diagnostic disable-next-line: undefined-global
vim.opt.rtp:prepend(mini_nvim_path)
---@diagnostic disable-next-line: undefined-global
vim.opt.rtp:prepend(project_root)
---@diagnostic disable-next-line: undefined-global
vim.opt.rtp:append(project_root .. "/tests")

-- A headless machine has no clipboard tool, so Neovim registers no provider:
-- writes to the `+` register are silently discarded and reads return "". Tests
-- that assert on clipboard behaviour then measure whether the host has xclip
-- installed rather than what louiselm did, and the resulting failure skips the
-- failing test's inline cleanup -- which is how one missing provider cascaded
-- into a second, unrelated failure in real ACP discovery (louiselm-zjct).
--
-- An in-process provider keeps the register readable and writable everywhere.
-- It also makes the suite hermetic: previously a run clobbered the developer's
-- real system clipboard, which is why callers had to save and restore it.
local clipboard_register = {
  ["+"] = { lines = { "" }, regtype = "v" },
  ["*"] = { lines = { "" }, regtype = "v" },
}

local function clipboard_copy(name)
  return function(lines, regtype)
    clipboard_register[name] = { lines = lines, regtype = regtype }
  end
end

local function clipboard_paste(name)
  return function()
    local entry = clipboard_register[name]
    return entry.lines, entry.regtype
  end
end

nvim.g.clipboard = {
  name = "louiselm-test",
  copy = { ["+"] = clipboard_copy("+"), ["*"] = clipboard_copy("*") },
  paste = { ["+"] = clipboard_paste("+"), ["*"] = clipboard_paste("*") },
}

-- The documented headless command invokes the MiniTest runner by this name.
require("mini.test").setup({
  collect = {
    -- Keep the repository's existing *_spec.lua naming convention.
    find_files = function()
      ---@diagnostic disable-next-line: undefined-global
      return vim.fn.globpath("tests", "**/*_spec.lua", true, true)
    end,
  },
})

require("louiselm.ui.chat.command").register()
require("louiselm.capture.command").register()
