local MiniTest = require("mini.test")
local Phase = require("louiselm.workflow.phase")
local Routing = require("louiselm.workflow.routing")

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim
local T = MiniTest.new_set()

local function design_phase()
  return assert(Phase.parse("design"))
end

---Two agents declaring the same trait, so tests vary one input at a time.
---@return table
local function request(overrides)
  local value = {
    phase = design_phase(),
    current = { agent = "alpha", model = "sonnet" },
    agents = {
      {
        name = "alpha",
        capabilities = { "reasoning" },
        models = {
          { value = "sonnet", name = "Sonnet" },
          { value = "opus", name = "Opus" },
        },
        context = { size = 100, used = 0 },
      },
      {
        name = "beta",
        capabilities = { "reasoning" },
        models = { { value = "flash", name = "Flash" } },
      },
    },
  }
  for key, override in pairs(overrides or {}) do
    value[key] = override
  end
  return value
end

---@param candidates table[]
---@return string[]
local function keys(candidates)
  local result = {}
  for index, candidate in ipairs(candidates) do
    result[index] = candidate.action .. ":" .. candidate.agent .. "/" .. (candidate.model or "-")
  end
  return result
end

---Locate one candidate by key, failing the test when it is absent.
local function find(candidates, key)
  for _, candidate in ipairs(candidates) do
    if candidate.action .. ":" .. candidate.agent .. "/" .. (candidate.model or "-") == key then
      return candidate
    end
  end
  error("no candidate " .. key .. " among " .. table.concat(keys(candidates), ", "))
end

T["rank"] = MiniTest.new_set()

T["rank"]["distinguishes continue, model change, and handoff"] = function()
  local ranking = assert(Routing.rank(request()))

  MiniTest.expect.equality(keys(ranking.candidates), {
    "continue:alpha/sonnet",
    "model:alpha/opus",
    "handoff:beta/flash",
  })
end

T["rank"]["scores the current choice without a switching penalty"] = function()
  local ranking = assert(Routing.rank(request()))

  MiniTest.expect.equality(find(ranking.candidates, "continue:alpha/sonnet").score, 0.8)
end

T["rank"]["charges a model change less than a handoff"] = function()
  local ranking = assert(Routing.rank(request()))

  MiniTest.expect.equality(find(ranking.candidates, "model:alpha/opus").score, 0.75)
  MiniTest.expect.equality(find(ranking.candidates, "handoff:beta/flash").score, 0.55)
end

T["rank"]["ranks an agent declaring the phase traits above one that does not"] = function()
  local value = request()
  value.agents[2].capabilities = { "speed" }
  local unmatched = find(assert(Routing.rank(value)).candidates, "handoff:beta/flash")

  value.agents[2].capabilities = { "reasoning" }
  local matched = find(assert(Routing.rank(value)).candidates, "handoff:beta/flash")

  MiniTest.expect.equality(unmatched.score < matched.score, true)
end

T["rank"]["weighs a primary phase trait above a secondary-only trait"] = function()
  local value = request({ phase = assert(Phase.parse({ primary = "qa", secondary = { "review" } })) })
  value.agents[1].capabilities = { "coding" }
  value.agents[2].capabilities = { "reasoning" }

  local ranking = assert(Routing.rank(value))

  local primary_only = find(ranking.candidates, "continue:alpha/sonnet")
  local secondary_only = find(ranking.candidates, "handoff:beta/flash")
  MiniTest.expect.equality(secondary_only.score < primary_only.score, true)
end

T["rank"]["is deterministic for fixed inputs"] = function()
  MiniTest.expect.equality(assert(Routing.rank(request())), assert(Routing.rank(request())))
end

T["rank"]["orders identically regardless of the order agents are supplied in"] = function()
  local value = request()
  local reversed = request()
  reversed.agents = { value.agents[2], value.agents[1] }

  MiniTest.expect.equality(
    keys(assert(Routing.rank(reversed)).candidates),
    keys(assert(Routing.rank(value)).candidates)
  )
end

T["rank"]["orders near misses independently of the order agents are supplied in"] = function()
  local value = request({ constraints = { require_traits = { "vision" } } })
  local reversed = request({ constraints = { require_traits = { "vision" } } })
  reversed.agents = { value.agents[2], value.agents[1] }

  MiniTest.expect.equality(keys(assert(Routing.rank(reversed)).rejected), keys(assert(Routing.rank(value)).rejected))
end

T["rank"]["breaks a score tie by agent name"] = function()
  local value = request()
  value.agents[3] = {
    name = "gamma",
    capabilities = { "reasoning" },
    models = { { value = "flash", name = "Flash" } },
  }

  local ranking = assert(Routing.rank(value))

  MiniTest.expect.equality(keys(ranking.candidates), {
    "continue:alpha/sonnet",
    "model:alpha/opus",
    "handoff:beta/flash",
    "handoff:gamma/flash",
  })
