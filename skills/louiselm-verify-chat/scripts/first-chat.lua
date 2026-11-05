-- Run from the LouiseLM repository root with an isolated XDG_STATE_HOME.
---@diagnostic disable-next-line: undefined-global -- Neovim injects `vim` into this development script.
local nvim = vim
local root = nvim.fn.getcwd()
local evidence_path = nvim.env.LOUISELM_VERIFY_EVIDENCE
assert(type(evidence_path) == "string" and evidence_path ~= "", "set LOUISELM_VERIFY_EVIDENCE")
assert(type(nvim.env.XDG_STATE_HOME) == "string" and nvim.env.XDG_STATE_HOME ~= "", "set XDG_STATE_HOME")
assert(nvim.fn.getftype(evidence_path) == "", "evidence path already exists")
assert(nvim.fn.filereadable(root .. "/AGENTS.md") == 1, "run from the LouiseLM repository root")
assert(nvim.fn.executable("sqlite3") == 1, "sqlite3 is required")

nvim.opt.rtp:prepend(root)
local Session = require("louiselm.session")
local Command = require("louiselm.ui.chat.command")
local prompt = "verify first chat turn"

local function count_lines(lines, text)
  local count = 0
  for _, line in ipairs(lines) do
    if line:find(text, 1, true) then
      count = count + 1
    end
  end
  return count
end

local function mock_definition()
  return {
    provider = "test-service",
    command = nvim.v.progpath,
    args = {
      "--headless",
      "--noplugin",
      "-u",
      root .. "/tests/mock/init.lua",
      "-c",
      "lua require('louiselm.dev.mock_agent').run()",
    },
    env = { LOUISELM_MOCK_MODE = "echo" },
  }
end

local function run()
  assert(require("louiselm").setup({ agents = { mock = mock_definition() } }))
  nvim.cmd("LouiselmChat")
  local buffer = nvim.api.nvim_get_current_buf()
  assert(nvim.api.nvim_buf_get_name(buffer):match("^louiselm://"), "chat buffer did not open")
  assert(
    nvim.wait(5000, function()
      local sessions = Session.exit_verdict()
      return #sessions == 1 and sessions[1].session:inspect().status == "ready"
    end, 10),
    "mock Agent did not become ready"
  )
  local session = Session.exit_verdict()[1].session

  local lines = nvim.api.nvim_buf_get_lines(buffer, 0, -1, false)
  assert(lines[#lines] == "> ", "editable chat prompt is absent")
  nvim.api.nvim_buf_set_lines(buffer, #lines - 1, #lines, false, { "> " .. prompt })
  local submit
  for _, mapping in ipairs(nvim.api.nvim_buf_get_keymap(buffer, "i")) do
    if mapping.lhs == "<CR>" then
      submit = mapping.callback
    end
  end
  assert(type(submit) == "function", "chat Enter mapping is absent")
  submit()
  assert(
    nvim.wait(5000, function()
      return session:inspect().current_turn == 1
        and session:inspect().status == "ready"
        and count_lines(nvim.api.nvim_buf_get_lines(buffer, 0, -1, false), prompt) >= 2
    end, 10),
    "first prompt or echoed reply did not complete in the chat buffer"
  )
  assert(nvim.fn.writefile({
    nvim.json.encode({
      feature = "first-chat-turn",
      command = ":LouiselmChat",
      submit = "insert-mode Enter",
      prompt = prompt,
      turn = 1,
      reply_visible = true,
    }),
  }, evidence_path) == 0, "could not write evidence")
end

local ok, err = pcall(run)
local cleanup_ok, cleanup_err = pcall(function()
  Session.dispose_all()
  Command.configure(nil)
  Command.register()
end)
if not ok then
  error(err)
end
assert(cleanup_ok, cleanup_err)
assert(#Session.exit_verdict() == 0, "verification Session survived cleanup")
assert(nvim.fn.filereadable(evidence_path) == 1, "cleanup removed evidence")
