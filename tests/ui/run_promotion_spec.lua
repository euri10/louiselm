local MiniTest = require("mini.test")
local Promotion = require("louiselm.ui.run_promotion")
---@diagnostic disable-next-line: undefined-global -- Neovim runtime API.
local nvim = vim

local T = MiniTest.new_set()

local function fixture()
  local f = { decisions = 0, previous = nvim.api.nvim_get_current_win() }
  f.preview = {
    selection = { bead_id = "bead-1", run_id = "run-1" },
    approval_digest = "sha256:approval",
    changes = { added = { "new.txt" }, modified = { "old.txt" }, deleted = { "gone.txt" } },
  }
  function f:open()
    self.cancel = Promotion.prompt(self.preview, function(accepted, err)
      MiniTest.expect.equality(nvim.in_fast_event(), false)
      self.decisions = self.decisions + 1
      self.accepted, self.error = accepted, err
    end)
    MiniTest.finally(function()
      if self.cancel then
        self.cancel()
      end
    end)
    self.window = nvim.api.nvim_get_current_win()
    self.buffer = nvim.api.nvim_get_current_buf()
  end
  return f
end

-- Saved review has no deadline; bounded application permission is tested natively.
T["focused review shows exact paths without a countdown"] = function()
  local f = fixture()
  f:open()
  assert(f.window ~= f.previous)
  local text = table.concat(nvim.api.nvim_buf_get_lines(f.buffer, 0, -1, false), "\n")
  for _, value in ipairs({
    "bead-1",
    "run-1",
    "sha256:approval",
    "+ new.txt",
    "~ old.txt",
    "- gone.txt",
    "Accept exact preview",
    "Discard saved result",
  }) do
    assert(text:find(value, 1, true), text)
  end
  assert(not text:find("Expires", 1, true), text)
  assert(not text:find("Time remaining", 1, true), text)
  MiniTest.expect.equality(f.decisions, 0)
  nvim.api.nvim_feedkeys("a", "mx!", false)
  MiniTest.expect.equality(f.accepted, true)
  MiniTest.expect.equality(f.decisions, 1)
  MiniTest.expect.equality(nvim.api.nvim_win_is_valid(f.window), false)
  MiniTest.expect.equality(nvim.api.nvim_get_current_win(), f.previous)
end

T["only explicit discard rejects; closing or Escape keeps saved work pending"] = function()
  for _, action in ipairs({ "reject", "close", "escape" }) do
    local f = fixture()
    f:open()
    if action == "reject" then
      nvim.api.nvim_feedkeys("r", "mx!", false)
    elseif action == "close" then
      nvim.api.nvim_win_close(f.window, true)
    else
      nvim.api.nvim_feedkeys(nvim.api.nvim_replace_termcodes("<Esc>", true, false, true), "mx!", false)
    end
    if action == "reject" then
      MiniTest.expect.equality(f.accepted, false)
    else
      MiniTest.expect.equality(f.accepted, nil)
      MiniTest.expect.equality(f.error, "operator closed promotion review; saved result remains pending")
    end
    MiniTest.expect.equality(f.decisions, 1)
  end
end

T["saved review waits without deciding and still accepts later"] = function()
  local f = fixture()
  f:open()
  nvim.wait(50, function()
    return false
  end, 5)
  MiniTest.expect.equality(f.decisions, 0)
  MiniTest.expect.equality(nvim.api.nvim_win_is_valid(f.window), true)
  nvim.api.nvim_feedkeys("a", "mx!", false)
  MiniTest.expect.equality(f.accepted, true)
  MiniTest.expect.equality(f.decisions, 1)
end

T["q stops the Run without accepting or rejecting a task"] = function()
  local f = fixture()
  f:open()
  nvim.api.nvim_feedkeys("q", "mx!", false)
  MiniTest.expect.equality(f.accepted, nil)
  MiniTest.expect.equality(f.error, "operator stopped promotion review")
  MiniTest.expect.equality(f.decisions, 1)
end

T["cancelling review releases its window without deciding"] = function()
  local f = fixture()
  f:open()
  assert(type(f.cancel) == "function")
  f.cancel()
  f.cancel()
  nvim.wait(200, function()
    return false
  end, 5)
  MiniTest.expect.equality(f.decisions, 0)
  MiniTest.expect.equality(nvim.api.nvim_buf_is_valid(f.buffer), false)
end

T["tainted promotion retains exact-use review"] = function()
  local f = fixture()
  f.preview.tainted_review = {
    taint_digest = "sha256:taint",
    output_digest = "sha256:output",
    request_digest = "sha256:request",
    action = "workspace_promotion",
    destination = { uid = 1000, device = 1, inode = 2 },
  }
  f.preview.tainted_review_digest = "sha256:review"
  f:open()
  local text = table.concat(nvim.api.nvim_buf_get_lines(f.buffer, 0, -1, false), "\n")
  for _, value in ipairs({ "sha256:taint", "sha256:output", "sha256:request", "sha256:review", "UID 1000" }) do
    assert(text:find(value, 1, true), text)
  end
  nvim.api.nvim_feedkeys("r", "mx!", false)
  MiniTest.expect.equality(f.accepted, false)
end

return T
