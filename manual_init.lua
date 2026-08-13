---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim
local project_root = nvim.fn.getcwd()

nvim.pack.add({
  {
    src = project_root,
    name = "louiselm.nvim",
  },
}, { confirm = false })
nvim.opt.rtp:prepend(project_root)

require("louiselm.ui.chat.command").register()
require("louiselm.capture.command").register()
