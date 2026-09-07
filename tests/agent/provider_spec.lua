local MiniTest = require("mini.test")
local Provider = require("louiselm.agent.provider")

local T = MiniTest.new_set()

T["resolves candidate routes against the remaining typed tuple without guessing"] = function()
  local routes = assert(Provider.normalize({
    { provider = "direct", options = { model = "shared-model", route = "direct", enabled = false } },
    { provider = "reseller", options = { model = "shared-model", route = "reseller", enabled = false } },
    { provider = "reseller", options = { model = "other-model", route = "direct", enabled = false } },
  }))
  local active = { model = "shared-model", route = "direct", enabled = false, effort = "high" }
  MiniTest.expect.equality(Provider.resolve(routes, active), "direct")
  local candidate = { model = "shared-model", route = "reseller", enabled = false, effort = "high" }
  MiniTest.expect.equality(Provider.resolve(routes, candidate), "reseller")
  candidate.model, candidate.route = "other-model", "direct"
  MiniTest.expect.equality(Provider.resolve(routes, candidate), "reseller")
  MiniTest.expect.equality(active, { model = "shared-model", route = "direct", enabled = false, effort = "high" })
  candidate.enabled = "false"
  MiniTest.expect.equality(Provider.resolve(routes, candidate), nil)
  MiniTest.expect.equality(Provider.resolve(nil, active), nil)
end

T["rejects duplicate matching routes even if both name the same service"] = function()
  local routes = assert(Provider.normalize({
    { provider = "service", options = { model = "small" } },
    { provider = "service", options = { enabled = false } },
  }))
  local provider, err = Provider.resolve(routes, { model = "small", enabled = false })
  MiniTest.expect.equality(provider, nil)
  MiniTest.expect.equality(assert(err):find("ambiguous", 1, true) ~= nil, true)
end

return T
