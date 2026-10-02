local MiniTest = require("mini.test")
local Session = require("louiselm.session")
local Protocol = require("louiselm.acp.protocol")
local Qualification = require("louiselm.routing.qualification")
---@diagnostic disable-next-line: undefined-global -- Neovim runtime API.
local nvim = vim
local T = MiniTest.new_set()
local key = "io.github.euri10.louiselm.selectedContent"

local function wait_for(predicate)
  assert(nvim.wait(6000, predicate, 10), "reader did not settle")
end

local function fixture(settings)
  settings = settings or {}
  local directory = nvim.fn.tempname()
  local original = nvim.system
  local writes, processes = {}, {}
  local function options(model)
    return {
      {
        id = "model",
        name = "Model",
        category = "model",
        type = "select",
        currentValue = model,
        options = { { value = "large", name = "Large" }, { value = "small", name = "Small" } },
      },
    }
  end
  rawset(nvim, "system", function(argv, opts, on_exit)
    if argv[1] ~= "reader-test-agent" then
      return original(argv, opts, on_exit)
    end
    local process = { closed = false }
    processes[#processes + 1] = process
    local function send(frame)
      local timer = assert(nvim.uv.new_timer())
      timer:start(0, 0, function()
        timer:close()
        if not process.closed or settings.late then
          opts.stdout(nil, assert(Protocol.encode(frame)) .. "\n")
        end
        process.deliveries = (process.deliveries or 0) + 1
      end)
    end
    process.send = send
    return {
      write = function(_, bytes)
        if bytes == nil then
          return
        end
        local request = assert(Protocol.decode(bytes:sub(1, -2)))
        writes[#writes + 1] = request
        if request.method == "initialize" then
          send(Protocol.response(request.id, {
            protocolVersion = 1,
            agentCapabilities = {
              _meta = settings.unsupported and {} or { [key] = { version = 1 } },
            },
          }))
        elseif request.method == "session/new" then
          send(Protocol.response(request.id, {
            sessionId = "worker-" .. #processes,
            configOptions = options("large"),
            _meta = { [key] = settings.bad_ack and {} or request.params._meta[key] },
          }))
        elseif request.method == "session/set_config_option" then
          if settings.pause_option then
            return
          end
          send(
            Protocol.response(
              request.id,
              { configOptions = options(settings.unconfirmed and "large" or request.params.value) }
            )
          )
        elseif request.method == "session/prompt" and not settings.pending then
          local response = settings.output
            or {
              answer = "Idle",
              references = { { source_id = "a", first_line = 10, last_line = 10 } },
              missing_context = {},
            }
          send(Protocol.notification("session/update", {
            sessionId = request.params.sessionId,
            update = {
              sessionUpdate = "agent_message_chunk",
              content = { type = "text", text = nvim.json.encode(response) },
            },
          }))
          send(Protocol.response(request.id, { stopReason = "end_turn" }))
        end
      end,
      kill = function()
        process.closed = true
        if settings.kill_error then
          error("fixture cleanup failed")
        end
      end,
      is_closing = function()
        return process.closed
      end,
    }
  end)
  MiniTest.finally(function()
    rawset(nvim, "system", original)
  end)
  local route = function(model)
    return {
      agent = "agent",
      provider = "OpenAI",
      model = model,
      model_option_id = "model",
      options = { model = model },
    }
  end
  local scope = {
    workload = { kind = "reader", id = "status" },
    baseline = route("large"),
    candidate = route("small"),
    policy_revision = "reader-1",
  }
  local approval = assert(Qualification.new(directory .. "/qualification.json"))
  if not settings.unqualified then
    local done
    approval:decide({
      action = "approve",
      report = {
        version = 1,
        id = "reader-report",
        workload = scope.workload,
        baseline = scope.baseline,
        candidate = scope.candidate,
        policy_revision = scope.policy_revision,
        fixtures = {
          {
            id = "fixture",
            source = "selected",
            digest = "sha256:" .. string.rep("a", 64),
            checks = { { id = "answer", baseline = "pass", candidate = "pass" } },
            human_review_required = false,
          },
        },
      },
    }, function(result, err)
      assert(result, err)
      done = true
    end)
    wait_for(function()
      return done
    end)
  end
  local api = assert(
    Session.new(
      { agent = { command = "reader-test-agent", provider = "OpenAI", auto = { model = "large" } } },
      nil,
      { usage_directory = directory, qualification_path = directory .. "/qualification.json" }
    )
  )
  MiniTest.finally(function()
    assert(api:dispose())
    local done
    api:flush_recording(function(err)
      assert(not err, err and err.message)
      done = true
    end)
    wait_for(function()
      return done
    end)
    nvim.fn.delete(directory, "rf")
  end)
  local job = {
    scope = scope,
    parent_turn_id = "parent-never-dispatched",
    parent_allowance_ms = 30000,
    question = "What is the status?",
    sources = {
      { id = "a", path = "status.txt", first_line = 10, lines = { "Idle" }, provenance = "selected snapshot" },
    },
    limits = { version = 1, input_bytes = 4096, output_bytes = 1024, max_tokens = 256, timeout_ms = 30000 },
  }
  local result, failure, completed = nil, nil, 0
  return api,
    job,
    function(value, err)
      result, failure, completed = value, err, completed + 1
    end,
    function()
      return completed, result, failure
    end,
    writes,
    processes,
    directory,
    approval
end

T["approved selected content gets a separate bounded worker with helper attribution"] = function()
  local api, job, callback, outcome, writes, processes, directory = fixture()
  local worker = assert(api:read_selected_content(job, callback))
  wait_for(function()
    return outcome() > 0
  end)
  local completed, result, err = outcome()
  MiniTest.expect.equality(completed, 1)
  MiniTest.expect.equality(err, nil)
  assert(result)
  MiniTest.expect.equality(result.answer, "Idle")
  MiniTest.expect.equality(result.usage, nil)
  MiniTest.expect.equality(result.references[1].path, "status.txt")
  MiniTest.expect.equality(result.references[1].provenance, "selected snapshot")
  MiniTest.expect.equality(processes[1].closed, true)
  MiniTest.expect.equality(api:list_sessions(), {})
  MiniTest.expect.equality(worker:dispose(), true)
  local prompts = nvim.tbl_filter(function(request)
    return request.method == "session/prompt"
  end, writes)
  MiniTest.expect.equality(#prompts, 1)
  local done
  api:flush_recording(function(err)
    assert(not err)
    done = true
  end)
  wait_for(function()
    return done
  end)
  local record = nvim
    .system(
      { "sqlite3", "-json", directory .. "/turns.sqlite3", "SELECT data FROM admission_events WHERE phase='decision'" },
      { text = true }
    )
    :wait()
  assert(record.code == 0, record.stderr)
  local data = nvim.json.decode(nvim.json.decode(record.stdout)[1].data)
  MiniTest.expect.equality(data.origin, "helper")
  MiniTest.expect.equality(data.parent_turn_id, "parent-never-dispatched")
end

for _, row in ipairs({
  { "unqualified", { unqualified = true }, "unqualified" },
  { "unsupported", { unsupported = true }, "unsupported_runtime" },
  { "unacknowledged", { bad_ack = true }, "unsupported_runtime" },
  { "unconfirmed", { unconfirmed = true }, "admission_changed" },
  {
    "forged source",
    {
      output = {
        answer = "x",
        references = { { source_id = "b", first_line = 10, last_line = 10 } },
        missing_context = {},
      },
    },
    "invalid_answer",
  },
  {
    "forged range",
    {
      output = {
        answer = "x",
        references = { { source_id = "a", first_line = 9, last_line = 10 } },
        missing_context = {},
      },
    },
    "invalid_answer",
  },
  {
    "raw diagnostic",
    { output = { answer = "x", references = {}, missing_context = {}, secret = "not accepted" } },
    "invalid_answer",
  },
  {
    "overflow",
    { output = { answer = string.rep("x", 1025), references = {}, missing_context = {} } },
    "output_limit",
  },
}) do
  local name, settings, code = row[1], row[2], row[3]
  T["refuses " .. name .. " without retries or residual Sessions"] = function()
    local api, job, callback, outcome, writes, processes = fixture(settings)
    assert(api:read_selected_content(job, callback))
    wait_for(function()
      return outcome() > 0
    end)
    local completed, result, err = outcome()
    MiniTest.expect.equality(completed, 1)
    MiniTest.expect.equality(result, nil)
    assert(err)
    MiniTest.expect.equality(err.code, code)
    MiniTest.expect.equality(api:list_sessions(), {})
    for _, process in ipairs(processes) do
      MiniTest.expect.equality(process.closed, true)
    end
    local prompts = nvim.tbl_filter(function(request)
      return request.method == "session/prompt"
    end, writes)
    MiniTest.expect.equality(#prompts, (code == "invalid_answer" or code == "output_limit") and 1 or 0)
  end
end

T["rejects input and parent allowance before creating a Session"] = function()
  local api, job, callback, _, writes = fixture()
  job.limits.input_bytes = 1
  local worker, err = api:read_selected_content(job, callback)
  MiniTest.expect.equality(worker, nil)
  assert(err)
  MiniTest.expect.equality(err.code, "invalid_job")
  MiniTest.expect.equality(writes, {})
  job.limits.input_bytes = 4096
  job.parent_allowance_ms = 1
  worker, err = api:read_selected_content(job, callback)
  MiniTest.expect.equality(worker, nil)
  MiniTest.expect.equality(writes, {})
end

T["disposal during pending work completes once and releases the process"] = function()
  local api, job, callback, outcome, writes, processes = fixture({ pending = true })
  local worker = assert(api:read_selected_content(job, callback))
  wait_for(function()
    return writes[#writes] and writes[#writes].method == "session/prompt"
  end)
  assert(worker:dispose())
  assert(worker:dispose())
  local completed, result, err = outcome()
  MiniTest.expect.equality(completed, 1)
  MiniTest.expect.equality(result, nil)
  assert(err)
  MiniTest.expect.equality(err.code, "cancelled")
  MiniTest.expect.equality(processes[1].closed, true)
  MiniTest.expect.equality(worker.timer, nil)
  MiniTest.expect.equality(api:list_sessions(), {})
end

T["deadline tears down a pending worker without retry"] = function()
  local api, job, callback, outcome, _, processes = fixture({ pending = true })
  job.limits.timeout_ms = 1000
  local worker = assert(api:read_selected_content(job, callback))
  wait_for(function()
    return outcome() > 0
  end)
  local _, result, err = outcome()
  assert(err)
  MiniTest.expect.equality(result, nil)
  MiniTest.expect.equality(err.code, "deadline")
  MiniTest.expect.equality(processes[1].closed, true)
  MiniTest.expect.equality(worker.timer, nil)
end

T["cleanup uncertainty refuses success and remains visible on repeated disposal"] = function()
  local api, job, callback, outcome = fixture({ kill_error = true })
  local worker = assert(api:read_selected_content(job, callback))
  wait_for(function()
    return outcome() > 0
  end)
  local count, result, err = outcome()
  assert(err)
  MiniTest.expect.equality(count, 1)
  MiniTest.expect.equality(result, nil)
  MiniTest.expect.equality(err.code, "cleanup_failed")
  for _ = 1, 2 do
    local disposed, cleanup_error = worker:dispose()
    MiniTest.expect.equality(disposed, false)
    assert(cleanup_error and cleanup_error:find("fixture cleanup failed", 1, true))
  end
  MiniTest.expect.equality(outcome(), 1)
end

T["owner disposal cancels qualification reads and their deadline immediately"] = function()
  local api, job, callback, outcome, writes = fixture()
  local worker = assert(api:read_selected_content(job, callback))
  assert(api:dispose())
  local count, _, err = outcome()
  assert(err)
  MiniTest.expect.equality(count, 1)
  MiniTest.expect.equality(err.code, "cancelled")
  MiniTest.expect.equality(worker.timer, nil)
  MiniTest.expect.equality(worker.done, true)
  MiniTest.expect.equality(writes, {})
end

T["revoking qualification during option confirmation prevents dispatch"] = function()
  local api, job, callback, outcome, writes, processes, _, approval = fixture({ pause_option = true })
  assert(api:read_selected_content(job, callback))
  wait_for(function()
    return writes[#writes] and writes[#writes].method == "session/set_config_option"
  end)
  local option_request = writes[#writes]
  local selected, done
  approval:lookup(job.scope, nil, function(value)
    selected = value
    done = true
  end)
  wait_for(function()
    return done
  end)
  assert(selected)
  done = false
  approval:decide({ action = "reject", report = selected.report }, function(result, err)
    assert(result, err)
    done = true
  end)
  wait_for(function()
    return done
  end)
  processes[1].send(Protocol.response(option_request.id, {
    configOptions = {
      {
        id = "model",
        name = "Model",
        category = "model",
        type = "select",
        currentValue = "small",
        options = { { value = "small", name = "Small" } },
      },
    },
  }))
  wait_for(function()
    return outcome() > 0
  end)
  local _, _, err = outcome()
  assert(err)
  MiniTest.expect.equality(err.code, "admission_changed")
  MiniTest.expect.equality(#nvim.tbl_filter(function(request)
    return request.method == "session/prompt"
  end, writes), 0)
end

T["a separate reader leaves the main Model, Auto policy and history untouched"] = function()
  local api, job, callback, outcome = fixture()
  local parent_ready
  local parent = assert(api:create_session("agent", nil, function(ready)
    parent_ready = ready
  end))
  wait_for(function()
    return parent_ready ~= nil
  end)
  local before = parent:inspect()
  assert(api:read_selected_content(job, callback))
  wait_for(function()
    return outcome() > 0
  end)
  local after = parent:inspect()
  MiniTest.expect.equality(after.config_options, before.config_options)
  MiniTest.expect.equality(after.auto_mode, before.auto_mode)
  MiniTest.expect.equality(after.current_turn, before.current_turn)
  MiniTest.expect.equality(after.turn_id, before.turn_id)
  MiniTest.expect.equality(api:list_sessions(), { parent.state.id })
end

T["late output after cancellation cannot produce a second result"] = function()
  local api, job, callback, outcome, writes, processes = fixture({ pending = true, late = true })
  local worker = assert(api:read_selected_content(job, callback))
  wait_for(function()
    return writes[#writes] and writes[#writes].method == "session/prompt"
  end)
  local prompt = writes[#writes]
  assert(worker:dispose())
  local delivered = processes[1].deliveries
  processes[1].send(Protocol.response(prompt.id, { stopReason = "end_turn" }))
  wait_for(function()
    return processes[1].deliveries > delivered
  end)
  MiniTest.expect.equality(outcome(), 1)
end

return T
