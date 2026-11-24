---Explicit saved-result refresh in a separately bounded, unpaid fixed verifier.
local Session = require("louiselm.session")
local Launch = require("louiselm.acp.launch")
---@diagnostic disable-next-line: undefined-global -- Neovim runtime API.
local nvim = vim
local M = {}

---@class louiselm.workflow.SavedRefreshOptions
---@field selection table Rust RefreshSelection: exact saved revision, new frozen baseline and current plan.
---@field envelope table Explicit fresh RunEnvelope: one Session, current plan, finite expiry, no paid or mutation capabilities.
---@field manifest table Current resolved SessionInputManifest for that new baseline and envelope.
---@field cache string Explicit immutable cache path for current launch input staging.
---@field session_id string Fresh fixed-verifier Session identity.
---@field system? fun(command: string[], options: table, callback: fun(result: table)): unknown Async subprocess seam.
---@field session_api? louiselm.session.Api Caller-owned Session seam.

---Refresh a saved decision without accepting, restarting a worker or extending its Run.
---Requires explicit fresh bounded authority and measured launch inputs. Callback and
---cancellation settle once on the main loop. Cancellation preserves pending work;
---a lost reply is inspected, never retried. The broker owns immutable revisions.
---@param options louiselm.workflow.SavedRefreshOptions
---@param callback fun(result: table?, error_message?: string) New saved observation, not automatic acceptance.
---@return fun()? cancel Idempotent; disposes only this call's verifier.
---@return string? error_message Immediate invalid-input refusal.
function M.refresh(options, callback)
  if type(options) ~= "table" or type(callback) ~= "function" then
    return nil, "refresh requires explicit inputs and a callback"
  end
  for key in pairs(options) do
    if
      not nvim.tbl_contains(
        { "selection", "envelope", "manifest", "cache", "session_id", "system", "session_api" },
        key
      )
    then
      return nil, "unknown refresh option"
    end
  end
  local envelope, selection, manifest = options.envelope, options.selection, options.manifest
  local function absent(value)
    return value == nil or value == nvim.NIL
  end
  if
    type(envelope) ~= "table"
    or type(selection) ~= "table"
    or type(manifest) ~= "table"
    or type(manifest.agent) ~= "table"
    or type(manifest.envelope) ~= "table"
    or type(options.session_id) ~= "string"
    or not options.session_id:match("^[%w_%-]+$")
    or type(options.cache) ~= "string"
    or options.cache:sub(1, 1) ~= "/"
    or envelope.max_sessions ~= 1
    or envelope.verification_plan_digest ~= selection.plan_digest
    or manifest.envelope.id ~= envelope.envelope_id
    or manifest.envelope.revision ~= envelope.envelope_revision
    or manifest.source_snapshot_digest ~= selection.snapshot_digest
    or not absent(envelope.provider_requests)
    or not absent(envelope.bead_scope)
    or not absent(envelope.commands)
    or not absent(envelope.skill_requests)
  then
    return nil, "refresh requires one fresh fixed verifier with no paid or mutation capabilities"
  end
  options = {
    selection = nvim.deepcopy(selection),
    envelope = nvim.deepcopy(envelope),
    manifest = nvim.deepcopy(manifest),
    cache = options.cache,
    session_id = options.session_id,
    system = options.system,
    session_api = options.session_api,
  }
  envelope, selection, manifest = options.envelope, options.selection, options.manifest
  local finished, pending, session, owned_api = false, nil, nil, nil
  local function finish(result, err)
    if finished then
      return
    end
    finished = true
    if session then
      local disposed, dispose_error = session:dispose()
      if not disposed then
        result, err = nil, dispose_error or "refresh verifier disposal unconfirmed"
      end
    end
    if owned_api then
      local disposed, dispose_error = owned_api:dispose()
      if not disposed then
        result, err = nil, dispose_error or "refresh Session API disposal unconfirmed"
      end
    end
    callback(result, err)
  end
  local function control(argv, request, next_step)
    local phase = argv[2] == "verification" and request.kind or argv[2] .. "/" .. argv[3]
    local encoded, bytes = pcall(nvim.json.encode, request)
    if not encoded then
      return finish(nil, "invalid refresh request")
    end
    local ok, process = pcall(
      options.system or nvim.system,
      argv,
      { stdin = bytes, text = true, cwd = "/" },
      function(output)
        nvim.schedule(function()
          pending = nil
          if finished then
            return
          end
          if type(output) ~= "table" or output.code ~= 0 then
            return finish(
              nil,
              "refresh " .. phase .. " refused or outcome uncertain; inspect saved result before another attempt"
            )
          end
          local decoded, value = pcall(nvim.json.decode, output.stdout)
          if not decoded or type(value) ~= "table" then
            return finish(nil, "invalid refresh response")
          end
          next_step(value)
        end)
      end
    )
    if not ok then
      finish(nil, "could not start refresh control")
    else
      pending = process
    end
  end
  local verify = { "louiselm-control", "verification", "--json" }
  local authorize = { "louiselm-control", "run", "authorize", "--json" }
  control(verify, { kind = "saved_result", result_id = selection.result_id, discard = false }, function(saved)
    if
      saved.kind ~= "saved_result"
      or type(saved.result) ~= "table"
      or type(saved.result.selection) ~= "table"
      or envelope.run_id == saved.result.selection.run_id
    then
      return finish(nil, "refresh requires a new separately approved Run")
    end
    control(verify, { kind = "stage_refresh", selection = selection }, function(prepared)
      if
        prepared.kind ~= "refresh_prepared"
        or type(prepared.input) ~= "table"
        or type(prepared.input.job) ~= "table"
      then
        return finish(nil, "invalid refresh preparation")
      end
      control({ "louiselm-control", "launch-inputs", "stage", "--json" }, {
        manifest = manifest,
        snapshot = selection.snapshot,
        cache = options.cache,
        expected_base_commit = selection.expected_head,
      }, function(binding)
        if type(binding.manifest_digest) ~= "string" then
          return finish(nil, "refresh launch inputs unavailable")
        end
        local id = options.session_id
        local launch = {
          schema = "louiselm.launch.request/3",
          protocol_version = 1,
          request_id = id .. "-request",
          authorization_id = id .. "-authorization",
          session_id = id,
          run_id = envelope.run_id,
          agent_id = manifest.agent.id,
          envelope_id = envelope.envelope_id,
          envelope_revision = envelope.envelope_revision,
          skills = manifest.skills,
          session_input_manifest_id = binding.manifest_digest,
        }
        local grant = {
          role = "fixed_verifier",
          request = launch,
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
        control(authorize, { kind = "run", envelope = envelope }, function(approval)
          if approval.kind ~= "run" or type(approval.receipt) ~= "table" then
            return finish(nil, "refresh authority refused")
          end
          control(
            authorize,
            { kind = "session", grant = grant, expected_envelope_digest = approval.receipt.envelope_digest },
            function(approved)
              local bytes = Launch.encode(launch)
              if
                approved.kind ~= "session"
                or type(approved.receipt) ~= "table"
                or not bytes
                or approved.receipt.request_digest ~= "sha256:" .. nvim.fn.sha256(bytes)
              then
                return finish(nil, "refresh verifier authorization mismatch")
              end
              local api = options.session_api
              if not api then
                local errors
                api, errors = Session.new({
                  [manifest.agent.id] = {
                    command = "/usr/local/lib/louiselm/current/bin/louiselm-launch",
                    args = {},
                    provider = "openai",
                    skills = { policy = "native" },
                  },
                })
                if not api then
                  return finish(nil, "refresh Session API unavailable: " .. nvim.inspect(errors))
                end
                owned_api = api
              end
              local created, err = api:create_session(manifest.agent.id, {
                cwd = "/var/lib/louiselm/sessions/" .. id .. "/workspace",
                broker_session_id = id,
                launch_request = launch,
                start_timeout_ms = 30000,
                permission_policy = {
                  name = "contained-verifier",
                  evaluate = function()
                    return "deny"
                  end,
                },
              }, function(ready, ready_error)
                nvim.schedule(function()
                  -- The returned handle is owned before readiness; cancellation already disposes it.
                  if finished then
                    return
                  end
                  if not ready then
                    return finish(nil, ready_error or "refresh verifier startup failed")
                  end
                  session = ready
                  control({ "louiselm-control", "session", "inspect", id, "--json" }, nvim.NIL, function(status)
                    if
                      status.state ~= "running"
                      or type(status.broker_head) ~= "table"
                      or status.broker_head.sequence ~= 1
                    then
                      return finish(nil, "refresh verifier has no fresh Running receipt")
                    end
                    control(verify, {
                      kind = "run",
                      request = {
                        schema = "louiselm.launch.verification/1",
                        protocol_version = 1,
                        request_id = selection.input_id .. "-verify",
                        launch = launch,
                        head = status.broker_head,
                        expires_at_ms = envelope.expires_at_ms,
                        operation = {
                          kind = "refresh",
                          input_id = selection.input_id,
                          input_digest = prepared.input_digest,
                          job_digest = prepared.input.job.job_digest,
                        },
                      },
                    }, function(executed)
                      if executed.kind ~= "executed" or executed.commands_passed ~= true then
                        return finish(nil, "refresh verification did not pass; original remains pending")
                      end
                      control(
                        verify,
                        { kind = "saved_result", result_id = selection.result_id, discard = false },
                        function(result)
                          if
                            result.kind ~= "saved_result"
                            or type(result.result) ~= "table"
                            or result.result.request.job.job_digest ~= prepared.input.job.job_digest
                          then
                            return finish(nil, "refresh publication unconfirmed; inspect saved result")
                          end
                          finish(result.result)
                        end
                      )
                    end)
                  end)
                end)
              end)
              if not created then
                finish(nil, err or "refresh verifier creation refused")
              else
                session = created
              end
            end
          )
        end)
      end)
    end)
  end)
  return function()
    if finished then
      return
    end
    if pending and type(pending.kill) == "function" then
      pending:kill(15)
    end
    finish(nil, "refresh cancelled; saved work remains pending")
  end
end

return M
