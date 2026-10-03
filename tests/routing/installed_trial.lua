-- Explicit disposable-VM gates. The focused startup case deliberately uses
-- the ACP peer without route options: both verifiers initialize, but no Model
-- prompt runs. Full paired qualification uses explicit synthetic route options.
---@diagnostic disable-next-line: undefined-global -- Standalone Neovim fixture.
local nvim = vim
nvim.opt.rtp:prepend(nvim.fn.getcwd())
local selection = nvim.json.decode(table.concat(nvim.fn.readfile(nvim.env.LOUISELM_QUALIFICATION_FIXTURE), "\n"))
local start_only = nvim.env.LOUISELM_QUALIFICATION_START_ONLY == "1"
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
    return nvim.system(argv, options, function(reply)
      if reply.code ~= 0 then
        nvim.schedule(function()
          print("fixture Control refusal: " .. reply.stderr)
        end)
      end
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
