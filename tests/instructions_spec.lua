local MiniTest = require("mini.test")
local Instructions = require("louiselm.instructions")

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim
local temp_dir
local T = MiniTest.new_set({
  hooks = {
    pre_case = function()
      temp_dir = nvim.fs.normalize(nvim.fn.tempname())
      assert(nvim.fn.mkdir(temp_dir, "p") == 1)
    end,
    post_case = function()
      nvim.fn.delete(temp_dir, "rf")
      temp_dir = nil
    end,
  },
})

T["link"] = MiniTest.new_set()

T["link"]["returns a resource-link context item when the file exists"] = function()
  local path = nvim.fs.joinpath(temp_dir, "AGENTS.md")
  assert(nvim.fn.writefile({ "# Contract" }, path) == 0)

  local item = Instructions.link("AGENTS.md", temp_dir)

  MiniTest.expect.equality(item, { label = "AGENTS.md", uri = "file://" .. path })
end

T["link"]["returns nil when the file is absent"] = function()
  local item = Instructions.link("AGENTS.md", temp_dir)

  MiniTest.expect.equality(item, nil)
end

T["link"]["returns nil when the filename is empty"] = function()
  assert(nvim.fn.writefile({ "# Contract" }, nvim.fs.joinpath(temp_dir, "AGENTS.md")) == 0)

  local item = Instructions.link("", temp_dir)

  MiniTest.expect.equality(item, nil)
end

T["link"]["returns nil for a directory of the same name"] = function()
  assert(nvim.fn.mkdir(nvim.fs.joinpath(temp_dir, "AGENTS.md"), "p") == 1)

  local item = Instructions.link("AGENTS.md", temp_dir)

  MiniTest.expect.equality(item, nil)
end

T["link"]["resolves a nested configured filename under the given root"] = function()
  local nested = nvim.fs.joinpath(temp_dir, "docs")
  assert(nvim.fn.mkdir(nested, "p") == 1)
  local path = nvim.fs.joinpath(nested, "AGENTS.md")
  assert(nvim.fn.writefile({ "# Contract" }, path) == 0)

  local item = Instructions.link("docs/AGENTS.md", temp_dir)

  MiniTest.expect.equality(item, { label = "docs/AGENTS.md", uri = "file://" .. path })
end

return T
