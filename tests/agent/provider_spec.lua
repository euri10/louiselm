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

T["resolves literal prefixes on any named option without owning caller tables"] = function()
  local input = { option = "access", prefixes = { ["direct./"] = "Direct", ["reseller/"] = "Reseller" } }
  local mapping = assert(Provider.normalize(input))
  input.option = "model"
  input.prefixes["direct./"] = "Changed"
  local active = { access = "direct./new-model", model = "irrelevant", enabled = false }
  MiniTest.expect.equality(Provider.resolve(mapping, active), "Direct")
  MiniTest.expect.equality(Provider.resolve(mapping, { access = "reseller/new-model" }), "Reseller")
  MiniTest.expect.equality(active, { access = "direct./new-model", model = "irrelevant", enabled = false })
  for _, value in ipairs({ "directX/model", "x-direct./model", "DIRECT./model", "unknown/model", false, 12 }) do
    local provider, err = Provider.resolve(mapping, { access = value })
    MiniTest.expect.equality(provider, nil)
    MiniTest.expect.equality(type(err), "string")
  end
  MiniTest.expect.equality(Provider.resolve(mapping, {}), nil)
end

T["rejects overlapping prefixes even for the same service instead of choosing longest"] = function()
  for _, service in ipairs({ "Direct", "Other" }) do
    local mapping = assert(Provider.normalize({
      option = "access",
      prefixes = { ["direct/"] = "Direct", ["direct/team/"] = service },
    }))
    local provider, err = Provider.resolve(mapping, { access = "direct/team/model" })
    MiniTest.expect.equality(provider, nil)
    MiniTest.expect.equality(assert(err):find("ambiguous", 1, true) ~= nil, true)
    MiniTest.expect.equality(Provider.resolve(mapping, { access = "direct/solo" }), "Direct")
  end
end

T["validates prefix maps through setup and headless configuration"] = function()
  local Schema = require("louiselm.schema")
  local Config = require("louiselm.agent.config")
  local provider = { option = "access", prefixes = { ["direct/"] = "Direct" } }
  local agents = { renamed = { command = "agent", provider = provider } }
  MiniTest.expect.equality(Schema.validate(require("louiselm.config").schema, { agents = agents }), {})
  local normalized = assert(Config.normalize(agents))
  MiniTest.expect.equality(Provider.resolve(normalized.renamed.provider, { access = "direct/new" }), "Direct")
  for _, invalid in ipairs({
    { prefixes = { ["direct/"] = "Direct" } },
    { option = "", prefixes = { ["direct/"] = "Direct" } },
    { option = "  ", prefixes = { ["direct/"] = "Direct" } },
    { option = false, prefixes = { ["direct/"] = "Direct" } },
    { option = "access" },
    { option = "access", prefixes = {} },
    { option = "access", prefixes = "direct/" },
    { option = "access", prefixes = { [""] = "Direct" } },
    { option = "access", prefixes = { [1] = "Direct" } },
    { option = "access", prefixes = { ["direct/"] = "  " } },
    { option = "access", prefixes = { ["direct/"] = false } },
    { option = "access", prefixes = { ["direct/"] = "Direct" }, extra = true },
  }) do
    local result, errors = Provider.normalize(invalid)
    MiniTest.expect.equality(result, nil)
    MiniTest.expect.equality(#errors > 0, true)
  end
end

return T
