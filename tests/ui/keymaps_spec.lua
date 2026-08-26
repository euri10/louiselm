local MiniTest = require("mini.test")
local Keymaps = require("louiselm.ui.keymaps")

local T = MiniTest.new_set()
---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim
nvim.g.mapleader = " "

local function delete_global(mode, lhs)
  pcall(nvim.keymap.del, mode, lhs)
end

local function mapping(mode, lhs)
  for _, value in ipairs(nvim.api.nvim_get_keymap(mode)) do
    if value.lhs == lhs then
      return value
    end
  end
end

T["keymaps"] = MiniTest.new_set({
  hooks = {
    post_case = function()
      Keymaps.configure({ keymaps = false })
      for _, lhs in ipairs({
        "<leader>lc",
        "<leader>ln",
        "<leader>ls",
        "<leader>ll",
      }) do
        delete_global("n", lhs)
        delete_global("x", lhs)
      end
    end,
  },
})

T["keymaps"]["installs defaults"] = function()
  Keymaps.configure({})

  MiniTest.expect.equality(mapping("n", " lc").rhs, "<Cmd>LouiselmChat<CR>")
  MiniTest.expect.equality(mapping("x", " ls").rhs, "<Cmd>LouiselmSendSelection<CR>")
  MiniTest.expect.equality(mapping("n", " ll").rhs, "<Cmd>LouiselmInline<CR>")
end

T["keymaps"]["can be disabled"] = function()
  Keymaps.configure({})
  Keymaps.configure({ keymaps = false })

  MiniTest.expect.equality(mapping("n", " lc"), nil)
end

T["keymaps"]["preserves existing mapping"] = function()
  nvim.keymap.set("n", "<leader>lc", "<cmd>OtherCommand<cr>")

  Keymaps.configure({})

  MiniTest.expect.equality(mapping("n", " lc").rhs, "<Cmd>OtherCommand<CR>")
end

return T
