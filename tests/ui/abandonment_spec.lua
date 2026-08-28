local MiniTest = require("mini.test")
local Abandonment = require("louiselm.ui.abandonment")

local T = MiniTest.new_set()

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

T["abandonment"] = MiniTest.new_set()

T["abandonment"]["records recovery truth and consumes the breadcrumb once"] = function()
  local directory = nvim.fn.tempname()
  local path = nvim.fs.joinpath(directory, "abandoned.json")
  local sessions = {
    {
      agent = "claude",
      acp_session_id = "claude-acp",
      recoverable = false,
      turn_active = false,
      staged = { contexts = 0, pending_skill = false, queued_prompt = false },
    },
    {
      agent = "codex",
      acp_session_id = "codex-acp",
      recoverable = true,
      turn_active = true,
      staged = { contexts = 2, pending_skill = false, queued_prompt = false },
    },
  }

  assert(Abandonment.write(path, sessions))
  local record = nvim.json.decode(table.concat(nvim.fn.readfile(path), "\n"))
  local mode = assert(nvim.uv.fs_stat(path)).mode % 512
  local message = assert(Abandonment.consume(path))

  MiniTest.expect.equality(record.version, 1)
  MiniTest.expect.equality(record.sessions, sessions)
  MiniTest.expect.equality(mode, 384)
  MiniTest.expect.equality(message:find("codex", 1, true) ~= nil, true)
  MiniTest.expect.equality(message:find("codex (codex-acp)", 1, true) ~= nil, true)
  MiniTest.expect.equality(message:find(":LouiselmResume", 1, true) ~= nil, true)
  MiniTest.expect.equality(message:find("claude", 1, true) ~= nil, true)
  MiniTest.expect.equality(nvim.uv.fs_stat(path), nil)
  MiniTest.expect.equality(Abandonment.consume(path), nil)
  nvim.fn.delete(directory, "rf")
end

T["abandonment"]["clears a malformed breadcrumb after reporting it"] = function()
  local directory = nvim.fn.tempname()
  local path = nvim.fs.joinpath(directory, "abandoned.json")
  assert(nvim.fn.mkdir(directory, "p") == 1)
  assert(nvim.fn.writefile({ "not json" }, path) == 0)

  local message, consume_error = Abandonment.consume(path)

  MiniTest.expect.equality(message, nil)
  MiniTest.expect.equality(consume_error, "abandonment breadcrumb is malformed")
  MiniTest.expect.equality(nvim.uv.fs_stat(path), nil)
  nvim.fn.delete(directory, "rf")
end

return T
