local MiniTest = require("mini.test")

local Correlate = require("louiselm.provenance.correlate")
local Sources = require("louiselm.provenance.sources")

local T = MiniTest.new_set()
local separator = string.char(30)
local nul = string.char(0)

-- Captured from `git log -n 4 --format='%H%x00%B%x00%x1e'` in this
-- repository during Session codex/01a03df9-6992-71e2-9050-1d4dd466011c.
-- The final record is a pre-trailer commit from the observed history.
local captured_log = table.concat({
  "32665fd3a1c99265496526a9a3116d0d8320182"
    .. nul
    .. "fix(session): guard unsafe editor exits\n\nRefs louiselm-ce6.34\n"
    .. nul,
  "5be054d50e2b48d231670933d896460e7d262b7a"
    .. nul
    .. "feat(session): resolve transcript paths\n\nRefs louiselm-dpik\nRefs codex/01a03df9-6992-71e2-9050-1d4dd466011c\n"
    .. nul,
  "0bcb2890022cf558a57c6f00257bd4f51e1b06ae" .. nul .. "chore(beads): plan diagnostics implementation\n" .. nul,
}, separator) .. separator

T["captured git log yields explicit issue and Session edges"] = function()
  local commits, parse_error = Sources.parse_git_log(captured_log)
  assert(parse_error == nil)
  assert(commits ~= nil)
  local edges, correlate_error = Correlate.commits(commits)
  assert(correlate_error == nil)
  assert(edges ~= nil)

  MiniTest.expect.equality(#edges, 3)
  MiniTest.expect.equality(edges[1], {
    source = { kind = "commit", id = "32665fd3a1c99265496526a9a3116d0d8320182" },
    target = { kind = "issue", id = "louiselm-ce6.34" },
    relation = "refs",
  })
  MiniTest.expect.equality(edges[2].target, { kind = "issue", id = "louiselm-dpik" })
  MiniTest.expect.equality(edges[3].target, { kind = "session", id = "codex/01a03df9-6992-71e2-9050-1d4dd466011c" })
end

T["stale references remain data rather than becoming errors"] = function()
  local edges, error_value = Correlate.commits({
    { id = "deadbeef", message = "chore: old\n\nRefs louiselm-no-longer-exists\n" },
  })
  assert(error_value == nil)
  assert(edges ~= nil)
  MiniTest.expect.equality(edges[1].target, { kind = "issue", id = "louiselm-no-longer-exists" })
end

T["invalid collected input returns a structured error"] = function()
  local malformed = {}
  rawset(malformed, "id", "commit")
  rawset(malformed, "message", false)
  local edges, error_value = Correlate.commits({ malformed })
  MiniTest.expect.equality(edges, nil)
  assert(error_value ~= nil)
  MiniTest.expect.equality(error_value.code, "invalid_commit")
  MiniTest.expect.equality(error_value.index, 1)
end

return T
