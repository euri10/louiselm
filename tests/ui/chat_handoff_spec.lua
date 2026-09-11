local MiniTest = require("mini.test")
local Handoffs = require("louiselm.ui.chat.handoff")
local T = MiniTest.new_set()

---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim

local function fixture()
  local f = { sent = {}, focused = {} }
  local source = { id = "source", agent = "codex", status = "ready", config_options = {} }
  local target_state = { id = "target", agent = "codex", status = "ready", config_options = {} }
  local target = {
    inspect = function()
      return target_state
    end,
    prompt = function() end,
  }
  local owner = Handoffs.new({
    submit = function(review, text, content)
      f.sent[#f.sent + 1] = { review = review, text = text, content = content }
      return true
    end,
    focus = function(id)
      f.focused[#f.focused + 1] = id
    end,
  })
  MiniTest.finally(function()
    owner:dispose()
  end)
  f.owner = owner
  f.buffer = assert(owner:open(target, source, {}))
  return f
end

T["abandons a review without sending"] = function()
  local f = fixture()
  assert(f.owner:abandon(f.buffer))
  MiniTest.expect.equality(f.sent, {})
  MiniTest.expect.equality(f.focused, { "target" })
  MiniTest.expect.equality(nvim.api.nvim_buf_is_valid(f.buffer), false)
end

T["refuses empty and blank-task reviews without closing them"] = function()
  local f = fixture()
  nvim.api.nvim_buf_set_lines(f.buffer, 0, -1, false, { "  " })
  MiniTest.expect.equality({ f.owner:submit(f.buffer) }, { false, "handoff prompt must be a non-empty string" })
  nvim.api.nvim_buf_set_lines(f.buffer, 0, -1, false, { "- takeover task: " })
  MiniTest.expect.equality(
    { f.owner:submit(f.buffer) },
    { false, "handoff takeover task must be filled in before submitting" }
  )
  MiniTest.expect.equality(f.sent, {})
  MiniTest.expect.equality(nvim.api.nvim_buf_is_valid(f.buffer), true)
end

T["disposal removes reviews and makes retained key callbacks inert"] = function()
  local f = fixture()
  nvim.api.nvim_buf_set_lines(f.buffer, 0, -1, false, { "- takeover task: continue" })
  local submit = nvim.fn.maparg("<C-s>", "n", false, true).callback
  assert(type(submit) == "function")
  f.owner:dispose()
  f.owner:dispose()
  submit()
  MiniTest.expect.equality(nvim.api.nvim_buf_is_valid(f.buffer), false)
  MiniTest.expect.equality(f.sent, {})
  MiniTest.expect.equality(f.focused, {})
  MiniTest.expect.equality({ f.owner:abandon(f.buffer) }, { false, "handoff buffer is not open" })
end

T["handles editor-wiped review buffers during submit and disposal"] = function()
  local f = fixture()
  nvim.api.nvim_buf_delete(f.buffer, { force = true })
  MiniTest.expect.equality({ f.owner:submit(f.buffer) }, { false, "handoff buffer is not open" })
  f.owner:dispose()
  MiniTest.expect.equality(f.sent, {})
end

return T
