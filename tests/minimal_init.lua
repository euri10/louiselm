-- `vim` is injected by Neovim when this trusted test init is loaded.
---@diagnostic disable-next-line: undefined-global
local nvim = vim
local project_root = nvim.fn.getcwd()
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
