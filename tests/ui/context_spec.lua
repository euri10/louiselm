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
  local original_stopinsert = nvim.cmd.stopinsert
  local mode = "i"
  local mode_at_select
  local selected
  nvim.cmd.stopinsert = function()
    mode = "n"
  end
  nvim.ui.select = function(items, _, callback)
    mode_at_select = mode
    MiniTest.expect.equality(items, files)
    callback(items[1])
  end
  assert(Context.files.pick(root, function(path)
    selected = path
  end))
  nvim.ui.select = original_select
  nvim.cmd.stopinsert = original_stopinsert

  MiniTest.expect.equality(mode_at_select, "n")
  MiniTest.expect.equality(selected, files[1])
  nvim.fn.delete(root, "rf")
end

T["skills"] = MiniTest.new_set()

T["skills"]["returns the selected skill and turns captured content into an exact context"] = function()
  local root = nvim.fn.tempname()
  assert(nvim.fn.mkdir(root, "p") == 1)
  local path = nvim.fs.joinpath(root, "SKILL.md")
  assert(nvim.fn.writefile({ "old instructions" }, path) == 0)
  local skills = { { name = "grill-me", description = "Stress test", path = path, content = "stale" } }
  local original_select = nvim.ui.select
  local selected
  local kind_at_select
  nvim.ui.select = function(items, opts, callback)
    kind_at_select = opts.kind
    MiniTest.expect.equality(#items, 1)
    MiniTest.expect.equality(items[1].file, path)
    callback(items[1])
  end
  assert(Context.skills.pick(skills, function(skill)
    selected = skill
  end))
  nvim.ui.select = original_select

  MiniTest.expect.equality(kind_at_select, "louiselm_skill")
  MiniTest.expect.equality(selected.name, "grill-me")
  MiniTest.expect.equality(selected.file, path)
  selected.content = "fresh instructions\n"
  MiniTest.expect.equality(Context.skills.context(selected), {
    label = "skill: grill-me",
    text = "fresh instructions\n",
  })
  nvim.fn.delete(root, "rf")
end

T["skills"]["does not read a selected skill before the chat owns the selection"] = function()
  local root = nvim.fn.tempname()
  assert(nvim.fn.mkdir(root, "p") == 1)
  local path = nvim.fs.joinpath(root, "SKILL.md")
  assert(nvim.fn.writefile({ "instructions" }, path) == 0)
  local original_select = nvim.ui.select
  local selected
  nvim.ui.select = function(items, _, callback)
    assert(nvim.fn.delete(path) == 0)
    callback(items[1])
  end
  assert(Context.skills.pick({ { name = "gone", description = "Gone", path = path } }, function(skill)
    selected = skill
  end))
  nvim.ui.select = original_select

  MiniTest.expect.equality(selected.name, "gone")
  nvim.fn.delete(root, "rf")
end

return T
