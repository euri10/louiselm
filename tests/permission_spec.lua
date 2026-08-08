local MiniTest = require("mini.test")
local Permission = require("louiselm.permission")

local T = MiniTest.new_set()

T["policy"] = MiniTest.new_set()

T["policy"]["defaults to asking a human"] = function()
  local policy = assert(Permission.policy())

  MiniTest.expect.equality(policy.name, "ask-human")
  MiniTest.expect.equality(Permission.gates.file_edit(policy, "/tmp/file.lua"), "ask")
  MiniTest.expect.equality(Permission.gates.command(policy, { "git", "status" }), "ask")
end

T["policy"]["rejects unknown policy names"] = function()
  local policy, err = Permission.policy("always")

  MiniTest.expect.equality(policy, nil)
  MiniTest.expect.equality(err, "permission policy must be one of: ask-human, auto-approve-scoped")
end

T["policy"]["auto-approves only scoped operations"] = function()
  local policy = assert(Permission.policy("auto-approve-scoped", {
    paths = { "/tmp/project" },
    commands = { { "git", "status" } },
  }))

  MiniTest.expect.equality(Permission.gates.file_edit(policy, "/tmp/project/lua/init.lua"), "allow")
  MiniTest.expect.equality(Permission.gates.file_edit(policy, "/tmp/project/../other/init.lua"), "deny")
  MiniTest.expect.equality(Permission.gates.file_edit(policy, "/tmp/other/init.lua"), "deny")
  MiniTest.expect.equality(Permission.gates.command(policy, { "git", "status" }), "allow")
  MiniTest.expect.equality(Permission.gates.command(policy, { "git", "push" }), "deny")
end

T["policy"]["does not coerce invalid operations"] = function()
  local policy = assert(Permission.policy())
  local decision, err = Permission.gates.command(policy, "git status")

  MiniTest.expect.equality(decision, nil)
  MiniTest.expect.equality(err, "permission command must be a dense array of strings")
end

T["policy"]["accepts ACP command strings for human review"] = function()
  local policy = assert(Permission.policy())
  local request = Permission.gates.from_acp({
    toolCall = { kind = "execute", rawInput = { command = "git status" } },
  })
  local decision, err = Permission.gates.check(policy, request)

  MiniTest.expect.equality(decision, "ask")
  MiniTest.expect.equality(err, nil)
end

return T
