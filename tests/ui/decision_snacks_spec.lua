local MiniTest = require("mini.test")

---@diagnostic disable-next-line: undefined-global -- Neovim runs this integration fixture.
local nvim = vim
local T = MiniTest.new_set()

-- Real provider contract: snacks.nvim 882c996cf28183f4d63640de0b4c02ec886d01f2.
-- Keep its global setup, timers and picker history inside a disposable editor.
local function start(ui_select, auto_confirm, layout, focus)
  local mini_path = nvim.env.MINI_NVIM_PATH or (nvim.fn.getcwd() .. "/.deps/mini.nvim")
  local snacks_path = nvim.env.SNACKS_NVIM_PATH or (nvim.fn.fnamemodify(mini_path, ":h") .. "/snacks.nvim")
  assert(nvim.fn.isdirectory(snacks_path) == 1, "Snacks tests require SNACKS_NVIM_PATH or .deps/snacks.nvim")
  local child = MiniTest.new_child_neovim()
  local private_root = nvim.fn.tempname()
  MiniTest.finally(function()
    child.stop()
    nvim.fn.delete(private_root, "rf")
  end)
  child.start({ "--noplugin", "-u", "NONE" })
  child.api.nvim_ui_attach(100, 35, { rgb = true })
  child.lua(
    [[
    local root = ...
    for _, name in ipairs({ "STATE", "DATA", "CACHE", "CONFIG" }) do
      vim.env["XDG_" .. name .. "_HOME"] = root .. "/" .. name:lower()
    end
  ]],
    { private_root }
  )
  child.lua("vim.opt.runtimepath:prepend(...)", { snacks_path })
  child.lua("vim.opt.runtimepath:prepend(...)", { nvim.fn.getcwd() })
  child.lua(
    [[
    local ui_select, auto_confirm, layout, focus = ...
    require("snacks").setup({ picker = {
      enabled = true,
      ui_select = ui_select,
      auto_confirm = auto_confirm,
      sources = { select = { layout = layout, focus = focus } },
    } })
    local state = {
      host = vim.api.nvim_get_current_win(),
      buffer = vim.api.nvim_get_current_buf(),
      visible = true,
      responses = {},
      items = { "Reject", "Allow once", "Allow for Session" },
    }
    state.owner = require("louiselm.ui.decision_prompt").new(function()
      return state.visible and (
        vim.api.nvim_get_current_buf() == state.buffer or state.owner:contains_current()
      )
    end)
    function state.open()
      assert(state.owner:select(state.items, { prompt = "Session A decision" }, function(item, index)
        state.responses[#state.responses + 1] = { item = item or "cancelled", index = index }
      end))
    end
    function state.picker()
      for _, picker in ipairs(Snacks.picker.get()) do
        if picker.opts.title == "Session A decision" then
          return picker
        end
      end
    end
    function state.wait_picker()
      assert(vim.wait(1000, function()
        local picker = state.picker()
        return picker and picker.shown and picker:is_focused() and not picker:is_active()
      end), "decision Snacks picker did not become ready")
      return state.picker()
    end
    function state.visible_decisions()
      local count = 0
      for _, win in ipairs(vim.api.nvim_list_wins()) do
        local config = vim.api.nvim_win_get_config(win)
        if config.relative ~= "" and not config.hide then
          local lines = vim.api.nvim_buf_get_lines(vim.api.nvim_win_get_buf(win), 0, -1, false)
          for _, line in ipairs(lines) do
            if line:find("Allow once", 1, true) or line:find("Allow for Session", 1, true) then
              count = count + 1
            end
          end
        end
      end
      return count
    end
    function state.leave()
      state.visible = false
      state.owner:hide()
      vim.api.nvim_set_current_win(state.host)
      state.other = state.other or vim.api.nvim_create_buf(false, true)
      vim.api.nvim_win_set_buf(state.host, state.other)
    end
    function state.return_to_owner()
      vim.api.nvim_set_current_win(state.host)
      vim.api.nvim_win_set_buf(state.host, state.buffer)
      state.visible = true
      assert(state.owner:show())
      return state.wait_picker()
    end
    _G.snacks_decision_test = state
  ]],
    {
      ui_select ~= false,
      auto_confirm == true,
      layout or { preset = "select", layout = { width = 72 } },
      focus or "input",
    }
  )
  return child
end

T["uses the configured Snacks select provider"] = function()
  local child = start()
  child.lua("snacks_decision_test.open()")
  MiniTest.expect.equality(child.lua_get("#Snacks.picker.get()"), 1)
  child.lua("snacks_decision_test.wait_picker()")
  MiniTest.expect.equality(child.lua_get("snacks_decision_test.picker().resolved_layout.layout.width"), 72)
  MiniTest.expect.equality(child.lua_get("snacks_decision_test.owner:contains_current()"), true)
end

T["loaded Snacks without ui_select preserves the native fallback"] = function()
  local child = start(false)
  child.lua("snacks_decision_test.open()")
  MiniTest.expect.equality(child.lua_get("#Snacks.picker.get()"), 0)
  MiniTest.expect.equality(child.lua_get("vim.api.nvim_buf_get_lines(0, 0, -1, false)"), {
    "Reject",
    "Allow once",
    "Allow for Session",
  })
  child.type_keys("<CR>")
  MiniTest.expect.equality(child.lua_get("snacks_decision_test.responses"), { { item = "Reject", index = 1 } })
end

T["filter and selected choice survive Session hiding and a hidden resize"] = function()
  local child = start()
  child.lua("snacks_decision_test.open(); snacks_decision_test.wait_picker()")
  child.type_keys("Allow")
  child.lua([[
    local picker = snacks_decision_test.picker()
    assert(vim.wait(1000, function() return picker.list:count() == 2 and not picker:is_active() end))
    picker.list:move(2, true, true)
    snacks_decision_test.selected = picker:current().idx
    snacks_decision_test.stale_picker = picker
    snacks_decision_test.stale_confirm = picker.opts.actions.confirm
    snacks_decision_test.stale_item = picker:current()
    snacks_decision_test.leave()
    snacks_decision_test.stale_confirm(picker, snacks_decision_test.stale_item)
  ]])
  MiniTest.expect.equality(child.lua_get("snacks_decision_test.visible_decisions()"), 0)
  MiniTest.expect.equality(child.lua_get("snacks_decision_test.responses"), {})
  child.api.nvim_ui_try_resize(110, 40)
  MiniTest.expect.equality(child.lua_get("snacks_decision_test.visible_decisions()"), 0)
  child.lua("snacks_decision_test.return_to_owner()")
  MiniTest.expect.equality(child.lua_get("snacks_decision_test.picker().input:get()"), "Allow")
  MiniTest.expect.equality(
    child.lua_get("snacks_decision_test.picker():current().idx"),
    child.lua_get("snacks_decision_test.selected")
  )
  child.type_keys("<CR>")
  child.lua("assert(vim.wait(1000, function() return #snacks_decision_test.responses == 1 end))")
  MiniTest.expect.equality(
    child.lua_get("snacks_decision_test.responses[1].index"),
    child.lua_get("snacks_decision_test.selected")
  )
end

T["filtered choices and repeated obsolete actions settle only once"] = function()
  local child = start()
  child.lua("snacks_decision_test.open(); snacks_decision_test.wait_picker()")
  child.type_keys("Session")
  child.lua([[
    local picker = snacks_decision_test.picker()
    assert(vim.wait(1000, function() return picker.list:count() == 1 and not picker:is_active() end))
    snacks_decision_test.stale_picker = picker
    snacks_decision_test.stale_confirm = picker.opts.actions.confirm
    snacks_decision_test.stale_item = picker:current()
  ]])
  child.type_keys("<CR>")
  child.lua([[
    local state = snacks_decision_test
    assert(vim.wait(1000, function() return #state.responses == 1 end))
    state.stale_confirm(state.stale_picker, state.stale_item)
  ]])
  MiniTest.expect.equality(
    child.lua_get("snacks_decision_test.responses"),
    { { item = "Allow for Session", index = 3 } }
  )
  child.lua("snacks_decision_test.open(); snacks_decision_test.wait_picker()")
  -- Snacks keeps its normal editing bindings: Escape leaves insert mode first.
  child.type_keys("<Esc>")
  MiniTest.expect.equality(child.api.nvim_get_mode().mode, "n")
  MiniTest.expect.equality(child.lua_get("#snacks_decision_test.responses"), 1)
  child.type_keys("<Esc>")
  child.lua("assert(vim.wait(1000, function() return #snacks_decision_test.responses == 2 end))")
  MiniTest.expect.equality(child.lua_get("snacks_decision_test.responses[2]"), { item = "cancelled" })
end

T["global resume never reveals hidden settled or disposed decisions"] = function()
  for _, lifecycle in ipairs({ "hidden", "settled", "disposed" }) do
    local child = start()
    child.lua([[
      vim.ui.select({ "Unrelated choice" }, { prompt = "Ordinary picker" }, function() end)
      local ordinary = Snacks.picker.get()[1]
      assert(vim.wait(1000, function() return ordinary.shown end))
      ordinary:close()
    ]])
    child.lua("snacks_decision_test.open(); snacks_decision_test.wait_picker()")
    if lifecycle == "settled" then
      child.type_keys("<CR>")
      child.lua("assert(vim.wait(1000, function() return #snacks_decision_test.responses == 1 end))")
    elseif lifecycle == "disposed" then
      child.lua("snacks_decision_test.owner:dispose()")
    end
    child.lua([[
      snacks_decision_test.leave()
      local resumed = Snacks.picker.resume()
      if resumed and not resumed.closed then
        assert(vim.wait(1000, function()
          return resumed.closed or (resumed.shown and not resumed:is_active())
        end))
      end
    ]])
    MiniTest.expect.equality(child.lua_get("snacks_decision_test.visible_decisions()"), 0)
    child.lua([[
      for _, picker in ipairs(Snacks.picker.get()) do
        picker:close()
      end
    ]])
    MiniTest.expect.equality(child.lua_get("#snacks_decision_test.responses"), lifecycle == "settled" and 1 or 0)
    child.stop()
  end
end

T["ordinary selectors coexist with hidden decisions and cannot settle them"] = function()
  local child = start()
  child.lua([[
    local state = snacks_decision_test
    state.open()
    state.wait_picker()
    state.leave()
    vim.ui.select({ "Unrelated choice" }, { prompt = "Ordinary picker" }, function(item)
      state.ordinary_answer = item
    end)
    assert(vim.wait(1000, function()
      for _, picker in ipairs(Snacks.picker.get()) do
        if picker.opts.title == "Ordinary picker" then
          return picker.shown and picker:is_focused()
        end
      end
      return false
    end))
  ]])
  MiniTest.expect.equality(child.lua_get("snacks_decision_test.visible_decisions()"), 0)
  child.type_keys("<CR>")
  child.lua("assert(vim.wait(1000, function() return snacks_decision_test.ordinary_answer ~= nil end))")
  MiniTest.expect.equality(child.lua_get("snacks_decision_test.ordinary_answer"), "Unrelated choice")
  MiniTest.expect.equality(child.lua_get("snacks_decision_test.responses"), {})
  child.lua("snacks_decision_test.return_to_owner()")
  MiniTest.expect.equality(child.lua_get("snacks_decision_test.owner:contains_current()"), true)
end

T["hiding or disposing does not stop insert mode in another focused editor"] = function()
  local child = start()
  child.lua([[
    local state = snacks_decision_test
    state.open()
    state.wait_picker()
    state.visible = false
    vim.api.nvim_set_current_win(state.host)
    state.other = vim.api.nvim_create_buf(false, true)
    vim.api.nvim_win_set_buf(state.host, state.other)
  ]])
  child.lua("vim.cmd.startinsert()")
  MiniTest.expect.equality(child.api.nvim_get_mode().mode, "i")
  child.lua("snacks_decision_test.owner:hide()")
  -- The provider's mode restoration crosses event-loop turns. Wait from the
  -- parent (not inside the child RPC), then prove focus and real typing.
  MiniTest.expect.equality(
    nvim.wait(1000, function()
      return child.lua_get("vim.api.nvim_get_mode().mode") == "i"
    end),
    true
  )
  MiniTest.expect.equality(child.lua_get("vim.api.nvim_get_current_win() == snacks_decision_test.host"), true)
  MiniTest.expect.equality(child.lua_get("vim.api.nvim_get_current_buf() == snacks_decision_test.other"), true)
  child.type_keys("typing survives")
  MiniTest.expect.equality(child.lua_get("vim.api.nvim_buf_get_lines(0, 0, -1, false)"), { "typing survives" })
  child.lua("snacks_decision_test.owner:dispose()")
  MiniTest.expect.equality(
    nvim.wait(1000, function()
      return child.lua_get("vim.api.nvim_get_mode().mode") == "i"
    end),
    true
  )
  child.type_keys(" disposal")
  MiniTest.expect.equality(child.lua_get("vim.api.nvim_buf_get_lines(0, 0, -1, false)"), { "typing survives disposal" })
  MiniTest.expect.equality(child.lua_get("snacks_decision_test.responses"), {})
end

T["a host switch while the provider enters its input leaves the request hidden"] = function()
  local child = start()
  child.lua([[
    local state = snacks_decision_test
    state.other = vim.api.nvim_create_buf(false, true)
    vim.api.nvim_create_autocmd("BufEnter", {
      callback = function(event)
        if not vim.bo[event.buf].filetype:find("^snacks_picker") then
          return
        end
        state.visible = false
        state.switched = true
        vim.api.nvim_set_current_win(state.host)
        vim.api.nvim_win_set_buf(state.host, state.other)
        return true
      end,
    })
    state.open()
    assert(vim.wait(1000, function() return state.switched end))
  ]])
  MiniTest.expect.equality(child.lua_get("vim.api.nvim_get_current_buf() == snacks_decision_test.other"), true)
  MiniTest.expect.equality(child.lua_get("snacks_decision_test.visible_decisions()"), 0)
  MiniTest.expect.equality(child.lua_get("snacks_decision_test.responses"), {})
  child.lua("snacks_decision_test.return_to_owner()")
  MiniTest.expect.equality(child.lua_get("snacks_decision_test.owner:contains_current()"), true)
end

T["changing the underlying Session host during provider entry hides its decision"] = function()
  local child = start()
  child.lua([[
    local state = snacks_decision_test
    state.other = vim.api.nvim_create_buf(false, true)
    vim.api.nvim_create_autocmd("BufEnter", {
      callback = function(event)
        if not vim.bo[event.buf].filetype:find("^snacks_picker") then
          return
        end
        state.switched = true
        vim.api.nvim_win_set_buf(state.host, state.other)
        return true
      end,
    })
    state.open()
    assert(vim.wait(1000, function() return state.switched end))
  ]])
  MiniTest.expect.equality(child.lua_get("snacks_decision_test.visible_decisions()"), 0)
  MiniTest.expect.equality(child.lua_get("snacks_decision_test.responses"), {})
  child.lua("snacks_decision_test.return_to_owner()")
  MiniTest.expect.equality(child.lua_get("snacks_decision_test.owner:contains_current()"), true)
end

T["one decision still requires an explicit answer when Snacks auto_confirm is enabled"] = function()
  local child = start(true, true)
  child.lua([[
    local state = snacks_decision_test
    state.items = { "Allow once" }
    state.open()
    assert(vim.wait(1000, function()
      local picker = state.picker()
      return #state.responses > 0 or (picker and picker.shown)
    end))
  ]])
  MiniTest.expect.equality(child.lua_get("snacks_decision_test.responses"), {})
  MiniTest.expect.equality(child.lua_get("snacks_decision_test.owner:contains_current()"), true)
  child.type_keys("<CR>")
  child.lua("assert(vim.wait(1000, function() return #snacks_decision_test.responses == 1 end))")
  MiniTest.expect.equality(child.lua_get("snacks_decision_test.responses"), { { item = "Allow once", index = 1 } })
end

T["disposal during provider entry invalidates delayed show callbacks"] = function()
  local child = start()
  child.lua([[
    local state = snacks_decision_test
    vim.api.nvim_create_autocmd("BufEnter", {
      callback = function(event)
        if not vim.bo[event.buf].filetype:find("^snacks_picker") then
          return
        end
        state.retired = assert(state.picker())
        state.owner:dispose()
        return true
      end,
    })
    state.open()
    assert(vim.wait(1000, function() return state.retired ~= nil end))
  ]])
  child.lua([[
    local state = snacks_decision_test
    assert(vim.wait(1000, function() return state.retired.closed and state.retired.layout.closed end))
    state.retired.opts.on_show(state.retired)
  ]])
  MiniTest.expect.equality(child.lua_get("snacks_decision_test.visible_decisions()"), 0)
  MiniTest.expect.equality(child.lua_get("snacks_decision_test.responses"), {})
  MiniTest.expect.equality(child.lua_get("snacks_decision_test.owner:contains_current()"), false)
end

T["a configured sidebar layout closes all split and floating windows while hidden"] = function()
  local child = start(true, false, { preset = "sidebar", preview = false })
  child.lua([[
    local state = snacks_decision_test
    state.initial_windows = #vim.api.nvim_list_wins()
    state.open()
    local picker = state.wait_picker()
    assert(picker.resolved_layout.layout.position == "left")
    assert(vim.api.nvim_win_get_config(picker.layout.root.win).relative == "")
    assert(#vim.api.nvim_list_wins() > state.initial_windows)
    state.leave()
  ]])
  MiniTest.expect.equality(
    child.lua_get("#vim.api.nvim_list_wins()"),
    child.lua_get("snacks_decision_test.initial_windows")
  )
  MiniTest.expect.equality(child.lua_get("snacks_decision_test.responses"), {})
  child.lua("snacks_decision_test.return_to_owner()")
  MiniTest.expect.equality(child.lua_get("snacks_decision_test.owner:contains_current()"), true)
  child.lua("snacks_decision_test.owner:dispose()")
  MiniTest.expect.equality(
    child.lua_get("#vim.api.nvim_list_wins()"),
    child.lua_get("snacks_decision_test.initial_windows")
  )
end

T["a selected choice opens editable feedback after deferred Snacks cleanup"] = function()
  local child = start()
  child.lua([[
    local state = snacks_decision_test
    assert(state.owner:select({ "Continue", "Reject" }, { prompt = "Session A decision" }, function(item)
      assert(item == "Continue")
      assert(state.owner:input({ prompt = "Feedback", default = "initial" }, function(value)
        state.feedback = value
      end))
      state.followup = true
    end))
    state.wait_picker()
  ]])
  child.type_keys("<CR>")
  child.lua("assert(vim.wait(1000, function() return snacks_decision_test.followup end))")
  MiniTest.expect.equality(child.api.nvim_get_mode().mode, "i")
  MiniTest.expect.equality(child.lua_get("snacks_decision_test.owner:contains_current()"), true)
  child.type_keys(" edited", "<CR>")
  MiniTest.expect.equality(child.lua_get("snacks_decision_test.feedback"), " editedinitial")
end

T["the Decisions controller isolates real Snacks requests across Session switches"] = function()
  local child = start()
  child.lua([[
    local state = snacks_decision_test
    local first = { inspect = function() return { id = "session-A", agent = "codex" } end }
    local second = { inspect = function() return { id = "session-B", agent = "codex" } end }
    local hosts = { [first] = state.buffer, [second] = vim.api.nvim_create_buf(false, true) }
    local owner = require("louiselm.ui.chat.decisions").new({
      is_live = function(session) return hosts[session] ~= nil end,
      is_visible = function(session) return vim.api.nvim_get_current_buf() == hosts[session] end,
      report_error = function(_, message) error(message) end,
      resolved = function() end,
    })
    function state.switch(session)
      local suspension = owner:suspend()
      vim.api.nvim_set_current_win(state.host)
      vim.api.nvim_win_set_buf(state.host, hosts[session])
      owner:present_next(suspension)
    end
    function state.request(session, id)
      owner:request(session, {
        request_id = id,
        options = {
          { optionId = "deny", kind = "reject_once", name = "Reject" },
          { optionId = "allow", kind = "allow_once", name = "Allow once" },
        },
      }, function(result)
        state.responses[#state.responses + 1] = { id = id, result = result }
        return true
      end)
    end
    function state.wait_decision(session)
      assert(vim.wait(1000, function()
        local picker = Snacks.picker.get()[1]
        return owner:active_session() == session
          and picker and picker.shown and picker:is_focused() and not picker:is_active()
      end))
    end
    state.decisions, state.first, state.second = owner, first, second
    state.switch(second)
    state.request(first, "A")
  ]])
  MiniTest.expect.equality(child.lua_get("#Snacks.picker.get()"), 0)
  MiniTest.expect.equality(child.lua_get("snacks_decision_test.responses"), {})
  child.lua([[
    local state = snacks_decision_test
    state.request(state.second, "B")
    state.wait_decision(state.second)
  ]])
  child.type_keys("<CR>")
  child.lua([[
    local state = snacks_decision_test
    assert(vim.wait(1000, function() return #state.responses == 1 end))
    state.switch(state.first)
    state.wait_decision(state.first)
  ]])
  child.type_keys("Allow")
  child.lua([[
    local state = snacks_decision_test
    local picker = Snacks.picker.get()[1]
    assert(vim.wait(1000, function() return picker.list:count() == 1 and not picker:is_active() end))
    state.switch(state.second)
  ]])
  MiniTest.expect.equality(child.lua_get("snacks_decision_test.visible_decisions()"), 0)
  MiniTest.expect.equality(child.lua_get("#snacks_decision_test.responses"), 1)
  child.lua([[
    local state = snacks_decision_test
    state.switch(state.first)
    state.wait_decision(state.first)
    assert(Snacks.picker.get()[1].input:get() == "Allow")
  ]])
  child.type_keys("<CR>")
  child.lua("assert(vim.wait(1000, function() return #snacks_decision_test.responses == 2 end))")
  MiniTest.expect.equality(child.lua_get("snacks_decision_test.responses"), {
    { id = "B", result = { outcome = { outcome = "selected", optionId = "deny" } } },
    { id = "A", result = { outcome = { outcome = "selected", optionId = "allow" } } },
  })
  child.lua([[
    local state = snacks_decision_test
    state.switch(state.second)
    state.request(state.first, "retired")
    state.decisions:dispose()
    state.decisions:dispose()
  ]])
  MiniTest.expect.equality(child.lua_get("#snacks_decision_test.responses"), 3)
  MiniTest.expect.equality(child.lua_get("snacks_decision_test.responses[3]"), {
    id = "retired",
    result = { outcome = { outcome = "cancelled" } },
  })
end

T["the maintainer's list focus and wide select layout survive hiding"] = function()
  local layout = { preset = "select", layout = { title_pos = "left", max_width = 999, width = 0.95 } }
  local child = start(true, false, layout, "list")
  child.lua("snacks_decision_test.open(); snacks_decision_test.wait_picker()")
  MiniTest.expect.equality(child.lua_get("snacks_decision_test.picker():current_win()"), "list")
  MiniTest.expect.equality(child.lua_get("snacks_decision_test.picker().resolved_layout.layout.width"), 0.95)
  MiniTest.expect.equality(child.lua_get("snacks_decision_test.picker().resolved_layout.layout.max_width"), 999)
  MiniTest.expect.equality(child.lua_get("snacks_decision_test.picker().resolved_layout.layout.title_pos"), "left")
  child.lua("snacks_decision_test.leave()")
  MiniTest.expect.equality(child.lua_get("snacks_decision_test.visible_decisions()"), 0)
  MiniTest.expect.equality(child.lua_get("snacks_decision_test.responses"), {})
  child.lua("snacks_decision_test.return_to_owner()")
  MiniTest.expect.equality(child.lua_get("snacks_decision_test.picker():current_win()"), "list")
  MiniTest.expect.equality(child.lua_get("snacks_decision_test.picker().resolved_layout.layout.width"), 0.95)
end

T["foreign prompt input stays editable and deliberate Escape does not restore insert mode"] = function()
  local child = start()
  child.lua([[
    local state = snacks_decision_test
    state.open()
    state.wait_picker()
    state.visible = false
    vim.api.nvim_set_current_win(state.host)
    state.other = vim.api.nvim_create_buf(false, true)
    vim.api.nvim_set_option_value("buftype", "prompt", { buf = state.other })
    vim.fn.prompt_setprompt(state.other, "> ")
    vim.api.nvim_win_set_buf(state.host, state.other)
  ]])
  child.lua("vim.cmd.startinsert()")
  child.type_keys("draft")
  MiniTest.expect.equality(child.api.nvim_get_mode().mode, "i")
  child.lua("snacks_decision_test.owner:hide(); snacks_decision_test.owner:dispose()")
  MiniTest.expect.equality(child.api.nvim_get_mode().mode, "i")
  MiniTest.expect.equality(child.lua_get("vim.api.nvim_buf_get_lines(0, 0, -1, false)"), { "> draft" })
  child.type_keys("<Esc>")
  child.lua("vim.schedule(function() snacks_decision_test.checked = true end)")
  child.lua("assert(vim.wait(1000, function() return snacks_decision_test.checked end))")
  MiniTest.expect.equality(child.api.nvim_get_mode().mode, "n")
  MiniTest.expect.equality(child.lua_get("snacks_decision_test.responses"), {})
end

return T
