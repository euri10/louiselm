local MiniTest = require("mini.test")
local Context = require("louiselm.ui.context")

local T = MiniTest.new_set()

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

T["buffer"] = MiniTest.new_set()

T["buffer"]["mentions the current named buffer"] = function()
  local buffer = nvim.api.nvim_create_buf(false, true)
  nvim.api.nvim_buf_set_name(buffer, nvim.fs.joinpath(nvim.fn.getcwd(), "lua", "init.lua"))
  nvim.api.nvim_set_current_buf(buffer)

  MiniTest.expect.equality(Context.buffer(), {
    label = "buffer: " .. nvim.fs.joinpath(nvim.fn.getcwd(), "lua", "init.lua"),
    text = "Current buffer: " .. nvim.fs.joinpath(nvim.fn.getcwd(), "lua", "init.lua"),
  })
  nvim.api.nvim_buf_delete(buffer, { force = true })
end

T["selection"] = MiniTest.new_set()

T["selection"]["captures visual text and its location"] = function()
  local buffer = nvim.api.nvim_create_buf(false, true)
  nvim.api.nvim_buf_set_name(buffer, "/tmp/context.lua")
  nvim.api.nvim_buf_set_lines(buffer, 0, -1, false, { "local first = 1", "local second = 2" })
  nvim.api.nvim_set_current_buf(buffer)
  nvim.api.nvim_buf_set_mark(buffer, "<", 1, 6, {})
  nvim.api.nvim_buf_set_mark(buffer, ">", 2, 11, {})

  local selection = assert(Context.selection())
  MiniTest.expect.equality(selection.label, "selection: /tmp/context.lua:1-2")
  MiniTest.expect.equality(selection.text, "Selection from /tmp/context.lua:1-2\n```\nfirst = 1\nlocal second\n```")
  nvim.api.nvim_buf_delete(buffer, { force = true })
end

T["files"] = MiniTest.new_set()

T["files"]["lists files and picks one through the native picker"] = function()
  local root = nvim.fn.tempname()
  assert(nvim.fn.mkdir(nvim.fs.joinpath(root, "nested"), "p") == 1)
  nvim.fn.writefile({ "one" }, nvim.fs.joinpath(root, "one.txt"))
  nvim.fn.writefile({ "two" }, nvim.fs.joinpath(root, "nested", "two.txt"))

  local files = assert(Context.files.list(root))
  MiniTest.expect.equality(files, {
    nvim.fs.joinpath(root, "nested", "two.txt"),
    nvim.fs.joinpath(root, "one.txt"),
  })

  local original_select = nvim.ui.select
  local selected
  nvim.ui.select = function(items, _, callback)
    MiniTest.expect.equality(items, files)
    callback(items[1])
  end
  assert(Context.files.pick(root, function(path)
    selected = path
  end))
  nvim.ui.select = original_select

  MiniTest.expect.equality(selected, files[1])
  nvim.fn.delete(root, "rf")
end

T["skills"] = MiniTest.new_set()

T["skills"]["picks and turns a skill into an invocation context"] = function()
  local skills = { { name = "grill-me", description = "Stress test", path = "/skills/grill-me/SKILL.md" } }
  local original_select = nvim.ui.select
  local selected
  nvim.ui.select = function(items, _, callback)
    MiniTest.expect.equality(items, skills)
    callback(items[1])
  end
  assert(Context.skills.pick(skills, function(skill)
    selected = Context.skills.context(skill)
  end))
  nvim.ui.select = original_select

  MiniTest.expect.equality(selected, {
    label = "skill: grill-me",
    text = "/grill-me",
  })
end

return T
