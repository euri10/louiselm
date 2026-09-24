local MiniTest = require("mini.test")
local Buffer = require("louiselm.ui.chat.buffer")

---@diagnostic disable-next-line: undefined-global -- Neovim injects its runtime API.
local nvim = vim
local T = MiniTest.new_set()

local function chat_buffer(text, highlighting)
  local state = { id = "table-test", agent = "test", status = "ready", config_options = {} }
  local owner = Buffer.new(state, {
    markdown_highlighting = highlighting ~= false,
    on_prompt_edit = function() end,
    prompt_prefix = function()
      return ""
    end,
    on_enter = function() end,
    submit = function() end,
  })
  MiniTest.finally(function()
    owner:dispose()
  end)
  owner:show(nvim.api.nvim_get_current_win(), false)
  owner:render({ type = "chunk", session_id = "table-test", data = { text = text } })
  return owner
end

local function decorations(owner)
  return nvim.api.nvim_buf_get_extmarks(
    owner.buffer,
    nvim.api.nvim_create_namespace("louiselm.chat.tables"),
    0,
    -1,
    { details = true }
  )
end

local function wait_for_tables(owner)
  MiniTest.expect.equality(
    nvim.wait(1000, function()
      return #decorations(owner) > 0
    end, 1),
    true
  )
  return decorations(owner)
end

