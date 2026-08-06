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

local chat

nvim.api.nvim_create_user_command("LouiselmChat", function()
  if chat ~= nil and chat:buffer() ~= nil then
    nvim.api.nvim_set_current_buf(chat:buffer())
    return
  end

  local command = nvim.env.LOUISELM_AGENT_COMMAND
  if command == nil or command == "" then
    command = "claude-agent-acp"
  end
  local sessions, session_errors = require("louiselm.session").new({
    default = { command = command, args = {} },
  })
  if sessions == nil then
    nvim.notify("louiselm: invalid agent configuration (" .. #session_errors .. " errors)", nvim.log.levels.ERROR)
    return
  end
  chat = assert(require("louiselm.ui.chat").new(sessions, { agents = { "default" } }))
  local _, session_error = chat:new_session()
  if session_error ~= nil then
    nvim.notify("louiselm: " .. session_error, nvim.log.levels.ERROR)
  end
end, { desc = "Open the louiselm chat buffer" })
