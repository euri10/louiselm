local MiniTest = require("mini.test")
local Permission = require("louiselm.permission")

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim
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

T["policy"]["accepts Antigravity CommandLine without splitting shell text"] = function()
  -- Antigravity ACP 1.1.1, Session d04b77b3-6061-4a74-9f01-8a3f9ef67155:
  -- ~/.local/state/acp-llm-adapter/proxy/sessions/<session-id>/log.jsonl,
  -- session/request_permission. Shape captured; compound text is a safety case.
  local data = {
    toolCall = {
      kind = "execute",
      rawInput = { CommandLine = "pwd", Cwd = "/tmp/louiselm-antigravity.KEcmf0", WaitMsBeforeAsync = 5000 },
    },
  }
  local request = Permission.gates.from_acp(data)
  MiniTest.expect.equality(request, { kind = "command", command = { "pwd" } })
  MiniTest.expect.equality(Permission.gates.check(Permission.ask_human(), request), "ask")
  MiniTest.expect.equality(data.toolCall.rawInput.CommandLine, "pwd")

  local scoped = assert(Permission.auto_approve_scoped({ commands = { { "pwd" } } }))
  data.toolCall.rawInput.CommandLine = "pwd && echo unapproved"
  request = Permission.gates.from_acp(data)
  MiniTest.expect.equality(request.command, { "pwd && echo unapproved" })
  MiniTest.expect.equality(Permission.gates.check(scoped, request), "deny")
end

T["remembered"] = MiniTest.new_set()

local function permission_context(workspace)
  return {
    session_id = "session-1",
    agent = "codex",
    adapter = { command = "codex-acp", args = { "serve" } },
    workspace = workspace,
  }
end

T["remembered"]["persists exact adapter and workspace command prefixes"] = function()
  local root = nvim.fn.tempname()
  local path = nvim.fs.joinpath(root, "permissions.json")
  local context = permission_context(nvim.fs.joinpath(root, "workspace"))
  local store = assert(Permission.store(path))

  local rule = assert(store:remember(context, { kind = "command", command = { "git", "status" } }, "allow", "always"))
  MiniTest.expect.equality(rule.lifetime, "always")

  local reloaded = assert(Permission.store(path))
  MiniTest.expect.equality(
    reloaded:evaluate(context, { kind = "command", command = { "git", "status", "--short" } }),
    "allow"
  )
  MiniTest.expect.equality(reloaded:evaluate(context, { kind = "command", command = { "git", "push" } }), "ask")
  local other_workspace = permission_context(nvim.fs.joinpath(root, "other"))
  MiniTest.expect.equality(
    reloaded:evaluate(other_workspace, { kind = "command", command = { "git", "status" } }),
    "ask"
  )
  local other_adapter = permission_context(context.workspace)
  other_adapter.adapter.args = { "other" }
  MiniTest.expect.equality(reloaded:evaluate(other_adapter, { kind = "command", command = { "git", "status" } }), "ask")
  nvim.fn.delete(root, "rf")
end

T["remembered"]["does not apply a rule remembered for one agent to a handoff target on another"] = function()
  local root = nvim.fn.tempname()
  local path = nvim.fs.joinpath(root, "permissions.json")
  local context = permission_context(nvim.fs.joinpath(root, "workspace"))
  local store = assert(Permission.store(path))

  assert(store:remember(context, { kind = "command", command = { "git", "status" } }, "allow", "always"))
  MiniTest.expect.equality(store:evaluate(context, { kind = "command", command = { "git", "status" } }), "allow")

  local other_agent = permission_context(context.workspace)
  other_agent.agent = "claude"
  MiniTest.expect.equality(store:evaluate(other_agent, { kind = "command", command = { "git", "status" } }), "ask")
  nvim.fn.delete(root, "rf")
end

