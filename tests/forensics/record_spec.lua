local MiniTest = require("mini.test")
local Record = require("louiselm.forensics.record")
local Store = require("louiselm.forensics.store")

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim
local temp_dir
local T = MiniTest.new_set({
  hooks = {
    pre_case = function()
      temp_dir = nvim.fn.tempname()
      assert(nvim.fn.mkdir(temp_dir, "p") == 1)
    end,
    post_case = function()
      nvim.fn.delete(temp_dir, "rf")
      temp_dir = nil
    end,
  },
})

local function input()
  return {
    id = "record-1",
    observed_at = 100,
    subject = { agent = "codex", acp_session_id = "acp-1" },
    diagnosing_session = "claude/acp-diagnoser",
    observations = {
      agent = "Codex",
      agent_version = "1.2.3\nsecret details",
      cwd = "/tmp/project",
      options = { model = "gpt-5", reasoning = true },
      capabilities = { load_session = true },
      dirty_files = { "a.lua", "b.lua" },
    },
    evidence_sources = {
      { kind = "acp_log", state = "present", path = "/tmp/session.log", mutable = true },
    },
  }
end

T["build"] = MiniTest.new_set()

T["build"]["creates a bounded allowlisted record"] = function()
  local record = assert(Record.build(input()))

  MiniTest.expect.equality(record.schema_version, 1)
  MiniTest.expect.equality(record.subject, { agent = "codex", acp_session_id = "acp-1" })
  MiniTest.expect.equality(record.observations.options, { model = "gpt-5", reasoning = true })
  MiniTest.expect.equality(record.observations.agent_version, "1.2.3\nsecret details")
  MiniTest.expect.equality(record.evidence_sources[1].mutable, true)
end

T["build"]["rejects missing subject identity"] = function()
  local value = input()
  value.subject.acp_session_id = nil

  local record, error_message = Record.build(value)

  MiniTest.expect.equality(record, nil)
  MiniTest.expect.equality(error_message, "forensics subject requires an ACP Session ID")
end

T["store"] = MiniTest.new_set()

T["store"]["publishes and reads an immutable private record"] = function()
  local store = assert(Store.new(nvim.fs.joinpath(temp_dir, "forensics")))
  local path = assert(store:write(input()))
  local loaded = assert(store:read(path))

  MiniTest.expect.equality(loaded.subject, { agent = "codex", acp_session_id = "acp-1" })
  MiniTest.expect.equality(nvim.uv.fs_stat(path).mode % 512, 384)

  loaded.subject.agent = "changed"
  local reread = assert(store:read(path))
  MiniTest.expect.equality(reread.subject.agent, "codex")
end

return T
