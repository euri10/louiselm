local MiniTest = require("mini.test")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local T = MiniTest.new_set()
local DIGEST = "sha256:" .. string.rep("a", 64)
local HEAD = string.rep("a", 40)

local function fixture()
  local f = { calls = {}, callbacks = {}, killed = 0 }
  f.selection = {
    schema = "louiselm.operator.bead-run/2",
    envelope = {
      schema = "louiselm.broker.run-envelope/2",
      run_id = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee",
      envelope_id = "env",
      envelope_revision = 1,
      controller_uid = 1000,
      expires_at_ms = 0,
      verification_plan_digest = DIGEST,
      max_sessions = 6,
      bead_scope = {
        project_digest = DIGEST,
        role = "coordinator",
        issue_ids = { "b-1", "b-2", "b-3" },
        effects = { { kind = "comment_add" } },
        max_mutations = 3,
        expires_at_ms = 0,
      },
      provider_requests = { models = { "gpt-5.6-luna" }, max_effort = "high", max_run_requests = 12, expires_at_ms = 0 },
    },
    beads = { { id = "b-1", prompt = "First" }, { id = "b-2", prompt = "Second" }, { id = "b-3", prompt = "Third" } },
    manifest = {
      agent = { id = "codex" },
      envelope = { id = "env", revision = 1 },
      skills = { mode = "enabled", generation_digest = DIGEST, view_digest = DIGEST },
      cache_base_digest = DIGEST,
      isolation = { network = "denied" },
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
    self:reply(0, "run/" .. self.selection.envelope.run_id .. "\n")
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

T["draft construction and reopen retain intent without activating authority"] = function()
  local f = fixture()
  f.selection.schema = "louiselm.operator.bead-run/2"
  f.selection.envelope.expires_at_ms = 0
  f.selection.envelope.bead_scope.expires_at_ms = 0
  f.selection.envelope.provider_requests.expires_at_ms = 0
  f.selection.execution = { kind = "duration", duration_ms = 1800000 }
  local saved = nvim.json.encode(f.selection)
  for _ = 1, 2 do
    local restored = nvim.json.decode(saved)
    local controller, err = require("louiselm.workflow.operator").new(restored, f.options)
    assert(controller, err)
    MiniTest.expect.equality(controller.options.draft.execution, f.selection.execution)
    MiniTest.expect.equality(controller.options.envelope.expires_at_ms, 0)
    MiniTest.expect.equality(#f.calls, 0)
    assert(controller:dispose())
  end
end

T["opening a published revision constructs only a waiting controller"] = function()
  local f = fixture()
  local registration = string.rep("2", 32)
  local binding = {
    run_id = f.selection.envelope.run_id,
    project_id = string.rep("3", 32),
    source_id = string.rep("4", 32),
    checkout_id = string.rep("5", 32),
    agents = { "codex" },
  }
  local controller, identity, refusal
  local pending = assert(require("louiselm.workflow.prepared").open_registered(registration, 3, function(value, id, err)
    MiniTest.expect.equality(nvim.in_fast_event(), false)
    controller, identity, refusal, f.done = value, id, err, true
  end, f.options))
  MiniTest.expect.equality(#f.calls, 1)
  MiniTest.expect.equality(f.calls[1].argv, {
    "louiselm-capture",
    "--require-interface=1",
    "phone-control",
    "open-prepared",
    "--registration-id",
    registration,
    "--revision",
    "3",
  })
  f.calls[1].options.stdout(
    nil,
    nvim.json.encode({
      v = 1,
      type = "prepared_waiting",
      registration_id = registration,
      revision = 3,
      binding = binding,
      selection = nvim.json.encode(f.selection),
    })
  )
  f:reply(0)
  assert(controller, refusal)
  MiniTest.expect.equality(controller.status, "new")
  MiniTest.expect.equality(identity, { registration_id = registration, revision = 3, run_id = binding.run_id })
  MiniTest.expect.equality(controller.options.prepared, {
    registration_id = registration,
    revision = 3,
    selection_sha256 = nvim.fn.sha256(nvim.json.encode(f.selection)),
  })
  MiniTest.expect.equality(#f.calls, 1)
  assert(pending:cancel())
  assert(controller:dispose())
end

T["waiting open refuses stale bindings and discards late cancellation"] = function()
  local f = fixture()
  local registration = string.rep("2", 32)
  local response = {
    v = 1,
    type = "prepared_waiting",
    registration_id = registration,
    revision = 1,
    binding = {
      run_id = f.selection.envelope.run_id,
      project_id = string.rep("3", 32),
      source_id = string.rep("4", 32),
      checkout_id = string.rep("5", 32),
      agents = { "other" },
    },
    selection = nvim.json.encode(f.selection),
  }
  local controller, refusal
  assert(require("louiselm.workflow.prepared").open_registered(registration, 1, function(value, _, err)
    controller, refusal, f.done = value, err, true
  end, f.options))
  f.calls[1].options.stdout(nil, nvim.json.encode(response))
  f:reply(0)
  MiniTest.expect.equality(controller, nil)
  assert(refusal)
  f.done = false
  local pending = assert(require("louiselm.workflow.prepared").open_registered(registration, 1, function()
    f.done = true
  end, f.options))
  assert(pending:cancel())
  MiniTest.expect.equality(f.killed, 1)
  local callback = table.remove(f.callbacks, 1)
  callback({ code = 0, stdout = nvim.json.encode(response), stderr = "" })
  nvim.wait(50)
  MiniTest.expect.equality(f.done, false)
end

T["waiting open reports unsupported integration and validates before spawning"] = function()
  local f = fixture()
  local open = require("louiselm.workflow.prepared").open_registered
  for _, pair in ipairs({ { "bad", 1 }, { string.rep("0", 32), 1 }, { string.rep("2", 32), 0 } }) do
    local pending, err = open(pair[1], pair[2], function() end, f.options)
    MiniTest.expect.equality(pending, nil)
    assert(err)
  end
  MiniTest.expect.equality(#f.calls, 0)
  local refusal
  assert(open(string.rep("2", 32), 1, function(_, _, err)
    refusal, f.done = err, true
  end, f.options))
  f.calls[1].options.stderr(nil, "phone-control request refused: unsupported")
  table.remove(f.callbacks, 1)({ code = 1 })
  assert(nvim.wait(1000, function()
    return f.done
  end))
  MiniTest.expect.equality(refusal, "unsupported")
end

T["waiting open stops oversized subprocess output while reading"] = function()
  for _, spec in ipairs({ { "stdout", 262145 }, { "stderr", 8192 } }) do
    local f = fixture()
    local refusal
    assert(require("louiselm.workflow.prepared").open_registered(string.rep("2", 32), 1, function(_, _, err)
      refusal, f.done = err, true
    end, f.options))
    local streams = f.calls[1].options
    assert(
      type(streams.stdout) == "function" and type(streams.stderr) == "function",
      "output must be bounded while reading"
    )
    streams[spec[1]](nil, string.rep("x", spec[2]))
    streams[spec[1]](nil, "x")
    streams[spec[1]](nil, "ignored")
    MiniTest.expect.equality(f.killed, 1)
    table.remove(f.callbacks, 1)({ code = 1 })
    assert(nvim.wait(1000, function()
      return f.done
    end))
    assert(refusal and not refusal:find("xxx", 1, true))
  end
end

T["waiting open consumes a real asynchronous streamed response"] = function()
  local f = fixture()
  local registration = string.rep("2", 32)
  local response = {
    v = 1,
    type = "prepared_waiting",
    registration_id = registration,
    revision = 1,
    binding = {
      run_id = f.selection.envelope.run_id,
      project_id = string.rep("3", 32),
      source_id = string.rep("4", 32),
      checkout_id = string.rep("5", 32),
      agents = { "codex" },
    },
    selection = nvim.json.encode(f.selection),
  }
  f.options.system = function(_, options, done)
    return nvim.system({ "/usr/bin/printf", "%s", nvim.json.encode(response) }, options, done)
  end
  local controller, refusal
  assert(require("louiselm.workflow.prepared").open_registered(registration, 1, function(value, _, err)
    controller, refusal, f.done = value, err, true
  end, f.options))
  assert(nvim.wait(3000, function()
    return f.done
  end, 5))
  assert(controller, refusal)
  MiniTest.expect.equality(controller.status, "new")
  assert(controller:dispose())
end

T["draft defaults to relative execution and rejects ambiguous or active terms before effects"] = function()
  local f = fixture()
  local controller = assert(require("louiselm.workflow.operator").new(f.selection, f.options))
  MiniTest.expect.equality(controller.options.draft.execution, { kind = "duration", duration_ms = 1800000 })
  assert(controller:dispose())
  for _, execution in ipairs({
    false,
    "30m",
    {},
    { kind = "duration", duration_ms = 0 },
    { kind = "duration", duration_ms = 10, expires_at_ms = 10 },
    { kind = "stop_by", expires_at_ms = 1, duration_ms = 10 },
  }) do
    f.selection.execution = execution
    local invalid, err = require("louiselm.workflow.operator").new(f.selection, f.options)
    MiniTest.expect.equality(invalid, nil)
    assert(type(err) == "string")
  end
  f.selection.execution = { kind = "stop_by", expires_at_ms = 1 }
  controller = assert(require("louiselm.workflow.operator").new(f.selection, f.options))
  assert(controller:dispose(), "an elapsed explicit cutoff remains inspectable as a draft")
  for _, envelope in ipairs({
    f.selection.envelope,
    f.selection.envelope.bead_scope,
    f.selection.envelope.provider_requests,
  }) do
    envelope.expires_at_ms = 1
    local invalid, err = require("louiselm.workflow.operator").new(f.selection, f.options)
    MiniTest.expect.equality(invalid, nil)
    assert(type(err) == "string")
    envelope.expires_at_ms = 0
  end
  MiniTest.expect.equality(#f.calls, 0)
end

T["brokered worker refuses pre-cutover before snapshots or child grants"] = function()
  local f = fixture()
  f.selection.manifest.isolation.network = "brokered"
  f.controller = assert(require("louiselm.workflow.operator").new(f.selection, f.options))
  f:prepare("b-1", HEAD)
  MiniTest.expect.equality(f.calls[1].argv, {
    "louiselm-skills",
    "workspace",
    "qa-runtime",
    "inspect-installation",
    "--robot-json",
  })
  f:reply(0, { conformance = "pre_cutover", operator_uid = 1000, release_id = DIGEST, policy_digest = DIGEST })
  assert(f.done and not f.prepared)
  assert(f.error:find("Brokered workers require enforced conformance", 1, true))
  MiniTest.expect.equality(#f.calls, 1)
  assert(f.controller:dispose())
end

T["brokered worker requires valid current installed policy without changing it"] = function()
  for _, response in ipairs({
    {},
    { conformance = "enforced", operator_uid = 2000, release_id = DIGEST, policy_digest = DIGEST },
    { conformance = "unknown", operator_uid = 1000, release_id = DIGEST, policy_digest = DIGEST },
  }) do
    local f = fixture()
    f.selection.manifest.isolation.network = "brokered"
    f.controller = assert(require("louiselm.workflow.operator").new(f.selection, f.options))
    f:prepare("b-1", HEAD)
    f:reply(0, response)
    assert(f.done and not f.prepared and f.error)
    MiniTest.expect.equality(#f.calls, 1)
    assert(f.controller:dispose())
  end
  local f = fixture()
  f.selection.manifest.isolation.network = "brokered"
  f.controller = assert(require("louiselm.workflow.operator").new(f.selection, f.options))
  f:prepare("b-1", HEAD)
  f:reply(0, { conformance = "enforced", operator_uid = 1000, release_id = DIGEST, policy_digest = DIGEST })
  f:stage(HEAD)
  MiniTest.expect.equality(f.calls[#f.calls].argv, { "louiselm-control", "launch-inputs", "stage", "--json" })
  assert(f.controller:dispose())
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
  MiniTest.expect.equality(f.prepared.grant.role, "agent")
  MiniTest.expect.equality(f.prepared.verification.verifier_grant.role, "fixed_verifier")
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

T["preserves reviewed execution order against the canonical envelope set"] = function()
  local f = fixture()
  f.selection.beads = {
    { id = "b-3", prompt = "Third ID first" },
    { id = "b-1", prompt = "First ID second" },
    { id = "b-2", prompt = "Second ID last" },
  }
  local original = nvim.deepcopy(f.selection)
  f.controller = assert(require("louiselm.workflow.operator").new(f.selection, f.options))
  MiniTest.finally(function()
    assert(f.controller:dispose())
  end)
  MiniTest.expect.equality(#f.calls, 0)
  MiniTest.expect.equality(f.controller.options.bead_ids, { "b-3", "b-1", "b-2" })
  MiniTest.expect.equality(f.controller.options.envelope.bead_scope.issue_ids, { "b-1", "b-2", "b-3" })
  f:prepare("b-3", HEAD)
  f:stage(HEAD)
  f:reply(0, {
    schema = "louiselm.launch-inputs.staged/1",
    manifest_digest = DIGEST,
    source_snapshot_digest = DIGEST,
    source_base_digest = DIGEST,
    cache_base_digest = DIGEST,
    base_commit = HEAD,
  })
  assert(f.prepared, f.error)
  MiniTest.expect.equality(f.prepared.prompt, "Third ID first")
  MiniTest.expect.equality(f.prepared.grant.beads_mutations.issue_ids, { "b-3" })
  MiniTest.expect.equality(f.prepared.grant.request.session_id, f.selection.envelope.run_id .. "-worker-1")
  MiniTest.expect.equality(f.selection, original)
end

T["requires exact canonical scope membership before effects"] = function()
  for _, scope in ipairs({
    { "b-1", "b-2" },
    { "b-1", "b-2", "b-3", "b-4" },
    { "b-1", "b-2", "foreign" },
    { "b-1", "b-1", "b-3" },
    { "b-3", "b-1", "b-2" },
  }) do
    local f = fixture()
    f.selection.envelope.bead_scope.issue_ids = scope
    if scope[1] == "b-3" then
      f.selection.beads = { f.selection.beads[3], f.selection.beads[1], f.selection.beads[2] }
    end
    local controller, err = require("louiselm.workflow.operator").new(f.selection, f.options)
    MiniTest.finally(function()
      if controller then
        assert(controller:dispose())
      end
    end)
    MiniTest.expect.equality(controller, nil)
    MiniTest.expect.equality(err, "reviewed Beads must match the exact sorted Run envelope scope")
    MiniTest.expect.equality(#f.calls, 0)
  end
  local f = fixture()
  f.selection.beads[3].id = "b-1"
  local controller, err = require("louiselm.workflow.operator").new(f.selection, f.options)
  MiniTest.expect.equality(controller, nil)
  MiniTest.expect.equality(err, "operator Run requires unique reviewed Beads and explicit prompts")
  MiniTest.expect.equality(#f.calls, 0)
end

T["denied Run families refuse the Bead workflow before effects"] = function()
  for _, family in ipairs({ "bead_scope", "provider_requests" }) do
    for _, denied in ipairs({ nvim.NIL, false }) do
      local f = fixture()
      f.selection.envelope[family] = denied
      if denied == false then
        f.selection.envelope[family] = nil
      end
      local controller, err = require("louiselm.workflow.operator").new(f.selection, f.options)
      MiniTest.expect.equality(controller, nil)
      MiniTest.expect.equality(type(err), "string")
      MiniTest.expect.equality(#f.calls, 0)
    end
  end
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
    MiniTest.expect.equality(#f.calls, 6)
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
    f:reply(0, "run/" .. f.selection.envelope.run_id .. "\n")
    f:reply(0, preview)
    assert(f.done and not f.prepared and f.error)
    MiniTest.expect.equality(#f.calls, 3)
    assert(f.controller:dispose())
  end
end

T["Start refuses a changed Run branch before snapshot or authority"] = function()
  local f = fixture()
  f.controller = assert(require("louiselm.workflow.operator").new(f.selection, f.options))
  MiniTest.finally(function()
    assert(f.controller:dispose())
  end)
  f:prepare("b-1", HEAD)
  f:reply(0, "1000:1500:750\n")
  f:reply(0, "main\n")
  MiniTest.expect.equality(f.error, "Run checkout branch changed; review the draft again")
  MiniTest.expect.equality(#f.calls, 2)
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
