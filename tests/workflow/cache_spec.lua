local MiniTest = require("mini.test")
local Workflow = require("louiselm.workflow")

local T = MiniTest.new_set()

local function manifest()
  return {
    first = {
      workflow = "reference",
      entry = true,
      ["park-expiry"] = "1h",
      outcomes = { { name = "done", terminal = true } },
    },
  }
end

local function result()
  return { ok = true, rejections = {} }
end

T["cache"] = MiniTest.new_set()

T["cache"]["computes unchanged document and manifest once"] = function()
  local cache = Workflow.new_cache()
  local calls = 0
  local function compute()
    calls = calls + 1
    return result()
  end

  cache:resolve("reference", "document", manifest(), compute)
  cache:resolve("reference", "document", manifest(), compute)

  MiniTest.expect.equality(calls, 1)
  MiniTest.expect.equality(cache.misses, 1)
  MiniTest.expect.equality(cache.hits, 1)
end

T["cache"]["invalidates when an unrelated skill is added"] = function()
  local cache = Workflow.new_cache()
  local calls = 0
  local function compute()
    calls = calls + 1
    return result()
  end
  local original = manifest()
  local expanded = manifest()
  expanded.unrelated = { workflow = "other", outcomes = {} }

  cache:resolve("reference", "document", original, compute)
  cache:resolve("reference", "document", expanded, compute)

  MiniTest.expect.equality(calls, 2)
  MiniTest.expect.equality(cache.misses, 2)
end

T["cache"]["invalidates only the changed document entry"] = function()
  local cache = Workflow.new_cache()
  local calls = 0
  local function compute()
    calls = calls + 1
    return result()
  end

  cache:resolve("reference", "document-a", manifest(), compute)
  cache:resolve("reference", "document-b", manifest(), compute)
  cache:resolve("reference", "document-a", manifest(), compute)

  MiniTest.expect.equality(calls, 2)
  MiniTest.expect.equality(cache.hits, 1)
end

T["cache"]["does not let callers mutate cached results"] = function()
  local cache = Workflow.new_cache()
  local first = cache:resolve("reference", "document", manifest(), result)
  first.rejections[1] = { reason = "caller-mutated", message = "caller-mutated" }
  local second = cache:resolve("reference", "document", manifest(), function()
    error("cache unexpectedly missed")
  end)

  MiniTest.expect.equality(second.rejections, {})
end

T["cache"]["is exposed through cached Validation"] = function()
  local cache = Workflow.new_cache()
  local result_value = Workflow.validate_cached(cache, "reference", "document", manifest())

  MiniTest.expect.equality(result_value.ok, true)
  MiniTest.expect.equality(cache.misses, 1)
end

return T
