-- Compiled into louiselm-control; standalone headless coordinator, never sourced by a worker.
---@diagnostic disable-next-line: undefined-global -- Neovim standalone entrypoint.
local nvim = vim
local selection = nvim.json.decode(table.concat(nvim.fn.readfile(nvim.env.LOUISELM_REHEARSAL_SELECTION), "\n"))
local check = nvim.env.LOUISELM_REHEARSAL_CHECK == "1"
local done, controller, api = false, nil, nil
---@type {id: string, plan: { runs: {envelope: table, authorization: { envelope_digest: string }}[] }}?
local plan
---@type louiselm.workflow.BeadPreparation?
local prepared
local observations, index = {}, 0
local phase, code = "workspace_prepare", "unavailable"

local function output(report)
  if done then
    return
  end
  done = true
  if api then
    local closed = api:dispose()
    if not closed then
      report.status, report.phase, report.code = "recovery_required", "launcher", "cleanup_unproven"
    end
  end
  if controller then
    local closed = controller:dispose()
    if not closed then
      report.status, report.phase, report.code = "failed", "workspace_prepare", "unavailable"
    end
  end
  io.stdout:write(nvim.json.encode(report))
  io.stdout:flush()
  nvim.cmd(report.status == "passed" and "qa!" or "cquit 1")
end

local function refused()
  output({
    schema = "louiselm.run-rehearsal/1",
    status = plan and "recovery_required" or "refused",
    phase = phase,
    code = code,
  })
end

-- Register the aggregate guard before loading plugin modules or starting I/O.
nvim.defer_fn(function()
  if not done then
    phase, code = "worker_startup", "timeout"
    refused()
  end
end, 600000)

local function call(request, callback)
  nvim.system({ "louiselm-control", "run", "authorize", "--json" }, {
    text = true,
    cwd = "/",
    stdin = nvim.json.encode(request),
  }, function(result)
    nvim.schedule(function()
      if done then
        return
      end
      local decoded, response = pcall(nvim.json.decode, result.stdout)
      if result.code ~= 0 or not decoded or type(response) ~= "table" then
        refused()
      else
        callback(response)
      end
    end)
  end)
end

local function rehearsal(request, callback)
  call({ kind = "rehearsal", request = request }, function(response)
    if
      response.kind ~= "rehearsal"
      or type(response.report) ~= "table"
      or response.report.schema ~= "louiselm.run-rehearsal/1"
    then
      return refused()
    end
    callback(response.report)
  end)
end

local function finish()
  if not plan then
    return refused()
  end
  phase, code = "receipt", "cleanup_unproven"
  rehearsal({ operation = "finish", id = plan.id, observations = observations }, output)
end

local next_worker
local function settle(session_id, failed)
  if api then
    local closed = api:dispose()
    api = nil
    if not closed then
      observations[index].failure = { phase = "launcher", code = "cleanup_unproven" }
      failed = true
    end
  end
  local deadline = nvim.uv.hrtime() + 45000000000
  local function inspect()
    if not plan then
      return refused()
    end
    phase, code = "receipt", "cleanup_unproven"
    rehearsal({ operation = "inspect", id = plan.id, observations = observations }, function(report)
      local worker = report.workers[index]
      if worker and worker.session_id == session_id and type(worker.terminal_head) == "table" then
        if failed then
          finish()
        else
          next_worker()
        end
      elseif nvim.uv.hrtime() >= deadline then
        finish()
      else
        nvim.defer_fn(inspect, 100)
      end
    end)
  end
  inspect()
end