T["aligns parsed tables without changing source or editable prompts"] = function()
  local text = "| Name | Value |\n| --- | --- |\n| a | 中 |\n| longer | x |"
  local owner = chat_buffer(text)
  owner:replace_prompt("draft\n| untouched | prompt |")
  local source = nvim.api.nvim_buf_get_lines(owner.buffer, 0, -1, false)
  local marks = wait_for_tables(owner)
  local displayed = {}
  for _, mark in ipairs(marks) do
    displayed[#displayed + 1] = mark[4].virt_text[1][1]
  end
  MiniTest.expect.equality(displayed, {
    "| Name   | Value |",
    "| ------ | ----- |",
    "| a      | 中    |",
    "| longer | x     |",
  })
  MiniTest.expect.equality(nvim.api.nvim_buf_get_lines(owner.buffer, 0, -1, false), source)
  MiniTest.expect.equality(owner:prompt_text(), "draft\n| untouched | prompt |")
  MiniTest.expect.equality(nvim.bo[owner.buffer].filetype, "louiselm-session")
  MiniTest.expect.equality(nvim.treesitter.get_parser(owner.buffer):lang(), "markdown")
end

T["leaves prose, fenced examples and malformed tables untouched"] = function()
  local owner = chat_buffer(
    "ordinary | prose\n\n| bad | table |\n| -- | nope |\n\n```markdown\n| a | b |\n| --- | --- |\n| c | d |\n```"
  )
  nvim.wait(30)
  MiniTest.expect.equality(decorations(owner), {})
end

T["preserves escaped pipes and empty cells with Markdown column alignment"] = function()
  local owner = chat_buffer("| Left | Middle | Right |\n| :--- | :---: | ---: |\n| a \\| b | 中 | 1 |\n| | é | 22 |")
  local marks = wait_for_tables(owner)
  MiniTest.expect.equality(marks[3][4].virt_text[1][1], "| a \\| b |   中   |     1 |")
  MiniTest.expect.equality(marks[4][4].virt_text[1][1], "|        |   é    |    22 |")
end

T["reflows across split resizing and returns to inline rendering"] = function()
  local owner = chat_buffer("| Name | Value |\n| --- | --- |\n| a | " .. string.rep("b", 30) .. " |")
  wait_for_tables(owner)
  nvim.cmd.vsplit()
  local split = nvim.api.nvim_get_current_win()
  MiniTest.finally(function()
    if nvim.api.nvim_win_is_valid(split) then
      nvim.api.nvim_win_close(split, true)
    end
  end)
  nvim.api.nvim_win_set_width(split, 25)
  nvim.api.nvim_exec_autocmds("WinResized", {})
  MiniTest.expect.equality(
    nvim.wait(1000, function()
      return #decorations(owner) == 1
    end, 1),
    true
  )
  nvim.api.nvim_win_close(split, true)
  nvim.api.nvim_exec_autocmds("WinResized", {})
  MiniTest.expect.equality(
    nvim.wait(1000, function()
      return #decorations(owner) == 3
    end, 1),
    true
  )
end

T["preserves enclosing quote prefixes and optional outer pipes"] = function()
  local owner = chat_buffer("> Name | Value\n> --- | ---\n> a | b\n> longer | c")
  local marks = wait_for_tables(owner)
  MiniTest.expect.equality(marks[1][4].virt_text[1][1], "> | Name   | Value |")
  MiniTest.expect.equality(marks[4][4].virt_text[1][1], "> | longer | c     |")
end

T["ignores queued refresh after immediate disposal"] = function()
  local owner = chat_buffer("| a | b |\n| --- | --- |\n| c | d |")
  owner:dispose()
  owner:dispose()
  nvim.wait(30)
  MiniTest.expect.equality(nvim.api.nvim_buf_is_valid(owner.buffer), false)
  for _, autocmd in ipairs(nvim.api.nvim_get_autocmds({ event = "WinResized" })) do
    MiniTest.expect.equality(autocmd.group_name == "louiselm.chat.tables." .. owner.buffer, false)
  end
end

T["keeps table rendering disabled when Markdown highlighting is disabled"] = function()
  local owner = chat_buffer("| a | b |\n| --- | --- |\n| c | d |", false)
  nvim.wait(30)
  MiniTest.expect.equality(decorations(owner), {})
  for _, mapping in ipairs(nvim.api.nvim_buf_get_keymap(owner.buffer, "n")) do
    MiniTest.expect.equality(mapping.lhs == "gT", false)
  end
end

T["updates streamed tables and clears decoration after invalidation"] = function()
  local owner = chat_buffer("| Name | Value |\n| --- | --- |\n| a | b |")
  wait_for_tables(owner)
  owner:render({ type = "chunk", session_id = "table-test", data = { text = "\n| longer name | c |" } })
  MiniTest.expect.equality(
    nvim.wait(1000, function()
      return #decorations(owner) == 4
    end, 1),
    true
  )
  nvim.api.nvim_buf_set_lines(owner.buffer, 0, -1, false, { "ordinary | prose" })
  MiniTest.expect.equality(
    nvim.wait(1000, function()
      return #decorations(owner) == 0
    end, 1),
    true
  )
  owner:dispose()
  nvim.wait(20)
end

T["offers full-width inspection for overflow and owns the inspector lifecycle"] = function()
  local text = "| Name | Value |\n| --- | --- |\n| a | " .. string.rep("long cell ", 30) .. "|"
  local owner = chat_buffer(text)
  local source = nvim.api.nvim_buf_get_lines(owner.buffer, 0, -1, false)
  local marks = wait_for_tables(owner)
  MiniTest.expect.equality(#marks, 1)
  MiniTest.expect.equality(marks[1][4].virt_lines[1][1][1]:find("gT", 1, true) ~= nil, true)
  nvim.api.nvim_win_set_cursor(owner.window, { marks[1][2] + 3, 0 })
  for _, mapping in ipairs(nvim.api.nvim_buf_get_keymap(owner.buffer, "n")) do
    if mapping.lhs == "gT" then
      mapping.callback()
    end
  end
  local inspector = nvim.api.nvim_get_current_win()
  MiniTest.expect.equality(inspector ~= owner.window, true)
  MiniTest.expect.equality(nvim.wo[inspector].wrap, false)
  local inspected = nvim.api.nvim_buf_get_lines(nvim.api.nvim_win_get_buf(inspector), 0, -1, false)
  MiniTest.expect.equality(inspected[3]:find(string.rep("long cell ", 29) .. "long cell", 1, true) ~= nil, true)
  MiniTest.expect.equality(nvim.api.nvim_buf_get_lines(owner.buffer, 0, -1, false), source)
  owner:dispose()
  MiniTest.expect.equality(nvim.api.nvim_win_is_valid(inspector), false)
end

return T
