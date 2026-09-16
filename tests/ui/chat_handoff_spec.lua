local MiniTest = require("mini.test")
local Handoffs = require("louiselm.ui.chat.handoff")
local T = MiniTest.new_set()

---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim

local function fixture(entries, embedded)
  local f = { sent = {}, focused = {} }
  local source = { id = "source", agent = "codex", status = "ready", config_options = {} }
  local target_state = { id = "target", agent = "codex", status = "ready", config_options = {} }
  target_state.embedded_context = embedded
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
  f.buffer = assert(owner:open(target, source, entries or {}))
  return f
end

T["abandons a review without sending"] = function()
  local f = fixture()
  assert(f.owner:abandon(f.buffer))
  MiniTest.expect.equality(f.sent, {})
  MiniTest.expect.equality(f.focused, { "target" })
  MiniTest.expect.equality(nvim.api.nvim_buf_is_valid(f.buffer), false)
end

T["reviews summary and recent conversation before submitting through either target transport"] = function()
  for _, embedded in ipairs({ true, false }) do
    local f = fixture({
      { kind = "user", text = "old history" },
      { kind = "assistant", text = "old answer" },
      { kind = "user", text = "current request" },
      {
        kind = "compaction",
        id = "c",
        retain_from = 3,
        compaction = {
          id = "c",
          status = "completed",
          summary = { { type = "text", text = "retained decisions" } },
        },
      },
      { kind = "assistant", text = "recent answer" },
    }, embedded)
    local text = table.concat(nvim.api.nvim_buf_get_lines(f.buffer, 0, -1, false), "\n")
    MiniTest.expect.equality(text:find("old history", 1, true), nil)
    for _, expected in ipairs({ "retained decisions", "current request", "recent answer", "## Source" }) do
      MiniTest.expect.equality(text:find(expected, 1, true) ~= nil, true)
    end
    MiniTest.expect.equality(f.owner:submit(f.buffer), false)
    text = text:gsub("<replace with the concrete action the target must take>", "finish the change")
    nvim.api.nvim_buf_set_lines(f.buffer, 0, -1, false, nvim.split(text, "\n", { plain = true }))
    assert(f.owner:submit(f.buffer))
    local content = f.sent[1].content
    if embedded then
      MiniTest.expect.equality(content[1].type, "text")
      MiniTest.expect.equality(content[1].text:find("retained decisions", 1, true), nil)
      MiniTest.expect.equality(content[2].type, "resource")
      MiniTest.expect.equality(content[2].resource.text:find("retained decisions", 1, true) ~= nil, true)
    else
      MiniTest.expect.equality(content, text)
    end
  end
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
