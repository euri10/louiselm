local MiniTest = require("mini.test")
local Session = require("louiselm.session")
local Chat = require("louiselm.ui.chat")

---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local T = MiniTest.new_set()

T["restores reasoning replayed while the chat is hidden"] = function()
  -- louiselm-p35y: exercise actual session/load notifications, not synthetic
  -- fold deletion. Hidden replay is a deterministic lifecycle case; the exact
  -- window ordering of the original OpenCode report was not captured.
  local directory = nvim.fn.tempname()
  local api = assert(Session.new({
    mock = {
      provider = "test-service",
      command = nvim.v.progpath,
      args = {
        "--headless",
        "--noplugin",
        "-u",
        nvim.fn.getcwd() .. "/tests/mock/init.lua",
        "-c",
        "lua require('louiselm.dev.mock_agent').run()",
      },
      env = { LOUISELM_MOCK_REPLAY_REASONING = "historical reasoning" },
    },
  }, nil, { usage_directory = directory }))
  MiniTest.finally(function()
    assert(api:dispose())
    local settled, failure = false, nil
    api:flush_recording(function(err)
      settled, failure = true, err
    end)
    assert(nvim.wait(6000, function()
      return settled
    end, 10))
    assert(failure == nil, failure and failure.message)
    nvim.fn.delete(directory, "rf")
  end)
  local chat = assert(Chat.new(api))
  MiniTest.finally(function()
    chat:dispose()
  end)
  local session = assert(api:load_session("mock", "reload-fold-fixture", { cwd = nvim.fn.getcwd() }))
  assert(chat:attach(session))
  local buffer = chat:buffer()
  local other = nvim.api.nvim_create_buf(false, true)
  MiniTest.finally(function()
    if nvim.api.nvim_buf_is_valid(other) then
      nvim.api.nvim_buf_delete(other, { force = true })
    end
  end)
  nvim.api.nvim_win_set_buf(0, other)
  assert(nvim.wait(6000, function()
    local lines = nvim.api.nvim_buf_get_lines(buffer, 0, -1, false)
    return table.concat(lines, "\n"):find("status=ready", 1, true) ~= nil
      and nvim.tbl_contains(lines, "historical reasoning")
  end, 10))
  assert(chat:switch(session:inspect().id))
  local headers = {}
  for line, text in ipairs(nvim.api.nvim_buf_get_lines(buffer, 0, -1, false)) do
    if text == "[thinking]" then
      headers[#headers + 1] = line
    end
  end
  MiniTest.expect.equality(#headers, 1)
  MiniTest.expect.equality(nvim.fn.foldclosed(headers[1]), headers[1])
  MiniTest.expect.equality(nvim.fn.foldclosedend(headers[1]), headers[1] + 1)
  -- Re-entering an already folded chat must not create nested folds.
  assert(chat:switch(session:inspect().id))
  MiniTest.expect.equality(nvim.fn.foldlevel(headers[1]), 1)
  -- Showing the chat must preserve a fold the operator deliberately opened.
  nvim.api.nvim_win_set_cursor(0, { headers[1], 0 })
  nvim.cmd("normal! zo")
  assert(chat:switch(session:inspect().id))
  MiniTest.expect.equality(nvim.fn.foldlevel(headers[1]), 1)
  MiniTest.expect.equality(nvim.fn.foldclosed(headers[1]), -1)
end

return T
