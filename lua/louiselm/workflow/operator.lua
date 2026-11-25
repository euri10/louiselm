---Concrete operator preparation for a reviewed, finite VM Bead Run.
local Workflow = require("louiselm.workflow")
local Service = require("louiselm.workflow.service")
---@diagnostic disable-next-line: undefined-global -- Neovim runtime API.
local nvim = vim
local M = {}

---@class louiselm.workflow.OperatorBeadSelection
---@field schema "louiselm.operator.bead-run/2"
---@field envelope table Complete inactive Rust RunEnvelope; all expiries zero, revision one, no credential.
---@field execution? { kind: "duration", duration_ms: integer }|{ kind: "stop_by", expires_at_ms: integer } Defaults to thirty minutes from dispatch; an explicit cutoff never shifts.
---@field beads { id: string, prompt: string }[] Exact reviewed order, three to five Beads.
---@field manifest table Complete resolved SessionInputManifest; source identities are refreshed.
---@field cache string Broker-readable immutable cache selected by the operator.
---@field plan string Broker-readable fixed verification plan bound by the envelope.
---@field snapshot_parent string Existing operator-owned directory, mode 0750 and input_group.
---@field input_group integer Trusted group containing only the operator and broker, not Session identities.
---@field worktree { path: string, journal_parent: string, head: string } Dedicated clean run/<run-id> checkout.

---@class louiselm.workflow.OperatorBeadOptions
---@field system? fun(command: string[], options: table, callback: fun(result: table)): unknown Async subprocess seam.
---@field session_api? louiselm.session.Api Session seam for deterministic tests.
---@field on_promotion? fun(preview: table, decide: fun(accepted: boolean?, error_message?: string)): louiselm.workflow.PromotionCancellation? Defaults to focused manual review; optional return cancels its UI.
---@field prepared? { registration_id: string, revision: integer, selection_sha256: string } Exact owner-selected registration.

local function digest(value)
  return type(value) == "string" and #value == 71 and value:match("^sha256:[0-9a-f]+$") ~= nil
end