end

T["rank"]["does not mutate the caller's request"] = function()
  local value = request()
  local before = nvim.deepcopy(value)

  Routing.rank(value)

  MiniTest.expect.equality(value, before)
end

T["hard filters"] = MiniTest.new_set()

T["hard filters"]["rejects an unavailable agent and reports it as a near miss"] = function()
  local value = request()
  value.agents[2].available = false

  local ranking = assert(Routing.rank(value))

  MiniTest.expect.equality(keys(ranking.candidates), { "continue:alpha/sonnet", "model:alpha/opus" })
  MiniTest.expect.equality(keys(ranking.rejected), { "handoff:beta/flash" })
  MiniTest.expect.equality(find(ranking.rejected, "handoff:beta/flash").reasons, { "agent is unavailable" })
end

T["hard filters"]["rejects a candidate missing a required trait"] = function()
  local value = request({ constraints = { require_traits = { "vision" } } })

  local ranking = assert(Routing.rank(value))

  MiniTest.expect.equality(ranking.candidates, {})
  MiniTest.expect.equality(#ranking.rejected, 3)
  MiniTest.expect.equality(find(ranking.rejected, "continue:alpha/sonnet").reasons, { "does not declare vision" })
end

T["hard filters"]["rejects a candidate without enough context headroom"] = function()
  local value = request()
  value.agents[1].context = { size = 100, used = 80 }

  local ranking = assert(Routing.rank(value))

  MiniTest.expect.equality(keys(ranking.candidates), { "handoff:beta/flash" })
  MiniTest.expect.equality(
    find(ranking.rejected, "continue:alpha/sonnet").reasons,
    { "20% context free, design needs 50%" }
  )
end

T["hard filters"]["rejects every handoff when handoff is not allowed"] = function()
  local value = request({ constraints = { allow_handoff = false } })

  local ranking = assert(Routing.rank(value))

  MiniTest.expect.equality(keys(ranking.candidates), { "continue:alpha/sonnet", "model:alpha/opus" })
  MiniTest.expect.equality(find(ranking.rejected, "handoff:beta/flash").reasons, { "handoff is not allowed" })
end

T["hard filters"]["recommends nothing rather than a candidate that fails a constraint"] = function()
  local value = request({ constraints = { require_traits = { "vision" } } })

  local ranking = assert(Routing.rank(value))

  MiniTest.expect.equality(ranking.candidates, {})
  MiniTest.expect.equality(#ranking.rejected > 0, true)
end

T["hard filters"]["reports every unmet constraint rather than only the first"] = function()
  local value = request({ constraints = { require_traits = { "vision", "audio" } } })
  value.agents[1].available = false

  local ranking = assert(Routing.rank(value))

  MiniTest.expect.equality(find(ranking.rejected, "continue:alpha/sonnet").reasons, {
    "agent is unavailable",
    "does not declare vision",
    "does not declare audio",
  })
end

T["confidence"] = MiniTest.new_set()

T["confidence"]["is complete when traits, evidence, and context are all known"] = function()
  local value = request({
    evidence = { { phase = "design", agent = "alpha", model = "sonnet", samples = 5, reliability = 1 } },
  })

  local ranking = assert(Routing.rank(value))

  MiniTest.expect.equality(find(ranking.candidates, "continue:alpha/sonnet").confidence, 1)
end

T["confidence"]["falls when no evidence backs a candidate"] = function()
  local backed = assert(Routing.rank(request({
    evidence = { { phase = "design", agent = "alpha", model = "sonnet", samples = 5, reliability = 1 } },
  })))
  local unbacked = assert(Routing.rank(request()))

  MiniTest.expect.equality(
    find(unbacked.candidates, "continue:alpha/sonnet").confidence
      < find(backed.candidates, "continue:alpha/sonnet").confidence,
    true
  )
end

T["confidence"]["rises with the number of evidence samples"] = function()
  local function confidence_for(samples)
    local value = request({
      evidence = { { phase = "design", agent = "alpha", model = "sonnet", samples = samples, reliability = 1 } },
    })
    return find(assert(Routing.rank(value)).candidates, "continue:alpha/sonnet").confidence
  end

  MiniTest.expect.equality(confidence_for(1) < confidence_for(5), true)
end

T["confidence"]["falls when the agent declares no capabilities to match"] = function()
  local value = request()
  value.agents[1].capabilities = {}

  local ranking = assert(Routing.rank(value))

  MiniTest.expect.equality(
    find(ranking.candidates, "continue:alpha/sonnet").confidence
      < find(assert(Routing.rank(request())).candidates, "continue:alpha/sonnet").confidence,
    true
  )
end

T["confidence"]["falls when the candidate's context is unknown"] = function()
  local ranking = assert(Routing.rank(request()))

  MiniTest.expect.equality(
    find(ranking.candidates, "handoff:beta/flash").confidence
      < find(ranking.candidates, "continue:alpha/sonnet").confidence,
    true
  )
end

T["confidence"]["inherits the phase's own confidence"] = function()
  local inferred = assert(Routing.rank(request({ phase = assert(Phase.infer("design-review")) })))
  local explicit = assert(Routing.rank(request({ phase = assert(Phase.parse("design")) })))

  MiniTest.expect.equality(
    find(inferred.candidates, "continue:alpha/sonnet").confidence
      < find(explicit.candidates, "continue:alpha/sonnet").confidence,
    true
  )
end

T["evidence"] = MiniTest.new_set()

T["evidence"]["prefers a phase-specific record over a global one"] = function()
  local value = request({
    evidence = {
      { agent = "alpha", model = "sonnet", samples = 5, reliability = 0 },
      { phase = "design", agent = "alpha", model = "sonnet", samples = 5, reliability = 1 },
    },
  })

  local ranking = assert(Routing.rank(value))

  MiniTest.expect.equality(find(ranking.candidates, "continue:alpha/sonnet").score, 1)
end

T["evidence"]["falls back to a global record when the phase has none"] = function()
  local value = request({
    evidence = { { agent = "alpha", model = "sonnet", samples = 5, reliability = 1 } },
  })

  local ranking = assert(Routing.rank(value))

  MiniTest.expect.equality(find(ranking.candidates, "continue:alpha/sonnet").score > 0.8, true)
end

T["evidence"]["trusts a global record less than a phase-specific one"] = function()
  local function confidence_for(phase)
    local value = request({
      evidence = { { phase = phase, agent = "alpha", model = "sonnet", samples = 5, reliability = 1 } },
    })
    return find(assert(Routing.rank(value)).candidates, "continue:alpha/sonnet").confidence
  end

  MiniTest.expect.equality(confidence_for(nil) < confidence_for("design"), true)
end

T["evidence"]["combines reported quality with observed reliability"] = function()
  local value = request({
    evidence = { { phase = "design", agent = "alpha", model = "sonnet", samples = 5, reliability = 1, quality = 0 } },
  })

  local ranking = assert(Routing.rank(value))

  MiniTest.expect.equality(find(ranking.candidates, "continue:alpha/sonnet").score, 0.8)
end

T["reasons"] = MiniTest.new_set()

T["reasons"]["explain the current choice and the traits that matched"] = function()
  local ranking = assert(Routing.rank(request()))

  MiniTest.expect.equality(find(ranking.candidates, "continue:alpha/sonnet").reasons, {
    "current choice",
    "declares reasoning",
    "no design evidence yet",
  })
end

T["reasons"]["name a handoff as a new session and the traits still missing"] = function()
  local value = request()
  value.agents[2].capabilities = {}

  local ranking = assert(Routing.rank(value))

  MiniTest.expect.equality(find(ranking.candidates, "handoff:beta/flash").reasons, {
    "starts a new session with beta",
    "does not declare reasoning",
    "no design evidence yet",
    "context unknown",
  })
end

T["profile"] = MiniTest.new_set()

T["profile"]["describes each canonical phase"] = function()
  for _, phase in ipairs(Phase.canonical()) do
    local profile = assert(Routing.profile(phase))
    MiniTest.expect.equality(type(profile.prefers), "table")
    MiniTest.expect.equality(type(profile.min_headroom), "number")
  end
end

T["profile"]["has no profile for a phase outside the contract"] = function()
  MiniTest.expect.equality(Routing.profile("testing"), nil)
end

T["request"] = MiniTest.new_set()

T["request"]["rejects a request without phase metadata"] = function()
  local value = request()
  value.phase = nil

  local ranking, error_message = Routing.rank(value)

  MiniTest.expect.equality(ranking, nil)
  MiniTest.expect.equality(error_message, "request requires phase metadata")
end

T["request"]["rejects a request whose current agent is not among the agents"] = function()
  local ranking, error_message = Routing.rank(request({ current = { agent = "gamma" } }))

  MiniTest.expect.equality(ranking, nil)
  MiniTest.expect.equality(error_message, "current agent 'gamma' is not among the supplied agents")
end

T["request"]["ranks an agent that advertises no models at all"] = function()
  local value = request({ current = { agent = "alpha" } })
  value.agents[1].models = {}
  value.agents[2].models = {}

  local ranking = assert(Routing.rank(value))

  MiniTest.expect.equality(keys(ranking.candidates), { "continue:alpha/-", "handoff:beta/-" })
end

return T
