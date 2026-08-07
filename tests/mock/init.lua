-- Add the checkout to the child Neovim's runtime path before the mock starts.
---@diagnostic disable-next-line: undefined-global
local nvim = vim
nvim.opt.rtp:prepend(nvim.fn.getcwd())
