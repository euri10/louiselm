local MiniTest = require("mini.test")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local T = MiniTest.new_set()
local DIGEST = "sha256:" .. string.rep("a", 64)
local HEAD = string.rep("a", 40)

local function fixture()
  local f = { calls = {}, callbacks = {}, killed = 0 }
  f.selection = {
    schema = "louiselm.operator.bead-run/1",
    envelope = {
      run_id = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee",
      envelope_id = "env",
      envelope_revision = 1,
      controller_uid = 1000,
      expires_at_ms = 9999999999999,
      verification_plan_digest = DIGEST,
      max_sessions = 6,
      bead_scope = {
        project_digest = DIGEST,
        role = "coordinator",
        issue_ids = { "b-1", "b-2", "b-3" },
        effects = { { kind = "comment_add" } },
        max_mutations = 3,
        expires_at_ms = 9999999999999,
      },
      provider_requests = { models = { "gpt-5.6-luna" }, max_effort = "high", max_run_requests = 12 },
    },
    beads = { { id = "b-1", prompt = "First" }, { id = "b-2", prompt = "Second" }, { id = "b-3", prompt = "Third" } },
    manifest = {
      agent = { id = "codex" },
      envelope = { id = "env", revision = 1 },
      skill_generation = { generation_digest = DIGEST },
      cache_base_digest = DIGEST,
    },
    input_group = 1500,
    snapshot_parent = "/private/inputs",
    cache = "/private/cache",
    plan = "/private/plan.json",
    worktree = { path = "/private/run", journal_parent = "/private/journals", head = HEAD },
  }
  f.options = {
    on_promotion = function() end,
    system = function(argv, options, done)
      f.calls[#f.calls + 1] = { argv = argv, options = options }
      f.callbacks[#f.callbacks + 1] = done
      return {
        kill = function()
          f.killed = f.killed + 1
        end,
      }
    end,
  }
  function f:reply(code, value)
    local callback = table.remove(self.callbacks, 1)
    assert(callback, "pending subprocess")
    local timer = assert(nvim.uv.new_timer())
    timer:start(0, 0, function()
      timer:close()
      callback({ code = code, stdout = type(value) == "table" and nvim.json.encode(value) or value or "", stderr = "" })
    end)
    assert(nvim.wait(1000, function()
      return #self.callbacks > 0 or self.done
    end, 5))
  end
  function f:prepare(id, head)
    self.done, self.prepared, self.error = false, nil, nil
    assert(self.controller.options.prepare(id, function(prepared, err)
      MiniTest.expect.equality(nvim.in_fast_event(), false)
      self.prepared, self.error, self.done = prepared, err, true
    end, head))
  end
  function f:stage(head, snapshot_digest)
    self:reply(0, "1000:1500:750\n")
    self:reply(0, {
      schema = "louiselm.workspace.preview/1",
      snapshot_digest = snapshot_digest or DIGEST,
      base_digest = DIGEST,
      base_commit = head,
      changes = {},
    })
    self:reply(0)
    self:reply(0)
    return self.calls[#self.calls].options.stdin and nvim.json.decode(self.calls[#self.calls].options.stdin)
  end
  return f
end

T["prepares exact HEAD and fresh scoped worker/verifier grants"] = function()
  local f = fixture()
  f.controller = assert(require("louiselm.workflow.operator").new(f.selection, f.options))
  f:prepare("b-1", HEAD)
  local request = f:stage(HEAD)
  MiniTest.expect.equality(request.expected_base_commit, HEAD)
  MiniTest.expect.equality(request.manifest.source_snapshot_digest, DIGEST)
  f:reply(0, {
    schema = "louiselm.launch-inputs.staged/1",
    manifest_digest = DIGEST,
    source_snapshot_digest = DIGEST,
    source_base_digest = DIGEST,
    cache_base_digest = DIGEST,
    base_commit = HEAD,
  })
  assert(f.prepared, f.error)
  MiniTest.expect.equality(f.prepared.grant.beads_mutations.issue_ids, { "b-1" })
  MiniTest.expect.equality(f.prepared.grant.beads_mutations.role, "worker")
  MiniTest.expect.equality(f.prepared.grant.provider_requests, f.selection.envelope.provider_requests)
  MiniTest.expect.equality(f.prepared.verification.verifier_grant.provider_requests, nvim.NIL)
  local first = f.prepared.grant.request.session_id
  local head = string.rep("b", 40)
  local snapshot = "sha256:" .. string.rep("b", 64)
  f:prepare("b-2", head)
  request = f:stage(head, snapshot)
  MiniTest.expect.equality(request.expected_base_commit, head)
  f:reply(0, {
    schema = "louiselm.launch-inputs.staged/1",
    manifest_digest = snapshot,
    source_snapshot_digest = snapshot,
    source_base_digest = DIGEST,
    cache_base_digest = DIGEST,
    base_commit = head,
  })
  assert(f.prepared, f.error)
  assert(f.prepared.grant.request.session_id ~= first)
  assert(f.prepared.verification.verifier_grant.request.session_id ~= f.prepared.grant.request.session_id)
  MiniTest.expect.equality(f.prepared.base_commit, head)
  MiniTest.expect.equality(f.selection.manifest.source_snapshot_digest, nil)
  assert(f.controller:dispose())
end

T["reuses only confirmed identical input binding without resending staging"] = function()
  local f = fixture()
  f.controller = assert(require("louiselm.workflow.operator").new(f.selection, f.options))
  f:prepare("b-1", HEAD)
  f:stage(HEAD)
  f:reply(0, {
    schema = "louiselm.launch-inputs.staged/1",
    manifest_digest = DIGEST,
    source_snapshot_digest = DIGEST,
    source_base_digest = DIGEST,
    cache_base_digest = DIGEST,
    base_commit = HEAD,
  })
  f:prepare("b-2", HEAD)
  f:stage(HEAD)
  assert(f.done and f.prepared, f.error)
  local stages = 0
  for _, call in ipairs(f.calls) do
    if call.argv[2] == "launch-inputs" then
      stages = stages + 1
    end
  end
  MiniTest.expect.equality(stages, 1)
  assert(f.controller:dispose())
end

T["refuses malformed or uncertain staging without retry"] = function()
  for _, result in ipairs({
    { code = 6, value = "" },
    { code = 0, value = "{}" },
    {
      code = 0,
      value = {
        schema = "louiselm.launch-inputs.staged/1",
        manifest_digest = DIGEST,
        source_snapshot_digest = DIGEST,
        source_base_digest = DIGEST,
        cache_base_digest = DIGEST,
        base_commit = string.rep("f", 40),
      },
    },
  }) do
    local f = fixture()
    f.controller = assert(require("louiselm.workflow.operator").new(f.selection, f.options))
    f:prepare("b-1", HEAD)
    f:stage(HEAD)
    f:reply(result.code, result.value)
    assert(f.done and not f.prepared and f.error)
    MiniTest.expect.equality(#f.calls, 5)
    assert(f.controller:dispose())
  end
end

T["disposal cancels pending preparation and ignores late callback"] = function()
  local f = fixture()
  f.controller = assert(require("louiselm.workflow.operator").new(f.selection, f.options))
  f:prepare("b-1", HEAD)
  local late = table.remove(f.callbacks, 1)
  assert(f.controller:dispose())
  MiniTest.expect.equality(f.killed, 1)
  late({ code = 0, stdout = "1000:1500:750\n", stderr = "" })
  nvim.wait(20, function()
    return false
  end, 5)
  MiniTest.expect.equality(#f.calls, 1)
  MiniTest.expect.equality(f.done, false)
end

T["rejects off-list, extra options and unsafe input group before effects"] = function()
  local f = fixture()
  f.selection.extra = true
  MiniTest.expect.equality(require("louiselm.workflow.operator").new(f.selection, f.options), nil)
  f.selection.extra = nil
  f.selection.input_group = 0
  MiniTest.expect.equality(require("louiselm.workflow.operator").new(f.selection, f.options), nil)
  f.selection.input_group = 1500
  f.selection.beads[1].id = "foreign"
  MiniTest.expect.equality(require("louiselm.workflow.operator").new(f.selection, f.options), nil)
  MiniTest.expect.equality(#f.calls, 0)
end

T["refuses public input directories, stale HEAD and dirty checkout before sharing"] = function()
  for _, metadata in ipairs({ "1000:1500:755\n", "0:1500:750\n", "1000:1000:750\n" }) do
    local f = fixture()
    f.controller = assert(require("louiselm.workflow.operator").new(f.selection, f.options))
    f:prepare("b-1", HEAD)
    f:reply(0, metadata)
    assert(f.done and not f.prepared and f.error)
    MiniTest.expect.equality(#f.calls, 1)
    assert(f.controller:dispose())
  end
  for _, preview in ipairs({
    { base_commit = string.rep("b", 40), snapshot_digest = DIGEST, base_digest = DIGEST, changes = {} },
    {
      base_commit = HEAD,
      snapshot_digest = DIGEST,
      base_digest = DIGEST,
      changes = { { kind = "untracked", included = false } },
    },
  }) do
    local f = fixture()
    f.controller = assert(require("louiselm.workflow.operator").new(f.selection, f.options))
    f:prepare("b-1", HEAD)
    f:reply(0, "1000:1500:750\n")
    f:reply(0, preview)
    assert(f.done and not f.prepared and f.error)
    MiniTest.expect.equality(#f.calls, 2)
    assert(f.controller:dispose())
  end
end

T["refuses implicit options defaults and excess first-Run authority"] = function()
  local f = fixture()
  local malformed_options = nvim.json.decode("false")
  MiniTest.expect.equality(require("louiselm.workflow.operator").new(f.selection, malformed_options), nil)
  f.selection.envelope.max_sessions = 8
  MiniTest.expect.equality(require("louiselm.workflow.operator").new(f.selection, f.options), nil)
  f.selection.envelope.max_sessions = 6
  f.selection.envelope.provider_requests.models = { "gpt-6-astra" }
  MiniTest.expect.equality(require("louiselm.workflow.operator").new(f.selection, f.options), nil)
  MiniTest.expect.equality(#f.calls, 0)
end

return T