local function commit(value)
  return type(value) == "string" and (#value == 40 or #value == 64) and value:match("^[0-9a-f]+$") ~= nil
end

local function absolute(value)
  return type(value) == "string" and value:sub(1, 1) == "/" and not value:find("\0", 1, true)
end

local function closed(value, fields)
  if type(value) ~= "table" then
    return false
  end
  for key in pairs(value) do
    if not nvim.tbl_contains(fields, key) then
      return false
    end
  end
  return true
end

local function validate(selection, options)
  if
    not closed(selection, {
      "schema",
      "envelope",
      "execution",
      "beads",
      "manifest",
      "cache",
      "plan",
      "snapshot_parent",
      "input_group",
      "worktree",
    })
    or selection.schema ~= "louiselm.operator.bead-run/2"
    or not closed(options, { "system", "session_api", "on_promotion", "prepared" })
    or (options.system ~= nil and type(options.system) ~= "function")
    or (options.on_promotion ~= nil and type(options.on_promotion) ~= "function")
  then
    return nil, "invalid closed operator Run selection or options"
  end
  local valid_prepared, prepared_error = Service.validate_prepared(options.prepared)
  if not valid_prepared then
    return nil, prepared_error
  end
  local envelope, manifest, beads, worktree =
    selection.envelope, selection.manifest, selection.beads, selection.worktree
  if
    type(envelope) ~= "table"
    or type(envelope.bead_scope) ~= "table"
    or type(envelope.provider_requests) ~= "table"
    or type(manifest) ~= "table"
    or type(manifest.agent) ~= "table"
    or type(manifest.agent.id) ~= "string"
    or type(manifest.envelope) ~= "table"
    or type(manifest.isolation) ~= "table"
    or not nvim.tbl_contains({ "denied", "brokered" }, manifest.isolation.network)
    or require("louiselm.acp.launch").encode_skills(manifest.skills) == nil
    or not digest(manifest.cache_base_digest)
    or manifest.envelope.id ~= envelope.envelope_id
    or manifest.envelope.revision ~= envelope.envelope_revision
    or type(envelope.run_id) ~= "string"
    or #envelope.run_id > 64
    or not envelope.run_id:match("^[%w_%-]+$")
    or type(envelope.controller_uid) ~= "number"
    or envelope.controller_uid <= 0
    or envelope.controller_uid % 1 ~= 0
    or type(beads) ~= "table"
    or not nvim.islist(beads)
    or #beads < 3
    or #beads > 5
    or not closed(worktree, { "path", "journal_parent", "head" })
    or not absolute(worktree.path)
    or not absolute(worktree.journal_parent)
    or not commit(worktree.head)
    or not absolute(selection.cache)
    or not absolute(selection.plan)
    or not absolute(selection.snapshot_parent)
    or type(selection.input_group) ~= "number"
    or selection.input_group <= 0
    or selection.input_group >= 4294967296
    or selection.input_group % 1 ~= 0
  then
    return nil, "operator Run requires resolved inputs, private sharing and exactly three to five Beads"
  end
  local provider = envelope.provider_requests
  local execution = selection.execution
  if execution == nil then
    execution = { kind = "duration", duration_ms = 1800000 }
  end
  if type(execution) ~= "table" then
    return nil, "Run draft requires a duration or explicit stop-by cutoff"
  end
  local limit = execution.kind == "duration" and execution.duration_ms or execution.expires_at_ms
  if
    not closed(execution, execution.kind == "duration" and { "kind", "duration_ms" } or { "kind", "expires_at_ms" })
    or not nvim.tbl_contains({ "duration", "stop_by" }, execution.kind)
    or type(limit) ~= "number"
    or limit <= 0
    or limit > 9007199254740991
    or limit % 1 ~= 0
    or envelope.envelope_revision ~= 1
    or envelope.expires_at_ms ~= 0
  then
    return nil, "Run draft requires an inactive envelope and a finite duration or explicit stop-by cutoff"
  end
  for _, key in ipairs({ "bead_scope", "provider_requests", "commands", "skill_requests" }) do
    if
      envelope[key] ~= nil
      and envelope[key] ~= nvim.NIL
      and (type(envelope[key]) ~= "table" or envelope[key].expires_at_ms ~= 0)
    then
      return nil, "Run draft capability expiries must be zero until dispatch"
    end
  end
  if
    type(provider.max_run_requests) ~= "number"
    or provider.max_run_requests < 1
    or provider.max_run_requests > 10000
    or provider.max_run_requests % 1 ~= 0
    or not nvim.deep_equal(provider.models, { "gpt-5.6-luna" })
    or not nvim.tbl_contains({ "none", "minimal", "low", "medium", "high" }, provider.max_effort)
  then
    return nil, "first Run requires a finite Luna request cap and at most high reasoning"
  end
  local ids, prompts = {}, {}
  for index, bead in ipairs(beads) do
    if
      not closed(bead, { "id", "prompt" })
      or type(bead.id) ~= "string"
      or bead.id == ""
      or prompts[bead.id]
      or type(bead.prompt) ~= "string"
      or bead.prompt == ""
    then
      return nil, "operator Run requires unique reviewed Beads and explicit prompts"
    end
    ids[index], prompts[bead.id] = bead.id, bead.prompt
  end
  local scope_ids = nvim.deepcopy(ids)
  table.sort(scope_ids)
  if not nvim.deep_equal(scope_ids, envelope.bead_scope.issue_ids) then
    return nil, "reviewed Beads must match the exact sorted Run envelope scope"
  end
  if
    envelope.max_sessions ~= 2 * #ids
    or envelope.bead_scope.max_mutations ~= #ids
    or envelope.bead_scope.role ~= "coordinator"
    or not nvim.deep_equal(envelope.bead_scope.effects, { { kind = "comment_add" } })
    or (envelope.commands ~= nil and envelope.commands ~= nvim.NIL)
  then
    return nil, "first Run approval must reserve exactly its child pairs and failure comments, without command grants"
  end
  return ids
end

local function grants(selection, id, index, binding)
  local envelope = selection.envelope
  local function grant(role)
    local session_id = envelope.run_id .. "-" .. role .. "-" .. index
    return {
      role = role == "verifier" and "fixed_verifier" or "agent",
      request = {
        schema = "louiselm.launch.request/3",
        protocol_version = 1,
        request_id = session_id .. "-request",
        authorization_id = session_id .. "-authorization",
        session_id = session_id,
        run_id = envelope.run_id,
        agent_id = selection.manifest.agent.id,
        envelope_id = envelope.envelope_id,
        envelope_revision = envelope.envelope_revision,
        skills = selection.manifest.skills,
        session_input_manifest_id = binding.manifest_digest,
      },
      controller_uid = envelope.controller_uid,
      expires_at_ms = envelope.expires_at_ms,
      broker_loss_grace_ms = 5000,
      require_cold_recovery = false,
      conformance = { attendance = "unattended", waiver = nvim.NIL },
      dependencies = nvim.NIL,
      commands = nvim.NIL,
      skill_requests = nvim.NIL,
      beads_mutations = nvim.NIL,
      provider_requests = nvim.NIL,
    }
  end
  local worker, verifier = grant("worker"), grant("verifier")
  worker.provider_requests = nvim.deepcopy(envelope.provider_requests)
  worker.beads_mutations = nvim.deepcopy(envelope.bead_scope)
  worker.beads_mutations.role = "worker"
  worker.beads_mutations.issue_ids = { id }
  worker.beads_mutations.effects = { { kind = "comment_add" } }
  worker.beads_mutations.max_mutations = 1
  return worker, verifier
end

---Construct the real caller without effects; start() admits the real capture ledger.
---Owns async snapshot processes and cancels them on dispose. Never retries staging,
---reads credentials, renews approval, or enables Verified posture. A new Run ID is mandatory.
---@param selection louiselm.workflow.OperatorBeadSelection Reviewed provisioned inputs; copied once.
---@param options? louiselm.workflow.OperatorBeadOptions Explicit test seams; production defaults are installed tools/UI.
---@return louiselm.workflow.BeadExecutor? controller Nil on invalid selection.
---@return string? error_message Safe refusal; no supplied configuration or command diagnostics.
function M.new(selection, options)
  if options == nil then
    options = {}
  end
  local ids, err = validate(selection, options)
  if not ids then
    return nil, err
  end
  selection = nvim.deepcopy(selection)
  selection.execution = selection.execution or { kind = "duration", duration_ms = 1800000 }
  local disposed, pending = false, nil
  local controller
  local seen, bindings = {}, {}
  local system = options.system or nvim.system
  local function prepare(id, done, head)
    local index
    for i, bead_id in ipairs(ids) do
      if bead_id == id then
        index = i
        break
      end
    end
    if disposed or pending or not index or seen[id] or not commit(head) then
      return false, "operator preparation is disposed, repeated, off-list or missing current HEAD"
    end
    seen[id] = true
    local completed = false
    local function finish(prepared, message)
      if disposed or completed then
        return
      end
      completed = true
      done(prepared, message)
    end
    local function execute(argv, stdin, callback)
      local encoded, bytes = true, nil
      if stdin then
        encoded, bytes = pcall(nvim.json.encode, stdin)
      end
      if not encoded then
        finish(nil, "operator input could not be encoded")
        return
      end
      local used = false
      local ok, job = pcall(system, argv, { text = true, cwd = "/", stdin = bytes }, function(result)
        if used then
          return
        end
        used = true
        nvim.schedule(function()
          pending = nil
          if disposed or completed then
            return
          end
          if type(result) ~= "table" or result.code ~= 0 then
            finish(nil, "operator preparation refused or outcome uncertain; inspect before any new Run")
          else
            callback(result.stdout)
          end
        end)
      end)
      if ok then
        pending = job
      else
        finish(nil, "could not start " .. argv[1] .. " for operator preparation")
      end
    end
    local snapshot = selection.snapshot_parent .. "/" .. selection.envelope.run_id .. "-" .. index
    local function ready(binding)
      if not controller then
        return finish(nil, "Run controller is unavailable")
      end
      local current = nvim.tbl_extend("force", {}, selection, { envelope = controller.options.envelope })
      local worker, verifier = grants(current, id, index, binding)
      finish({
        grant = worker,
        prompt = selection.beads[index].prompt,
        base_commit = head,
        verification = {
          snapshot = snapshot,
          snapshot_digest = binding.source_snapshot_digest,
          plan = selection.plan,
          plan_digest = selection.envelope.verification_plan_digest,
          verifier_grant = verifier,
        },
      })
    end
    local function snapshot_branch_checked(branch)
      if branch ~= "run/" .. selection.envelope.run_id .. "\n" then
        finish(nil, "Run checkout branch changed; review the draft again")
        return
      end
      execute(
        {
          "louiselm-skills",
          "workspace",
          "prepare",
          "--repository",
          selection.worktree.path,
          "--output",
          snapshot,
          "--robot-json",
        },
        nil,
        function(bytes)
          local ok, preview = pcall(nvim.json.decode, bytes)
          if
            not ok
            or type(preview) ~= "table"
            or preview.base_commit ~= head
            or not digest(preview.snapshot_digest)
            or not digest(preview.base_digest)
            or type(preview.changes) ~= "table"
            or not nvim.islist(preview.changes)
          then
            finish(nil, "snapshot did not bind the exact current Run HEAD")
            return
          end
          for _, change in ipairs(preview.changes) do
            if type(change) ~= "table" or change.kind ~= "ignored" or change.included ~= false then
              finish(nil, "Run checkout has unaccepted working-copy changes")
              return
            end
          end
          -- Only this fresh frozen snapshot is shared; never keys, config or journals.
          execute({ "/usr/bin/chgrp", "-R", "--", tostring(selection.input_group), snapshot }, nil, function()
            execute({ "/usr/bin/chmod", "-R", "g+rX,o-rwx", "--", snapshot }, nil, function()
              local prior = bindings[preview.snapshot_digest]
              if prior then
                if prior.base_commit ~= head or prior.source_base_digest ~= preview.base_digest then
                  finish(nil, "previously confirmed snapshot binding changed")
                  return
                end
                ready(prior)
                return
              end
              local manifest = nvim.deepcopy(selection.manifest)
              manifest.source_snapshot_digest, manifest.source_base_digest =
                preview.snapshot_digest, preview.base_digest
              execute({ "louiselm-control", "launch-inputs", "stage", "--json" }, {
                manifest = manifest,
                snapshot = snapshot,
                cache = selection.cache,
                expected_base_commit = head,
              }, function(response)
                local decoded, binding = pcall(nvim.json.decode, response)
                if
                  not decoded
                  or not closed(binding, {
                    "schema",
                    "manifest_digest",
                    "source_snapshot_digest",
                    "source_base_digest",
                    "cache_base_digest",
                    "base_commit",
                  })
                  or binding.schema ~= "louiselm.launch-inputs.staged/1"
                  or not digest(binding.manifest_digest)
                  or binding.source_snapshot_digest ~= preview.snapshot_digest
                  or binding.source_base_digest ~= preview.base_digest
                  or binding.cache_base_digest ~= manifest.cache_base_digest
                  or binding.base_commit ~= head
                then
                  finish(nil, "broker returned an invalid input binding; inspect without retrying")
                  return
                end
                bindings[preview.snapshot_digest] = binding
                ready(binding)
              end)
            end)
          end)
        end
      )
    end
    local function snapshot_parent_checked(metadata)
      if metadata ~= selection.envelope.controller_uid .. ":" .. selection.input_group .. ":750\n" then
        return finish(nil, "snapshot parent must be operator-owned, input-group-owned and mode 0750")
      end
      execute(
        { "/usr/bin/git", "-C", selection.worktree.path, "symbolic-ref", "--quiet", "--short", "HEAD" },
        nil,
        snapshot_branch_checked
      )
    end
    local function prepare_snapshot()
      execute({ "/usr/bin/stat", "--format=%u:%g:%a", "--", selection.snapshot_parent }, nil, snapshot_parent_checked)
    end
    if selection.manifest.isolation.network == "brokered" then
      -- This caller prepares Agent workers, not just network-denied fixed verifiers.
      -- PreCutover always yields Unevaluated, which cannot admit Brokered startup.
      execute(
        { "louiselm-skills", "workspace", "qa-runtime", "inspect-installation", "--robot-json" },
        nil,
        function(bytes)
          local ok, installed = pcall(nvim.json.decode, bytes)
          if
            not ok
            or type(installed) ~= "table"
            or installed.operator_uid ~= selection.envelope.controller_uid
            or not digest(installed.release_id)
            or not digest(installed.policy_digest)
            or not nvim.tbl_contains({ "pre_cutover", "enforced" }, installed.conformance)
          then
            finish(nil, "current installed conformance policy is unavailable or invalid")
          elseif installed.conformance ~= "enforced" then
            finish(
              nil,
              "Brokered workers require enforced conformance and current certification; policy was not changed"
            )
          else
            prepare_snapshot()
          end
        end
      )
    else
      prepare_snapshot()
    end
    return true
  end
  local construction_error
  controller, construction_error = Workflow.new_bead_executor({
    envelope = selection.envelope,
    draft = { schema = "louiselm.broker.run-draft/1", envelope = selection.envelope, execution = selection.execution },
    bead_ids = ids,
    agent_id = selection.manifest.agent.id,
    prepare = prepare,
    worktree = selection.worktree,
    on_worker = function(result, continue)
      continue(result.verification_passed == true)
    end,
    on_promotion = options.on_promotion or require("louiselm.ui.run_promotion").prompt,
    system = system,
    session_api = options.session_api,
    prepared = options.prepared,
  })
  if not controller then
    return nil, construction_error
  end
  local dispose = controller.dispose
  function controller:dispose()
    disposed = true
    local stopped, stop_error = true, nil
    if pending then
      stopped, stop_error = pcall(pending.kill, pending, 15)
      pending = nil
    end
    local released, release_error = dispose(self)
    if not released then
      return false, release_error
    end
    if not stopped then
      return false, "could not cancel snapshot preparation: " .. tostring(stop_error)
    end
    return true
  end
  return controller
end

return M
