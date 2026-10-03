---Explicit paired trials using the installed broker, launcher and fixed verifier.
local Launch = require("louiselm.acp.launch")
local Session = require("louiselm.session")
---@diagnostic disable-next-line: undefined-global -- Neovim runtime API.
local nvim = vim
local M = {}
local Trial = {}
Trial.__index = Trial

---@class louiselm.routing.TrialSelection
---@field schema "louiselm.qualification-run/1"
---@field manifest table Validated curated manifest; the operator entrypoint pins its bytes.
---@field prompts string[] Selected fixture inputs/instructions, in manifest order; memory only.
---@field policy_revision string Applicable comparison policy.
---@field workload louiselm.routing.ComparisonWorkload Explicit comparison scope.
---@field retention_days integer Exactly seven: acknowledges installed workspace retention.
---@field envelope table Complete closed Rust RunEnvelope explicitly approved by start().
---@field input_manifest table Resolved SessionInputManifest for the pinned source/cache.
---@field snapshot string Private immutable snapshot containing only selected fixture bytes.
---@field snapshot_digest string Exact snapshot identity.
---@field base_commit string Snapshot baseline commit.
---@field plan string Fixed, private verification plan covering declared commands in order.
---@field cache string Trusted immutable cache.

---@class louiselm.routing.TrialOptions
---@field system? fun(argv: string[], options: table, callback: fun(result: table)): unknown Asynchronous process seam; defaults to vim.system.
---@field session_api? louiselm.session.Api Explicit asynchronous Session seam for offline tests.

---@class louiselm.routing.TrialResult
---@field report louiselm.routing.ComparisonReport Pending or failed checks prevent approval.
---@field observations table Payload-free local outcomes and measurement coverage; no inferred prices/quota.

---@class louiselm.routing.TrialTimer
---@field start fun(self: louiselm.routing.TrialTimer, timeout: integer, repeat_interval: integer, callback: fun())
---@field stop fun(self: louiselm.routing.TrialTimer)
---@field close fun(self: louiselm.routing.TrialTimer)

---@class louiselm.routing.Trial
---@field selection louiselm.routing.TrialSelection
---@field options louiselm.routing.TrialOptions
---@field status "new"|"active"|"completed"|"disposed"
---@field sessions louiselm.session.Session[] Owned workers and separate verifiers.
---@field processes table[] Pending Control CLI processes owned by this controller.
---@field disposal_error? string Retained local cleanup failure; repeat disposal must not hide it.
---@field owned_api? louiselm.session.Api
---@field api? louiselm.session.Api
---@field timer? louiselm.routing.TrialTimer
---@field callback? fun(result: louiselm.routing.TrialResult, error_message?: string)
---@field arms table<string, table>
---@field arm string
---@field envelope_digest? string
---@field manifest_digest? string
---@field output_bytes integer Observed text bytes, shared by both arms.
---@field started_ns? number
---@field start fun(self: louiselm.routing.Trial, callback: fun(result: louiselm.routing.TrialResult, error_message?: string)): boolean, string?
---@field dispose fun(self: louiselm.routing.Trial): boolean, string?

local function digest(value)
  return type(value) == "string" and #value == 71 and value:match("^sha256:[0-9a-f]+$") ~= nil
end

local function present(value)
  return value ~= nil and value ~= nvim.NIL
end

local function closed(value, keys)
  if type(value) ~= "table" then
    return false
  end
  for key in pairs(value) do
    if not nvim.tbl_contains(keys, key) then
      return false
    end
  end
  return true
end

local function positive(value)
  return type(value) == "number" and value > 0 and value % 1 == 0 and value < 2147483648
end

local function absolute(value)
  return type(value) == "string" and value:sub(1, 1) == "/" and not value:find("\0", 1, true)
end

local function now_ms()
  local seconds, microseconds = nvim.uv.gettimeofday()
  return seconds * 1000 + math.floor(microseconds / 1000)
end

