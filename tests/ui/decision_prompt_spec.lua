local MiniTest = require("mini.test")
local Prompt = require("louiselm.ui.decision_prompt")

---@diagnostic disable-next-line: undefined-global -- Neovim owns the tested windows and mappings.
local nvim = vim
local T = MiniTest.new_set()

local function prompt()
  local owner = Prompt.new()
  MiniTest.finally(function()
    owner:dispose()
    nvim.cmd.stopinsert()
  end)
  return owner
end

local function mapping(mode, key)
  for _, entry in ipairs(nvim.api.nvim_buf_get_keymap(0, mode)) do
    if entry.lhs == key then
      return entry.callback
    end
  end
  error("missing decision mapping " .. mode .. " " .. key)
end

T["selection uses owned UI and settles the chosen item exactly once"] = function()
  local owner = prompt()
  local denied, allowed = { label = "Deny" }, { label = "Allow\nnow" }
  local responses = {}
  assert(owner:select({ denied, allowed }, {
    prompt = "Session\nrequest",
    format_item = function(item)
      return item.label
    end,
  }, function(item, index)
    responses[#responses + 1] = { item, index }
  end))
  local buffer = nvim.api.nvim_get_current_buf()
  local window = nvim.api.nvim_get_current_win()
  MiniTest.expect.equality(owner:contains_current(), true)
  MiniTest.expect.equality(nvim.api.nvim_buf_get_lines(buffer, 0, -1, false), { "Deny", "Allow now" })
  MiniTest.expect.equality(nvim.api.nvim_win_get_config(window).title[1][1], "Session request")
  MiniTest.expect.equality(nvim.api.nvim_get_option_value("modifiable", { buf = buffer }), false)
  nvim.api.nvim_win_set_cursor(window, { 2, 0 })
  local choose = mapping("n", "<CR>")
  choose()
  choose()
  MiniTest.expect.equality(#responses, 1)
  MiniTest.expect.equality(responses[1][1] == allowed, true)
  MiniTest.expect.equality(responses[1][2], 2)
  MiniTest.expect.equality(nvim.api.nvim_buf_is_valid(buffer), false)
  MiniTest.expect.equality(owner:contains_current(), false)
end

T["hiding preserves choices and cursor without answering and invalidates old mappings"] = function()
  local owner = prompt()
  local responses = {}
  assert(owner:select({ "Deny", "Allow" }, { prompt = "Permission" }, function(item)
    responses[#responses + 1] = item or "cancelled"
  end))
  local buffer = nvim.api.nvim_get_current_buf()
  local window = nvim.api.nvim_get_current_win()
  nvim.api.nvim_win_set_cursor(window, { 2, 0 })
  local old_choose = mapping("n", "<CR>")
  assert(owner:hide())
  old_choose()
  MiniTest.expect.equality(nvim.api.nvim_win_is_valid(window), false)
  MiniTest.expect.equality(nvim.api.nvim_buf_is_valid(buffer), true)
  MiniTest.expect.equality(owner:contains_current(), false)
  MiniTest.expect.equality(responses, {})
  assert(owner:show())
  local reopened = nvim.api.nvim_get_current_win()
  assert(owner:show())
  MiniTest.expect.equality(nvim.api.nvim_get_current_win(), reopened)
  MiniTest.expect.equality(nvim.api.nvim_get_current_buf(), buffer)
  MiniTest.expect.equality(nvim.api.nvim_win_get_cursor(reopened), { 2, 0 })
  old_choose()
  MiniTest.expect.equality(responses, {})
  mapping("n", "<CR>")()
  MiniTest.expect.equality(responses, { "Allow" })
end

T["input keeps edited draft and cursor across hiding then submits only on Enter"] = function()
  local owner = prompt()
  local responses = {}
  assert(owner:input({ prompt = "Feedback", default = "initial" }, function(value)
    responses[#responses + 1] = value or "cancelled"
  end))
  local buffer = nvim.api.nvim_get_current_buf()
  nvim.api.nvim_buf_set_lines(buffer, 0, -1, false, { "edited feedback" })
  nvim.api.nvim_win_set_cursor(0, { 1, 5 })
  assert(owner:hide())
  assert(owner:show())
  MiniTest.expect.equality(nvim.api.nvim_get_current_buf(), buffer)
  MiniTest.expect.equality(nvim.api.nvim_buf_get_lines(buffer, 0, -1, false), { "edited feedback" })
  MiniTest.expect.equality(nvim.api.nvim_win_get_cursor(0), { 1, 5 })
  MiniTest.expect.equality(responses, {})
  mapping("i", "<CR>")()
  MiniTest.expect.equality(responses, { "edited feedback" })
end

T["replacement and disposal remove resources without settling obsolete presentations"] = function()
  local owner = prompt()
  local responses = {}
  assert(owner:select({ "Allow" }, { prompt = "Old" }, function()
    responses[#responses + 1] = "old"
  end))
  local old_buffer = nvim.api.nvim_get_current_buf()
  local stale = mapping("n", "<CR>")
  assert(owner:input({ prompt = "New" }, function(value)
    responses[#responses + 1] = value or "cancelled"
  end))
  local input_buffer = nvim.api.nvim_get_current_buf()
  local cancel = mapping("i", "<Esc>")
  MiniTest.expect.equality(nvim.api.nvim_buf_is_valid(old_buffer), false)
  stale()
  MiniTest.expect.equality(responses, {})
  cancel()
  cancel()
  MiniTest.expect.equality(responses, { "cancelled" })
  MiniTest.expect.equality(nvim.api.nvim_buf_is_valid(input_buffer), false)
  assert(owner:select({ "Allow" }, {}, function()
    responses[#responses + 1] = "disposed"
  end))
  local disposed_buffer = nvim.api.nvim_get_current_buf()
  local late = mapping("n", "<CR>")
  owner:hide()
  owner:dispose()
  owner:dispose()
  late()
  MiniTest.expect.equality(nvim.api.nvim_buf_is_valid(disposed_buffer), false)
  MiniTest.expect.equality(responses, { "cancelled" })
  MiniTest.expect.equality({ owner:show() }, { false, "decision prompt is disposed" })
end

T["inspectors retain scroll position and return only on explicit close"] = function()
  local owner = prompt()
  local closed = 0
  assert(owner:inspect({ "Plan", "details" }, "Session plan", function()
    closed = closed + 1
    assert(owner:select({ "Approve" }, { prompt = "Follow-up" }, function() end))
  end))
  local buffer = nvim.api.nvim_get_current_buf()
  nvim.api.nvim_win_set_cursor(0, { 2, 0 })
  assert(owner:hide())
  MiniTest.expect.equality(closed, 0)
  assert(owner:show())
  MiniTest.expect.equality(nvim.api.nvim_win_get_cursor(0), { 2, 0 })
  local close = mapping("n", "q")
  close()
  close()
  MiniTest.expect.equality(closed, 1)
  MiniTest.expect.equality(nvim.api.nvim_buf_is_valid(buffer), false)
  MiniTest.expect.equality(nvim.api.nvim_buf_get_lines(0, 0, -1, false), { "Approve" })
end

T["selection cancellation works through real key input"] = function()
  local owner = prompt()
  local cancelled = 0
  assert(owner:select({ "Allow" }, {}, function(item)
    MiniTest.expect.equality(item, nil)
    cancelled = cancelled + 1
  end))
  nvim.api.nvim_feedkeys("q", "mx!", false)
  MiniTest.expect.equality(cancelled, 1)
end

T["an unfocused prompt cannot answer before its host reconciles focus"] = function()
  local owner = prompt()
  local host = nvim.api.nvim_get_current_win()
  local responses = 0
  assert(owner:select({ "Allow" }, {}, function()
    responses = responses + 1
  end))
  local window = nvim.api.nvim_get_current_win()
  local choose = mapping("n", "<CR>")
  nvim.api.nvim_set_current_win(host)
  MiniTest.expect.equality(owner:contains_current(), false)
  choose()
  MiniTest.expect.equality(responses, 0)
  nvim.api.nvim_set_current_win(window)
  choose()
  MiniTest.expect.equality(responses, 1)
end

T["construction and failed formatting create no resources or responses"] = function()
  local buffers, windows = nvim.api.nvim_list_bufs(), nvim.api.nvim_list_wins()
  local owner = prompt()
  MiniTest.expect.equality(nvim.api.nvim_list_bufs(), buffers)
  MiniTest.expect.equality(nvim.api.nvim_list_wins(), windows)
  MiniTest.expect.equality({ owner:show() }, { false, "no decision prompt is prepared" })
  MiniTest.expect.equality({ owner:select({}, {}, function() end) }, { false, "decision choices must not be empty" })
  MiniTest.expect.equality({
    owner:select({ "secret payload" }, {
      format_item = function(item)
        error(item)
      end,
    }, function()
      error("failed formatting must not settle the decision")
    end),
  }, { false, "could not format decision choices" })
  MiniTest.expect.equality(nvim.api.nvim_list_bufs(), buffers)
  MiniTest.expect.equality(nvim.api.nvim_list_wins(), windows)
end

T["input hide cancels active and pending insert mode without changing another focused editor"] = function()
  local child = MiniTest.new_child_neovim()
  MiniTest.finally(function()
    child.stop()
  end)
  child.start({ "--noplugin", "-u", "NONE" })
  child.api.nvim_ui_attach(80, 24, { rgb = true })
  child.lua("vim.opt.runtimepath:prepend(...)", { nvim.fn.getcwd() })
  child.lua([[
    _G.decision_mode_owner = require("louiselm.ui.decision_prompt").new()
    _G.decision_mode_host = vim.api.nvim_get_current_win()
    assert(decision_mode_owner:input({}, function() end))
    decision_mode_owner:hide()
  ]])
  MiniTest.expect.equality(child.api.nvim_get_mode().mode, "n")
  child.lua("assert(decision_mode_owner:show())")
  MiniTest.expect.equality(child.api.nvim_get_mode().mode, "i")
  child.type_keys("draft")
  child.lua("decision_mode_owner:hide()")
  MiniTest.expect.equality(child.api.nvim_get_mode().mode, "n")
  child.lua("assert(decision_mode_owner:show())")
  MiniTest.expect.equality(child.lua_get("vim.api.nvim_buf_get_lines(0, 0, -1, false)"), { "draft" })
  child.lua([[
    vim.api.nvim_set_current_win(decision_mode_host)
    vim.cmd.startinsert()
    decision_mode_owner:hide()
  ]])
  MiniTest.expect.equality(child.api.nvim_get_mode().mode, "i")
  child.type_keys("<Esc>")
  child.lua("assert(decision_mode_owner:show()); decision_mode_owner:dispose()")
  MiniTest.expect.equality(child.api.nvim_get_mode().mode, "n")
end

T["follow-up input remains prepared and hidden when closing its picker switches the host"] = function()
  local origin = nvim.api.nvim_get_current_buf()
  local other = nvim.api.nvim_create_buf(false, true)
  local owner
  owner = Prompt.new(function()
    return nvim.api.nvim_get_current_buf() == origin or owner:contains_current()
  end)
  MiniTest.finally(function()
    owner:dispose()
    nvim.api.nvim_buf_delete(other, { force = true })
    nvim.cmd.stopinsert()
  end)
  assert(owner:select({ "Continue" }, {}, function()
    assert(owner:input({ default = "retained follow-up" }, function() end))
  end))
  local choose = mapping("n", "<CR>")
  local observer = nvim.api.nvim_create_augroup("louiselm-prompt-followup-test", { clear = true })
  nvim.api.nvim_create_autocmd("WinEnter", {
    group = observer,
    once = true,
    callback = function()
      nvim.api.nvim_set_current_buf(other)
    end,
  })
  MiniTest.finally(function()
    nvim.api.nvim_del_augroup_by_id(observer)
  end)
  choose()
  MiniTest.expect.equality(nvim.api.nvim_get_current_buf(), other)
  MiniTest.expect.equality(owner.window, nil)
  MiniTest.expect.equality(nvim.api.nvim_buf_get_lines(owner.buffer, 0, -1, false), { "retained follow-up" })
  nvim.api.nvim_set_current_buf(origin)
  assert(owner:show())
  MiniTest.expect.equality(owner:contains_current(), true)
end

T["a host switch during float creation leaves its prepared decision hidden"] = function()
  local host = nvim.api.nvim_get_current_win()
  local origin = nvim.api.nvim_get_current_buf()
  local other = nvim.api.nvim_create_buf(false, true)
  local owner
  owner = Prompt.new(function()
    return nvim.api.nvim_get_current_buf() == origin or owner:contains_current()
  end)
  MiniTest.finally(function()
    owner:dispose()
    nvim.api.nvim_buf_delete(other, { force = true })
  end)
  local observer = nvim.api.nvim_create_augroup("louiselm-prompt-creation-test", { clear = true })
  nvim.api.nvim_create_autocmd("BufEnter", {
    group = observer,
    once = true,
    callback = function()
      nvim.api.nvim_set_current_win(host)
      nvim.api.nvim_set_current_buf(other)
    end,
  })
  MiniTest.finally(function()
    nvim.api.nvim_del_augroup_by_id(observer)
  end)
  assert(owner:select({ "Allow" }, {}, function()
    error("navigation must not answer")
  end))
  MiniTest.expect.equality(nvim.api.nvim_get_current_buf(), other)
  MiniTest.expect.equality(owner.window, nil)
  MiniTest.expect.equality(nvim.api.nvim_buf_is_valid(owner.buffer), true)
  nvim.api.nvim_set_current_buf(origin)
  assert(owner:show())
  MiniTest.expect.equality(owner:contains_current(), true)
end

return T