next_worker = function()
  local current, inputs = plan, prepared
  if not current or not inputs then
    return refused()
  end
  index = index + 1
  if index > #selection.beads then
    return finish()
  end
  local grant = nvim.deepcopy(inputs.grant)
  local trial = current.plan.runs[index]
  if not trial then
    return refused()
  end
  local id = current.id .. "-worker-" .. index
  local request = grant.request
  request.run_id, request.session_id = trial.envelope.run_id, id
  request.request_id, request.authorization_id = id .. "-request", id .. "-authorization"
  grant.expires_at_ms = trial.envelope.expires_at_ms
  grant.provider_requests = nvim.deepcopy(trial.envelope.provider_requests)
  grant.beads_mutations.expires_at_ms = grant.expires_at_ms
  grant.beads_mutations.issue_ids = { selection.beads[index].id }
  local observation = { started = false, attached = false, failure = nvim.NIL }
  observations[index] = observation
  phase, code = "authorization", "refused"
  call(
    { kind = "session", grant = grant, expected_envelope_digest = trial.authorization.envelope_digest },
    function(response)
      if response.kind ~= "session" or response.receipt.session_id ~= id then
        return finish()
      end
      api = require("louiselm.session").new({
        [selection.manifest.agent.id] = {
          command = "/usr/local/lib/louiselm/current/bin/louiselm-launch",
          args = {},
          provider = "openai",
          skills = { policy = "native" },
        },
      })
      if not api then
        observation.failure = { phase = "worker_startup", code = "initialization_failed" }
        return finish()
      end
      local completed = false
      local function started(ready, err)
        if completed or done then
          return
        end
        completed = true
        if not ready then
          observation.failure = require("louiselm.acp.launch").decode_failure(err, request)
            or { phase = "worker_startup", code = "initialization_failed" }
          return settle(id, true)
        end
        observation.started = true
        local state = ready:inspect()
        local service = require("louiselm.workflow.service")
        local admitted = service.admit(
          { id = request.run_id, generated_work_max = 1, park_ttl_ms = 3600000 },
          function(token, _)
            if not token then
              observation.failure = { phase = "capture_attach", code = "unavailable" }
              return settle(id, true)
            end
            local attached = service.attach({
              id = request.run_id,
              session_id = state.agent .. "/" .. state.acp_session_id,
              agent = state.agent,
              acp_session_id = state.acp_session_id,
              cwd = state.working_dir,
              load_session = ready.client ~= nil and ready.client.agent_capabilities.loadSession == true,
            }, function(ok, _)
              observation.attached = ok
              if not ok then
                observation.failure = { phase = "capture_attach", code = "unavailable" }
              end
              settle(id, not ok)
            end)
            if not attached then
              observation.failure = { phase = "capture_attach", code = "unavailable" }
              settle(id, true)
            end
          end
        )
        if not admitted then
          observation.failure = { phase = "capture_attach", code = "unavailable" }
          settle(id, true)
        end
      end
      local session, err = api:create_session(selection.manifest.agent.id, {
        cwd = "/var/lib/louiselm/sessions/" .. id .. "/workspace",
        broker_session_id = id,
        launch_request = request,
        start_timeout_ms = 60000,
        permission_policy = {
          name = "contained-rehearsal",
          evaluate = function()
            return "allow"
          end,
        },
      }, started)
      if not session then
        started(nil, err)
      end
    end
  )
end

local copied = nvim.deepcopy(selection)
copied.snapshot_parent = nvim.env.LOUISELM_REHEARSAL_PARENT
controller = require("louiselm.workflow.operator").new(copied)
if not controller then
  return refused()
end
local function prepare()
  local started = controller.options.prepare(selection.beads[1].id, function(value, _)
    if not value then
      return refused()
    end
    prepared = value
    phase, code = "tracker_prepare", "unavailable"
    rehearsal({
      operation = check and "check" or "begin",
      plugin_digest = nvim.env.LOUISELM_REHEARSAL_PLUGIN_DIGEST,
      selection = selection,
      manifest_id = prepared.grant.request.session_input_manifest_id,
    }, function(report)
      if check then
        return output(report)
      end
      if report.status ~= "ready" or type(report.plan) ~= "table" then
        return refused()
      end
      plan = report
      next_worker()
    end)
  end, selection.worktree.head)
  if not started then
    refused()
  end
end
if check then
  prepare()
else
  rehearsal({ operation = "invalidate", selection = selection }, function()
    prepare()
  end)
end