local function validate(s, options)
  if
    not closed(s, {
      "schema",
      "manifest",
      "prompts",
      "policy_revision",
      "workload",
      "retention_days",
      "envelope",
      "input_manifest",
      "snapshot",
      "snapshot_digest",
      "base_commit",
      "plan",
      "cache",
    })
    or s.schema ~= "louiselm.qualification-run/1"
    or s.retention_days ~= 7
    or not closed(options, { "system", "session_api" })
    or (options.system ~= nil and type(options.system) ~= "function")
    or (
      options.session_api ~= nil
      and (type(options.session_api) ~= "table" or type(options.session_api.create_session) ~= "function")
    )
  then
    return "invalid closed trial selection or options"
  end
  local manifest, envelope, inputs = s.manifest, s.envelope, s.input_manifest
  if
    type(manifest) ~= "table"
    or type(manifest.routes) ~= "table"
    or type(manifest.limits) ~= "table"
    or type(manifest.fixtures) ~= "table"
    or not nvim.islist(manifest.fixtures)
    or #manifest.fixtures == 0
    or type(s.prompts) ~= "table"
    or not nvim.islist(s.prompts)
    or #s.prompts ~= #manifest.fixtures
    or type(envelope) ~= "table"
    or type(envelope.provider_requests) ~= "table"
    or type(inputs) ~= "table"
    or type(inputs.agent) ~= "table"
    or type(inputs.envelope) ~= "table"
    or type(inputs.skill_generation) ~= "table"
    or not digest(inputs.skill_generation.generation_digest)
    or not digest(inputs.cache_base_digest)
    or not digest(inputs.source_base_digest)
    or not digest(s.snapshot_digest)
    or inputs.source_snapshot_digest ~= s.snapshot_digest
    or inputs.envelope.id ~= envelope.envelope_id
    or inputs.envelope.revision ~= envelope.envelope_revision
    or type(s.base_commit) ~= "string"
    or not s.base_commit:match("^[0-9a-f]+$")
    or (#s.base_commit ~= 40 and #s.base_commit ~= 64)
    or not absolute(s.snapshot)
    or not absolute(s.plan)
    or not absolute(s.cache)
    or type(s.policy_revision) ~= "string"
    or s.policy_revision == ""
    or not closed(s.workload, { "kind", "id" })
    or s.workload.kind ~= "main"
    or type(s.workload.id) ~= "string"
    or s.workload.id == ""
  then
    return "trial requires pinned inputs, selected fixture prompts and an explicit main workload"
  end
  for _, key in ipairs({ "model_requests", "elapsed_seconds", "input_bytes", "output_bytes" }) do
    if not positive(manifest.limits[key]) then
      return "trial limits must be positive integers"
    end
  end
  local provider, models = envelope.provider_requests, {}
  for _, arm in ipairs({ "baseline", "candidate" }) do
    local route = manifest.routes[arm]
    if
      type(route) ~= "table"
      or route.mode ~= "direct"
      or route.agent ~= inputs.agent.id
      or route.provider ~= "openai"
      or not closed(route.options, { "model", "reasoning_effort" })
      or type(route.options.model) ~= "string"
      or route.options.model == ""
      or not nvim.tbl_contains({ "none", "minimal", "low", "medium", "high" }, route.options.reasoning_effort)
    then
      return "unsupported: trial requires one installed Codex Agent, OpenAI and explicit model/reasoning_effort pairs"
    end
    if not nvim.tbl_contains(models, route.options.model) then
      models[#models + 1] = route.options.model
    end
  end
  table.sort(models)
  if
    nvim.deep_equal(manifest.routes.baseline, manifest.routes.candidate)
    or provider.provider ~= "openai"
    or not nvim.deep_equal(provider.models, models)
    or provider.max_run_requests ~= manifest.limits.model_requests
    or not nvim.tbl_contains({ "none", "minimal", "low", "medium", "high" }, provider.max_effort)
    or provider.expires_at_ms ~= envelope.expires_at_ms
    or not positive(envelope.envelope_revision)
    or not positive(envelope.controller_uid)
    or envelope.max_sessions ~= 4
    or present(envelope.commands)
    or not digest(envelope.verification_plan_digest)
    or type(envelope.run_id) ~= "string"
    or #envelope.run_id > 36
    or not envelope.run_id:match("^[%w_%-]+$")
    or type(envelope.expires_at_ms) ~= "number"
    or envelope.expires_at_ms <= now_ms()
    or envelope.expires_at_ms > now_ms() + manifest.limits.elapsed_seconds * 1000
  then
    return "trial envelope must bind four fresh Sessions, exact Models, shared request cap and bounded expiry"
  end
  for index, fixture in ipairs(manifest.fixtures) do
    if
      type(s.prompts[index]) ~= "string"
      or s.prompts[index] == ""
      or type(fixture) ~= "table"
      or type(fixture.id) ~= "string"
      or type(fixture.provenance) ~= "table"
      or type(fixture.acceptance) ~= "table"
      or type(fixture.acceptance.reference_checks) ~= "table"
      or type(fixture.acceptance.commands) ~= "table"
    then
      return "trial fixture preparation is incomplete"
    end
  end
end

local function checks(self, arm, fixture, command_index)
  local state, result = self.arms[arm], {}
  local output = state.outputs[fixture.id]
  for index, reference in ipairs(fixture.acceptance.reference_checks) do
    result[#result + 1] = {
      id = "reference-" .. index,
      value = output == nil and "pending"
        or (output:find(reference.answer_contains, 1, true) and output:find(reference.citation, 1, true)) and "pass"
        or "fail",
    }
  end
  for index in ipairs(fixture.acceptance.commands) do
    local step = state.steps and state.steps[command_index + index]
    result[#result + 1] = {
      id = "command-" .. index,
      value = type(step) ~= "table" and "pending"
        or step.state ~= "completed" and "pending"
        or (step.exit_code == 0 and step.timed_out == false) and "pass"
        or "fail",
    }
  end
  if #result == 0 then
    result[1] = { id = "human-review", value = "pending" }
  end
  return result
end

local function result(self)
  local s, fixtures, command_index = self.selection, {}, 0
  for _, fixture in ipairs(s.manifest.fixtures) do
    local baseline = checks(self, "baseline", fixture, command_index)
    local candidate = checks(self, "candidate", fixture, command_index)
    local values = {}
    for index, check in ipairs(baseline) do
      values[index] = { id = check.id, baseline = check.value, candidate = candidate[index].value }
    end
    fixtures[#fixtures + 1] = {
      id = fixture.id,
      source = fixture.provenance.reference,
      digest = s.snapshot_digest,
      checks = values,
      human_review_required = present(fixture.acceptance.human_review),
    }
    command_index = command_index + #fixture.acceptance.commands
  end
  local function route(arm)
    local declared = s.manifest.routes[arm]
    return {
      agent = declared.agent,
      provider = declared.provider,
      model = declared.options.model,
      model_option_id = "model",
      options = nvim.deepcopy(declared.options),
    }
  end
  local arms = {}
  for name, arm in pairs(self.arms) do
    arms[name] = {
      outcome = arm.outcome,
      confirmed = arm.confirmed or nvim.NIL,
      turn_ids = arm.turn_ids,
      session_id = arm.launch and arm.launch.session_id or nvim.NIL,
      verification_request_id = arm.verification_request_id or nvim.NIL,
    }
  end
  return {
    report = {
      version = 1,
      id = s.envelope.run_id,
      policy_revision = s.policy_revision,
      workload = nvim.deepcopy(s.workload),
      baseline = route("baseline"),
      candidate = route("candidate"),
      fixtures = fixtures,
    },
    observations = {
      arms = arms,
      output_text_bytes = self.output_bytes,
      elapsed_ms = self.started_ns and math.floor((nvim.uv.hrtime() - self.started_ns) / 1000000) or 0,
      provider_requests = nvim.NIL,
      api_cost = nvim.NIL,
      quota = nvim.NIL,
      provider_request_cap = s.manifest.limits.model_requests,
      cleanup = "await_terminal_receipts",
    },
  }
end

local function finish(self, err)
  if self.status ~= "active" then
    return
  end
  self.status = "completed"
  if self.timer then
    self.timer:stop()
    self.timer:close()
    self.timer = nil
  end
  local callback = self.callback
  self.callback = nil
  -- Results have no raw outputs. Drop retained text once checks are constructed.
  local value = result(self)
  for _, arm in pairs(self.arms) do
    arm.outputs = {}
  end
  if callback then
    nvim.schedule(function()
      callback(value, err)
    end)
  end
end

local function response(self, callback)
  local used = false
  return function(first, second)
    if used then
      return
    end
    used = true
    nvim.schedule(function()
      if self.status == "active" then
        callback(first, second)
      end
    end)
  end
end

local function control(self, argv, request, kind, callback)
  local bytes
  if request then
    local ok, encoded = pcall(nvim.json.encode, request)
    if not ok then
      callback(nil, "invalid control request")
      return
    end
    bytes = encoded
  end
  local deliver = response(self, function(reply)
    if type(reply) ~= "table" or reply.code ~= 0 then
      callback(nil, "Control broker refused " .. kind)
      return
    end
    local decoded, value = pcall(nvim.json.decode, reply.stdout)
    if not decoded or type(value) ~= "table" or (kind ~= "inspect" and kind ~= "inputs" and value.kind ~= kind) then
      callback(nil, "invalid Control broker " .. kind .. " response")
      return
    end
    callback(value)
  end)
  local operation, used = {}, false
  self.processes[#self.processes + 1] = operation
  local function done(reply)
    if used then
      return
    end
    used = true
    for index, item in ipairs(self.processes) do
      if item == operation then
        table.remove(self.processes, index)
        break
      end
    end
    deliver(reply)
  end
  local ok, job = pcall(self.options.system or nvim.system, argv, { cwd = "/", stdin = bytes, text = true }, done)
  if not ok then
    done({ code = 1, stdout = "" })
  else
    operation.job = job
  end
end

local function grant(self, arm, verifier)
  local s, envelope = self.selection, self.selection.envelope
  local id = envelope.run_id .. "-" .. arm .. (verifier and "-verifier" or "-worker")
  local provider = nvim.NIL
  if not verifier then
    provider = nvim.deepcopy(envelope.provider_requests)
    provider.models = { s.manifest.routes[arm].options.model }
    provider.max_effort = s.manifest.routes[arm].options.reasoning_effort
  end
  return {
    role = verifier and "fixed_verifier" or "agent",
    request = {
      schema = "louiselm.launch.request/2",
      protocol_version = 1,
      request_id = id .. "-request",
      authorization_id = id .. "-approval",
      session_id = id,
      run_id = envelope.run_id,
      agent_id = s.input_manifest.agent.id,
      envelope_id = envelope.envelope_id,
      envelope_revision = envelope.envelope_revision,
      skill_generation_id = s.input_manifest.skill_generation.generation_digest,
      session_input_manifest_id = self.manifest_digest,
    },
    controller_uid = envelope.controller_uid,
    expires_at_ms = envelope.expires_at_ms,
    broker_loss_grace_ms = 5000,
    conformance = { attendance = "unattended", waiver = nvim.NIL },
    require_cold_recovery = false,
    dependencies = nvim.NIL,
    commands = nvim.NIL,
    skill_requests = nvim.NIL,
    beads_mutations = nvim.NIL,
    provider_requests = provider,
  }
end

local function launch(self, approval, verifier, callback)
  local request = approval.request
  control(
    self,
    { "louiselm-control", "run", "authorize", "--json" },
    { kind = "session", grant = approval, expected_envelope_digest = self.envelope_digest },
    "session",
    function(value, err)
      if not value then
        callback(nil, err)
        return
      end
      local receipt, bytes = value.receipt, Launch.encode(request)
      if
        type(receipt) ~= "table"
        or receipt.schema ~= "louiselm.broker.child-authorization/1"
        or receipt.run_id ~= request.run_id
        or receipt.session_id ~= request.session_id
        or receipt.authorization_id ~= request.authorization_id
        or receipt.envelope_digest ~= self.envelope_digest
        or receipt.envelope_revision ~= request.envelope_revision
        or not bytes
        or receipt.request_digest ~= "sha256:" .. nvim.fn.sha256(bytes)
      then
        callback(nil, "mismatched child authorization")
        return
      end
      local failure = "contained trial launch failed; inspect named Session receipts"
      local session, start_error = self.api:create_session(
        request.agent_id,
        {
          cwd = "/var/lib/louiselm/sessions/" .. request.session_id .. "/workspace",
          broker_session_id = request.session_id,
          launch_request = request,
          permission_policy = {
            name = "contained-trial",
            evaluate = function()
              return verifier and "deny" or "allow"
            end,
          },
        },
        response(self, function(ready)
          -- Session errors can include Agent stderr. Evaluation artifacts retain
          -- only this stable refusal; original diagnostics stay with the Session.
          callback(ready, not ready and failure or nil)
        end)
      )
      if session then
        self.sessions[#self.sessions + 1] = session
      else
        callback(nil, failure)
      end
    end
  )
end

local next_arm

local function verify(self, worker, input_id, input_digest)
  local arm, s = self.arms[self.arm], self.selection
  local request = arm.launch
  local function unknown()
    arm.outcome = "verification_unknown"
    finish(self, "verification outcome uncertain; inspect the original request")
  end
  local function verification(payload, kind, callback)
    control(self, { "louiselm-control", "verification", "--json" }, payload, kind, function(value)
      if value then
        callback(value)
      else
        unknown()
      end
    end)
  end
  control(
    self,
    { "louiselm-control", "session", "inspect", request.session_id, "--json" },
    nil,
    "inspect",
    function(status)
      if not status or status.state ~= "running" or type(status.broker_head) ~= "table" then
        unknown()
        return
      end
      verification(
        {
          kind = "park",
          request = {
            schema = "louiselm.launch.lifecycle-request/1",
            protocol_version = 1,
            request_id = request.session_id .. "-park",
            authorization_id = request.session_id .. "-park-approval",
            session_id = request.session_id,
            run_id = request.run_id,
            action = "park",
            expected_state = "running",
            expected_receipt_sequence = status.broker_head.sequence,
            envelope_revision = request.envelope_revision,
          },
        },
        "parked",
        function(parked)
          local export = {
            schema = "louiselm.launch.verification/1",
            protocol_version = 1,
            request_id = request.session_id .. "-export",
            launch = request,
            head = parked.head,
            expires_at_ms = s.envelope.expires_at_ms,
            operation = { kind = "export", input_id = input_id, input_digest = input_digest },
          }
          verification({ kind = "export", request = export }, "exported", function(exported)
            if
              not digest(exported.export_digest)
              or type(exported.evidence) ~= "table"
              or type(exported.evidence.job) ~= "table"
              or not digest(exported.evidence.job.job_digest)
            then
              unknown()
              return
            end
            local verifier_launch = arm.verifier_launch
            control(
              self,
              { "louiselm-control", "session", "inspect", verifier_launch.session_id, "--json" },
              nil,
              "inspect",
              function(view)
                if
                  not view
                  or view.state ~= "running"
                  or type(view.broker_head) ~= "table"
                  or view.broker_head.sequence ~= 1
                then
                  unknown()
                  return
                end
                local run = {
                  schema = "louiselm.launch.verification/1",
                  protocol_version = 1,
                  request_id = verifier_launch.session_id .. "-verify",
                  launch = verifier_launch,
                  head = view.broker_head,
                  expires_at_ms = s.envelope.expires_at_ms,
                  operation = {
                    kind = "run",
                    producer_session_id = request.session_id,
                    export_request_id = export.request_id,
                    export_digest = exported.export_digest,
                    job_digest = exported.evidence.job.job_digest,
                  },
                }
                -- Read durable status after any reply; never resend a spent command intent.
                control(
                  self,
                  { "louiselm-control", "verification", "--json" },
                  { kind = "run", request = run },
                  "executed",
                  function()
                    control(
                      self,
                      { "louiselm-control", "verification", "--json" },
                      { kind = "status", session_id = verifier_launch.session_id },
                      "status",
                      function(report)
                        local state = report and report.status
                        local execution = type(state) == "table"
                          and type(state.detail) == "table"
                          and state.detail.execution
                        if
                          type(state) ~= "table"
                          or state.state ~= "completed"
                          or type(execution) ~= "table"
                          or type(execution.steps) ~= "table"
                          or execution.cleanup_proven ~= true
                          or execution.interrupted ~= false
                          or not nvim.deep_equal(execution.request, run)
                          or type(execution.job) ~= "table"
                          or execution.job.job_digest ~= exported.evidence.job.job_digest
                        then
                          unknown()
                          return
                        end
                        arm.steps = execution.steps
                        arm.verification_request_id = run.request_id
                        arm.outcome = "checked"
                        next_arm(self)
                      end
                    )
                  end
                )
              end
            )
          end)
        end
      )
    end
  )
end

local function configure(self, session, route, done)
  local ids = { "model", "reasoning_effort" }
  local function step(index)
    local state = session:inspect()
    if self.status ~= "active" then
      return
    end
    if index > #ids then
      local values = {}
      for _, option in ipairs(state.config_options or {}) do
        values[option.id] = option.current_value
      end
      if not nvim.deep_equal(values, route.options) then
        done(false)
        return
      end
      self.arms[self.arm].confirmed = { agent = state.agent, provider = "openai", options = values }
      done(true)
      return
    end
    local id, expected = ids[index], route.options[ids[index]]
    local option
    for _, candidate in ipairs(state.config_options or {}) do
      if candidate.id == id then
        option = candidate
      end
    end
    if not option or option.type ~= "select" or option.category ~= (index == 1 and "model" or "thought_level") then
      done(false)
      return
    end
    local advertised = false
    for _, value in ipairs(option.options or {}) do
      if value.value == expected then
        advertised = true
      end
    end
    if not advertised then
      done(false)
      return
    end
    if option.current_value == expected then
      step(index + 1)
      return
    end
    local request, err = session:set_config_option(
      id,
      expected,
      response(self, function(options)
        if options == nil then
          done(false)
          return
        end
        local confirmed
        for _, value in ipairs(options) do
          if value.id == id then
            confirmed = value.current_value
          end
        end
        if confirmed ~= expected then
          done(false)
        else
          step(index + 1)
        end
      end)
    )
    if not request then
      done(false)
    end
  end
  step(1)
end

local function run_arm(self)
  local s, arm = self.selection, self.arms[self.arm]
  local input_id = s.envelope.run_id .. "-" .. self.arm .. "-input"
  control(
    self,
    { "louiselm-control", "verification", "--json" },
    {
      kind = "stage",
      input_id = input_id,
      snapshot = s.snapshot,
      snapshot_digest = s.snapshot_digest,
      expected_base_commit = s.base_commit,
      plan = s.plan,
      plan_digest = s.envelope.verification_plan_digest,
    },
    "staged",
    function(staged, err)
      if not staged or not digest(staged.input_digest) then
        finish(self, err or "invalid verification input binding")
        return
      end
      local approval = grant(self, self.arm, false)
      arm.launch = approval.request
      launch(self, approval, false, function(worker, launch_error)
        if not worker then
          finish(self, launch_error or "contained trial launch failed")
          return
        end
        local route = s.manifest.routes[self.arm]
        configure(self, worker, route, function(confirmed)
          if not confirmed then
            arm.outcome = "configuration_unconfirmed"
            next_arm(self)
            return
          end
          local index, chunks, output_error = 1, {}, nil
          local unsubscribe = worker:on(function(event)
            if self.status ~= "active" or (event.type ~= "chunk" and event.type ~= "thought_chunk") then
              return
            end
            local data = event.data
            local content = type(data) == "table" and data.content
            if type(content) ~= "table" or content.type ~= "text" or type(content.text) ~= "string" then
              return
            end
            self.output_bytes = self.output_bytes + #content.text
            if self.output_bytes > s.manifest.limits.output_bytes then
              output_error = "output_limit"
              arm.outcome = output_error
              local cancelled = worker:cancel()
              if not cancelled then
                finish(self, "could not cancel output-limited trial")
              end
            elseif event.type == "chunk" then
              chunks[#chunks + 1] = content.text
            end
          end)
          local function send()
            local state = worker:inspect()
            local values = {}
            for _, option in ipairs(state.config_options or {}) do
              values[option.id] = option.current_value
            end
            if not nvim.deep_equal(values, route.options) then
              arm.outcome = "configuration_changed"
              unsubscribe()
              next_arm(self)
              return
            end
            local id, prompt_error = worker:prompt(
              s.prompts[index],
              response(self, function(_, error_message)
                if error_message or output_error then
                  arm.outcome = output_error or "worker_failed"
                  unsubscribe()
                  if output_error then
                    finish(self, "trial output cap exceeded")
                  else
                    next_arm(self)
                  end
                  return
                end
                local effective = worker:inspect().turn_identity
                if
                  type(effective) ~= "table"
                  or effective.agent ~= route.agent
                  or effective.provider ~= route.provider
                  or not nvim.deep_equal(effective.options, route.options)
                then
                  arm.outcome = "configuration_changed"
                  unsubscribe()
                  next_arm(self)
                  return
                end
                arm.outputs[s.manifest.fixtures[index].id] = table.concat(chunks)
                chunks = {}
                index = index + 1
                if index > #s.prompts then
                  unsubscribe()
                  verify(self, worker, input_id, staged.input_digest)
                else
                  send()
                end
              end)
            )
            if not id then
              arm.outcome = "not_sent"
              unsubscribe()
              next_arm(self)
            else
              arm.turn_ids[#arm.turn_ids + 1] = id
            end
          end
          send()
        end)
      end)
    end
  )
end

local function preflight_verifiers(self, index)
  local name = ({ "baseline", "candidate" })[index]
  if not name then
    run_arm(self)
    return
  end
  local approval = grant(self, name, true)
  local arm = self.arms[name]
  arm.verifier_launch = approval.request
  launch(self, approval, true, function(verifier)
    if not verifier then
      arm.outcome = "verifier_unsupported"
      finish(
        self,
        "unsupported: configured Agent cannot initialize the fixed verifier offline; inspect named Session receipts"
      )
      return
    end
    preflight_verifiers(self, index + 1)
  end)
end

next_arm = function(self)
  if self.arm == "baseline" then
    self.arm = "candidate"
    run_arm(self)
  else
    finish(self)
  end
end

---Construct an inert owner for an explicitly prepared trial. No processes or approvals occur.
---The trusted operator entrypoint must validate the curated manifest and exact selected snapshot/plan bytes first.
---@param selection louiselm.routing.TrialSelection Pinned, operator-selected inputs and deployment authority.
---@param options? louiselm.routing.TrialOptions Asynchronous test seams; production uses installed components.
---@return louiselm.routing.Trial? controller Nil on invalid or unsupported selection.
---@return string? error_message Safe pre-effect refusal.
function M.new(selection, options)
  options = options or {}
  local err = validate(selection, options)
  if err then
    return nil, err
  end
  local controller = setmetatable({
    selection = nvim.deepcopy(selection),
    options = options,
    status = "new",
    sessions = {},
    processes = {},
    arm = "baseline",
    arms = {
      baseline = { outputs = {}, turn_ids = {}, outcome = "not_started" },
      candidate = { outputs = {}, turn_ids = {}, outcome = "not_started" },
    },
    output_bytes = 0,
  }, Trial)
  return controller, nil
end

---Approve the exact Run, stage its selected inputs, and execute each arm once.
---@param callback fun(result: louiselm.routing.TrialResult, error_message?: string) Called once on the main loop, including cancellation.
---@return boolean started
---@return string? error_message Immediate misuse; asynchronous refusals reach callback.
function Trial:start(callback)
  if self.status ~= "new" or type(callback) ~= "function" then
    return false, "trial already started/disposed or missing callback"
  end
  self.status, self.callback, self.started_ns = "active", callback, nvim.uv.hrtime()
  self.api = self.options.session_api
  if not self.api then
    local id = self.selection.input_manifest.agent.id
    local api, errors = Session.new({
      [id] = {
        command = "/usr/local/lib/louiselm/current/bin/louiselm-launch",
        args = {},
        provider = "openai",
        skills = { policy = "native" },
      },
    })
    if not api then
      finish(self, "could not construct contained trial API")
      return true
    end
    self.api, self.owned_api = api, api
  end
  local timer, timer_error = nvim.uv.new_timer()
  if not timer then
    finish(self, "could not create trial deadline")
    return true
  end
  self.timer = timer
  timer:start(math.max(1, self.selection.envelope.expires_at_ms - now_ms()), 0, function()
    nvim.schedule(function()
      if self.status == "active" then
        self.arms[self.arm].outcome = "timeout"
        finish(self, "trial expired; remote outcomes may be unknown")
        self:dispose()
      end
    end)
  end)
  control(
    self,
    { "louiselm-control", "run", "authorize", "--json" },
    { kind = "run", envelope = self.selection.envelope },
    "run",
    function(value, err)
      local receipt = value and value.receipt
      if
        type(receipt) ~= "table"
        or receipt.schema ~= "louiselm.broker.run-authorization/1"
        or receipt.run_id ~= self.selection.envelope.run_id
        or receipt.envelope_revision ~= self.selection.envelope.envelope_revision
        or not digest(receipt.envelope_digest)
      then
        finish(self, err or "mismatched Run authorization")
        return
      end
      self.envelope_digest = receipt.envelope_digest
      local s = self.selection
      control(
        self,
        { "louiselm-control", "launch-inputs", "stage", "--json" },
        { manifest = s.input_manifest, snapshot = s.snapshot, cache = s.cache, expected_base_commit = s.base_commit },
        "inputs",
        function(binding, stage_error)
          if
            not binding
            or binding.schema ~= "louiselm.launch-inputs.staged/1"
            or not digest(binding.manifest_digest)
            or binding.source_snapshot_digest ~= s.snapshot_digest
            or binding.source_base_digest ~= s.input_manifest.source_base_digest
            or binding.cache_base_digest ~= s.input_manifest.cache_base_digest
            or binding.base_commit ~= s.base_commit
          then
            finish(self, stage_error or "mismatched staged trial inputs")
            return
          end
          self.manifest_digest = binding.manifest_digest
          preflight_verifiers(self, 1)
        end
      )
    end
  )
  return true
end

---Cancel owned work and close controller input; the launcher owns terminal cleanup proof.
---No caller snapshot, operator checkout, tracker or production approval is mutated.
---@return boolean disposed
---@return string? error_message First local cleanup failure; terminal receipts still require inspection.
function Trial:dispose()
  if self.status == "disposed" then
    return self.disposal_error == nil, self.disposal_error
  end
  if self.status == "active" then
    self.arms[self.arm].outcome = "cancelled"
    finish(self, "trial cancelled")
  end
  self.status = "disposed"
  if self.timer then
    self.timer:stop()
    self.timer:close()
    self.timer = nil
  end
  local first_error
  for _, operation in ipairs(self.processes) do
    local job = operation.job
    if type(job) == "table" and type(job.kill) == "function" then
      local ok, err = pcall(job.kill, job, 15)
      if not ok then
        first_error = first_error or "trial control process cleanup failed"
      end
    end
  end
  for _, session in ipairs(self.sessions) do
    local state = session:inspect().status
    if state == "prompting" or state == "running" then
      local ok, err = session:cancel()
      if not ok then
        first_error = first_error or err or "trial cancellation failed"
      end
    end
    local ok, err = session:dispose()
    if not ok then
      first_error = first_error or err or "trial disposal failed"
    end
  end
  self.sessions = {}
  if self.owned_api then
    local ok, err = self.owned_api:dispose()
    if not ok then
      first_error = first_error or err or "trial API disposal failed"
    end
    self.owned_api = nil
  end
  self.disposal_error = first_error
  return first_error == nil, first_error
end

return M
