---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

nvim.pack.add({
  {
    src = nvim.fn.getcwd(),
    name = "louiselm.nvim",
  },
}, { confirm = false })
