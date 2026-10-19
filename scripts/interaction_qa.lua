-- Trusted development init: use a separate Neovim and private XDG_STATE_HOME.
-- This configures offline Agents only; scripts/session-control starts each case.
---@diagnostic disable-next-line: undefined-global -- Neovim loads this explicit QA init.
local nvim = vim
local root = nvim.fn.getcwd()
assert(nvim.env.XDG_STATE_HOME and nvim.env.XDG_STATE_HOME:sub(1, 1) == "/", "set a private XDG_STATE_HOME")
assert(nvim.fn.filereadable(root .. "/tests/mock/init.lua") == 1, "run from the LouiseLM checkout")
assert(nvim.fn.executable("sqlite3") == 1, "sqlite3 is required")
nvim.opt.rtp:prepend(root)
assert(#require("louiselm.session").exit_verdict() == 0, "use a separate Neovim; existing Sessions must stay untouched")

local function agent(fixture)
  return {
    provider = "test-service",
    command = nvim.v.progpath,
    args = {
      "--headless",
      "--noplugin",
      "-i",
      "NONE",
      "-u",
      root .. "/tests/mock/init.lua",
      "-c",
      "lua require('louiselm.dev.mock_agent').run({mode='echo',generate_count=0})",
    },
    env = fixture and { LOUISELM_MOCK_INTERACTION_FILE = root .. "/tests/fixtures/" .. fixture } or {},
  }
end

local ok, report = require("louiselm").setup({
  agents = {
    ["replay-plan"] = agent("grok_exit_plan_mode.json"),
    ["replay-questions"] = agent("grok_ask_user_question.json"),
    ["replay-echo"] = agent(),
  },
})
assert(ok, report and report.text)
