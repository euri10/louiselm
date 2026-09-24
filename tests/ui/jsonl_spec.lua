local MiniTest = require("mini.test")
---@diagnostic disable-next-line: undefined-global -- Neovim runtime.
local nvim = vim
local T = MiniTest.new_set()

local function child_view()
  local child = MiniTest.new_child_neovim()
  MiniTest.finally(function()
    child.stop()
  end)
  child.start({ "--noplugin", "-u", "NONE" })
  child.api.nvim_ui_attach(90, 15, { rgb = true })
  child.lua("vim.opt.runtimepath:prepend(...)", { nvim.fn.getcwd() })
  child.lua([[require("louiselm.ui.chat.command").register()]])
  return child
end

local function screen(child)
  local lines = {}
  for _, row in ipairs(child.get_screenshot().text) do
    lines[#lines + 1] = table.concat(row)
  end
  return table.concat(lines, "\n")
end

T["generic summaries preserve malformed and oversized records"] = function()
  local Jsonl = require("louiselm.jsonl")
  MiniTest.expect.equality(
    Jsonl.summary('{"z":false,"id":"a","detail":{"x":1},"nil":null}'),
    "detail={…} · id=a · nil=null · z=false"
  )
  MiniTest.expect.equality(Jsonl.summary('["hello",1,true,null]'), "[hello · 1 · true · null]")
  MiniTest.expect.equality(Jsonl.summary("{}"), "{}")
  MiniTest.expect.equality(Jsonl.summary("[]"), "[]")
  MiniTest.expect.equality(Jsonl.summary("null"), "null")
  MiniTest.expect.equality(Jsonl.summary('{"items":[1,2],"label":"été"}'), "items=[2 items] · label=été")
  MiniTest.expect.equality(Jsonl.summary("[1,2,3,4,5,6,7,8,9]"), "[1 · 2 · 3 · 4 · 5 · 6 · 7 · 8 · …]")
  MiniTest.expect.equality(Jsonl.summary('"hello\\nworld"'), "hello\\nworld")
  MiniTest.expect.equality(Jsonl.summary("{broken"), nil)
  MiniTest.expect.equality(Jsonl.summary('"' .. string.rep("a", 16384) .. '"'), nil)
end

T["command decorates generically and reveals raw cursor record without changing bytes"] = function()
  local child = child_view()
  local lines = { '{"id":"first","status":"open"}', '{"message":"second","count":2}', "not-json" }
  child.api.nvim_buf_set_lines(0, 0, -1, false, lines)
  child.lua("vim.bo.modified = false; vim.wo.wrap = true; vim.wo.conceallevel = 0")
  MiniTest.expect.equality(screen(child):find(lines[2], 1, true) ~= nil, true)
  MiniTest.expect.equality(child.lua_get("vim.wo.wrap"), true)
  child.cmd("LouiselmJsonl")
  local shown = screen(child)
  MiniTest.expect.equality(shown:find(lines[1], 1, true) ~= nil, true)
  MiniTest.expect.equality(shown:find("count=2 · message=second", 1, true) ~= nil, true)
  MiniTest.expect.equality(shown:find("not-json", 1, true) ~= nil, true)
  MiniTest.expect.equality(child.api.nvim_buf_get_lines(0, 0, -1, false), lines)
  MiniTest.expect.equality(child.lua_get("vim.bo.modified"), false)
  child.type_keys("j")
  shown = screen(child)
  MiniTest.expect.equality(shown:find(lines[2], 1, true) ~= nil, true)
  MiniTest.expect.equality(shown:find("id=first · status=open", 1, true) ~= nil, true)
  child.api.nvim_buf_set_lines(0, 0, 1, false, { '{"id":"changed"}' })
  MiniTest.expect.equality(screen(child):find("id=changed", 1, true) ~= nil, true)
  child.lua("vim.wo.concealcursor = 'n'")
  child.cmd("LouiselmJsonl")
  MiniTest.expect.equality(child.lua_get("vim.wo.wrap"), true)
  MiniTest.expect.equality(child.lua_get("vim.wo.conceallevel"), 0)
  MiniTest.expect.equality(child.lua_get("vim.wo.concealcursor"), "n")
  MiniTest.expect.equality(screen(child):find('{"id":"changed"}', 1, true) ~= nil, true)
end

T["window changes and repeated registration release display ownership"] = function()
  local child = child_view()
  child.api.nvim_buf_set_lines(0, 0, -1, false, { '{"a":1}', '{"b":2}', '{"c":3}' })
  child.lua("vim.wo.wrap = true; vim.wo.conceallevel = 0; vim.wo.concealcursor = 'n'")
  child.cmd("LouiselmJsonl")
  child.cmd("vsplit")
  child.type_keys("j")
  MiniTest.expect.equality(screen(child):find("a=1", 1, true) ~= nil, true)
  child.cmd("enew!")
  child.lua("vim.wait(100, function() return vim.wo.wrap end)")
  MiniTest.expect.equality(child.lua_get("vim.wo.wrap"), true)
  child.lua([[require("louiselm.ui.chat.command").register()]])
  child.cmd("wincmd p")
  MiniTest.expect.equality(child.lua_get("vim.wo.wrap"), true)
  MiniTest.expect.equality(child.lua_get("vim.wo.conceallevel"), 0)
  MiniTest.expect.equality(child.lua_get("vim.wo.concealcursor"), "n")
  MiniTest.expect.equality(screen(child):find('{"b":2}', 1, true) ~= nil, true)
end

T["scrolling, visual selection and malformed tails keep the original records accessible"] = function()
  local child = child_view()
  child.lua([[
    local lines = {}
    for index = 1, 2000 do lines[index] = vim.json.encode({record=index, detail={nested=true}}) end
    lines[1999] = "unfinished {"
    vim.api.nvim_buf_set_lines(0,0,-1,false,lines)
    vim.bo.modified = false
  ]])
  child.cmd("LouiselmJsonl")
  child.type_keys("G")
  local shown = screen(child)
  MiniTest.expect.equality(shown:find("record=1998", 1, true) ~= nil, true)
  MiniTest.expect.equality(shown:find("unfinished {", 1, true) ~= nil, true)
  child.type_keys("gg", "Vj")
  shown = screen(child)
  MiniTest.expect.equality(shown:find('"record":2', 1, true) ~= nil, true)
  MiniTest.expect.equality(shown:find("record=", 1, true), nil)
  child.type_keys("y")
  MiniTest.expect.equality(child.lua_get("vim.fn.getreg('0')"):find('"record":2', 1, true) ~= nil, true)
  MiniTest.expect.equality(child.lua_get("vim.bo.modified"), false)
  child.cmd("LouiselmJsonl")
  child.cmd("LouiselmJsonl")
  child.cmd("bwipeout!")
  child.lua([[require("louiselm.ui.chat.command").register()]])
  MiniTest.expect.equality(child.lua_get("vim.wo.wrap"), true)
end

return T