T["remembered"]["keeps session rules in memory and file rules scoped to one exact path"] = function()
  local root = nvim.fn.tempname()
  local path = nvim.fs.joinpath(root, "permissions.json")
  local workspace = nvim.fs.joinpath(root, "workspace")
  local context = permission_context(workspace)
  local store = assert(Permission.store(path))
  local exact_path = nvim.fs.joinpath(workspace, "lua", "init.lua")

  assert(store:remember(context, { kind = "file_edit", path = "lua/init.lua" }, "deny", "session"))
  MiniTest.expect.equality(store:evaluate(context, { kind = "file_edit", path = exact_path }), "deny")
  MiniTest.expect.equality(
    store:evaluate(context, { kind = "file_edit", path = nvim.fs.joinpath(workspace, "lua", "other.lua") }),
    "ask"
  )
  assert(store:clear_session("session-1"))
  MiniTest.expect.equality(store:evaluate(context, { kind = "file_edit", path = exact_path }), "ask")

  assert(store:remember(context, { kind = "file_edit", path = exact_path }, "allow", "always"))
  local reloaded = assert(Permission.store(path))
  MiniTest.expect.equality(reloaded:evaluate(context, { kind = "file_edit", path = exact_path }), "allow")
  nvim.fn.delete(root, "rf")
end

T["remembered"]["lists and revokes rules without exposing mutable state"] = function()
  local root = nvim.fn.tempname()
  local path = nvim.fs.joinpath(root, "permissions.json")
  local context = permission_context(root)
  local store = assert(Permission.store(path))
  local rule = assert(store:remember(context, { kind = "command", command = { "make", "test" } }, "allow", "always"))

  local rules = assert(store:list())
  rules[1].command[1] = "mutated"
  MiniTest.expect.equality(assert(store:list())[1].command, { "make", "test" })
  MiniTest.expect.equality(store:revoke(rule.id), true)
  MiniTest.expect.equality(assert(store:list()), {})
  MiniTest.expect.equality(store:revoke(rule.id), false)
  nvim.fn.delete(root, "rf")
end

T["remembered"]["fails closed without replacing malformed state"] = function()
  local root = nvim.fn.tempname()
  assert(nvim.fn.mkdir(root, "p") == 1)
  local path = nvim.fs.joinpath(root, "permissions.json")
  assert(nvim.fn.writefile({ "not json" }, path) == 0)
  local context = permission_context(root)
  local store = assert(Permission.store(path))

  local decision, read_error = store:evaluate(context, { kind = "command", command = { "git" } })
  local rule, write_error = store:remember(context, { kind = "command", command = { "git" } }, "allow", "always")

  MiniTest.expect.equality(decision, nil)
  MiniTest.expect.equality(read_error, "permission state is not valid JSON")
  MiniTest.expect.equality(rule, nil)
  MiniTest.expect.equality(write_error, "permission state is not valid JSON")
  MiniTest.expect.equality(nvim.fn.readfile(path), { "not json" })
  nvim.fn.delete(root, "rf")
end

T["remembered"]["uses exact ACP option kinds instead of labels"] = function()
  local data = {
    options = {
      { optionId = "once", name = "Always allow this", kind = "allow_once" },
      { optionId = "forever", name = "Continue", kind = "allow_always" },
      { optionId = "custom", name = "Allow forever", kind = "adapter_custom" },
    },
  }

  MiniTest.expect.equality(
    { Permission.gates.remembered_choice(data, { outcome = { outcome = "selected", optionId = "forever" } }) },
    { "allow", "always" }
  )
  MiniTest.expect.equality(
    Permission.gates.remembered_choice(data, { outcome = { outcome = "selected", optionId = "custom" } }),
    nil
  )
  MiniTest.expect.equality(Permission.gates.remembered_response(data, "allow"), {
    outcome = { outcome = "selected", optionId = "once" },
  })
  MiniTest.expect.equality(Permission.gates.remembered_response({ options = { "allow" } }, "allow"), nil)
end

return T
