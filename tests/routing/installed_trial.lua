-- Explicit disposable-VM gates. The focused startup case deliberately uses
-- the ACP peer without route options: both verifiers initialize, but no Model
-- prompt runs. Full paired qualification uses explicit synthetic route options.
---@diagnostic disable-next-line: undefined-global -- Standalone Neovim fixture.
local nvim = vim
nvim.opt.rtp:prepend(nvim.fn.getcwd())
local selection = nvim.json.decode(table.concat(nvim.fn.readfile(nvim.env.LOUISELM_QUALIFICATION_FIXTURE), "\n"))
local start_only = nvim.env.LOUISELM_QUALIFICATION_START_ONLY == "1"
local expect_unknown = nvim.env.LOUISELM_QUALIFICATION_EXPECT_UNKNOWN == "1"
-- Decode diagnostic metadata only after failure, outside the live callback path.
-- Never print request/output bytes or Agent stderr.
local control_trace = {}
-- Use the existing ACP metadata extension, not forbidden registered arguments
-- or environment. This selects synthetic peer behavior, not launch authority.
local api = assert(require("louiselm.session").new({
  agent = {
    command = "/usr/local/lib/louiselm/current/bin/louiselm-launch",
    args = {},
    provider = "openai",
    skills = { policy = "native" },
    options = { _meta = { louiselmFixture = { qualification = not start_only } } },
  },
}))
local controller = assert(require("louiselm.routing.trial").new(selection, {
  session_api = api,
  system = function(argv, options, done)
    local observation = { argv = argv, stdin = options.stdin }
    control_trace[#control_trace + 1] = observation
    return nvim.system(argv, options, function(reply)
      observation.reply = reply
      done(reply)
    end)
  end,
}))
local completed, failure, result
assert(controller:start(function(value, err)
  local disposed, disposal_error = controller:dispose()
  local api_disposed, api_error = api:dispose()
  failure = err or (not disposed and disposal_error or nil) or (not api_disposed and api_error or nil)
  result = value
  completed = true
end))
assert(
  nvim.wait(115000, function()
    return completed == true
  end, 10),
  "installed paired trial timed out"
)
if failure then
  local verification_requests = {}
  print("fixture Control metadata:")
  for _, observation in ipairs(control_trace) do
    local argv, reply = observation.argv, observation.reply or {}
    local request = observation.stdin and nvim.json.decode(observation.stdin) or {}
    local launch = request.request and request.request.launch
    local subject = launch and launch.session_id or request.session_id or argv[4]
    if argv[2] == "verification" and request.kind == "run" then
      verification_requests[subject] = request.request
    end
    local decoded, value = pcall(nvim.json.decode, reply.stdout or "")
    value = decoded and type(value) == "table" and value or {}
    local status = type(value.status) == "table" and value.status or {}
    local record = type(status.detail) == "table" and status.detail or value.record
    local execution = type(record) == "table" and record.execution or {}
    local metadata = {
      operation = argv[2],
      request_kind = request.kind,
      code = reply.code,
      signal = reply.signal,
      decoded = decoded,
      response_kind = value.kind,
      state = value.state or status.state,
      head_sequence = type(value.broker_head) == "table" and value.broker_head.sequence or nil,
      cleanup_proven = execution.cleanup_proven,
      interrupted = execution.interrupted,
      request_matches = execution.request and nvim.deep_equal(execution.request, verification_requests[subject]),
    }
    for _, suffix in ipairs({ "baseline-worker", "baseline-verifier", "candidate-worker", "candidate-verifier" }) do
      if subject == selection.envelope.run_id .. "-" .. suffix then
        metadata.subject = suffix
      end
    end
    print(nvim.json.encode(metadata))
  end
end
if expect_unknown then
  assert(failure == "verification outcome uncertain; inspect the original request", failure)
  local unknown, runs = false, 0
  for _, observation in ipairs(control_trace) do
    if observation.argv[2] == "verification" and observation.reply then
      local decoded, value = pcall(nvim.json.decode, observation.reply.stdout or "")
      if decoded and type(value) == "table" and value.kind == "status" and type(value.status) == "table" then
        unknown = unknown or value.status.state == "unknown"
      end
      local request = nvim.json.decode(observation.stdin or "{}")
      runs = runs + (request.kind == "run" and 1 or 0)
    end
  end
  assert(unknown, "refused mechanic must leave a durable Unknown, not success or refunded authority")
  assert(runs == 1, "uncertain verification must not be retried")
  print("PAIRED_TRIAL_REFUSAL_RETAINS_UNKNOWN")
  nvim.cmd("cquit 6")
end
assert(not failure, failure)
for _, arm in ipairs({ "baseline", "candidate" }) do
  local observation = assert(result.observations.arms[arm])
  if start_only then
    assert(observation.outcome == "configuration_unconfirmed", arm .. " did not finish guarded ACP startup")
    assert(#observation.turn_ids == 0)
    assert(result.report.fixtures[1].checks[1][arm] == "pending")
    assert(result.report.fixtures[2].checks[1][arm] == "pending")
  else
    assert(observation.outcome == "checked", arm .. " was not verified")
    assert(#observation.turn_ids == 3)
    assert(nvim.deep_equal(observation.confirmed.options, selection.manifest.routes[arm].options))
    assert(result.report.fixtures[1].checks[1][arm] == "pass")
    assert(result.report.fixtures[2].checks[1][arm] == "pass")
  end
  assert(result.report.fixtures[3].checks[1][arm] == "pending")
end
assert(result.observations.provider_requests == nvim.NIL)
assert(result.report.fixtures[3].human == nil)
assert(controller:dispose())
print(start_only and "ACTIVATED_GUARD_STARTED_WITHOUT_MODEL_PROMPTS" or "PAIRED_TRIAL_CHECKED")
nvim.cmd("qa!")
