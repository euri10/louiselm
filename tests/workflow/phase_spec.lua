local MiniTest = require("mini.test")
local Phase = require("louiselm.workflow.phase")

local T = MiniTest.new_set()

T["canonical"] = MiniTest.new_set()

T["canonical"]["lists the canonical phases in workflow order"] = function()
  MiniTest.expect.equality(Phase.canonical(), {
    "design",
    "planning",
    "implementation",
    "review",
    "qa",
    "mechanical",
  })
end

T["canonical"]["returns a fresh copy the caller may not use to mutate the contract"] = function()
  local phases = Phase.canonical()
  phases[1] = "mechanical"

  MiniTest.expect.equality(Phase.canonical()[1], "design")
end

T["canonical"]["recognizes canonical names only"] = function()
  MiniTest.expect.equality(Phase.is_canonical("qa"), true)
  MiniTest.expect.equality(Phase.is_canonical("testing"), false)
  MiniTest.expect.equality(Phase.is_canonical("QA"), false)
  MiniTest.expect.equality(Phase.is_canonical(""), false)
end

T["parse"] = MiniTest.new_set()

T["parse"]["accepts a bare phase name as the primary phase"] = function()
  MiniTest.expect.equality(assert(Phase.parse("implementation")), {
    primary = "implementation",
    secondary = {},
    source = "explicit",
    confidence = 1,
  })
end

T["parse"]["accepts a mapping with a primary and ordered secondary phases"] = function()
  MiniTest.expect.equality(assert(Phase.parse({ primary = "qa", secondary = { "review", "mechanical" } })), {
    primary = "qa",
    secondary = { "review", "mechanical" },
    source = "explicit",
    confidence = 1,
  })
end

T["parse"]["treats an omitted secondary as no secondary phases"] = function()
  MiniTest.expect.equality(assert(Phase.parse({ primary = "design" })).secondary, {})
end

T["parse"]["rejects a value that is neither a name nor a mapping"] = function()
  local metadata, error_message = Phase.parse(7)

  MiniTest.expect.equality(metadata, nil)
  MiniTest.expect.equality(error_message, "phase must be a phase name or a mapping")
end

T["parse"]["names the unknown phase and the accepted alternatives"] = function()
  local metadata, error_message = Phase.parse("testing")

  MiniTest.expect.equality(metadata, nil)
  MiniTest.expect.equality(
    error_message,
    "unknown phase 'testing'; expected one of design, planning, implementation, review, qa, mechanical"
  )
end

T["parse"]["rejects a mapping without a primary phase"] = function()
  local metadata, error_message = Phase.parse({ secondary = { "review" } })

  MiniTest.expect.equality(metadata, nil)
  MiniTest.expect.equality(error_message, "phase mapping requires a primary phase name")
end

T["parse"]["rejects an unknown secondary phase"] = function()
  local metadata, error_message = Phase.parse({ primary = "qa", secondary = { "review", "smoke" } })

  MiniTest.expect.equality(metadata, nil)
  MiniTest.expect.equality(
    error_message,
    "unknown phase 'smoke'; expected one of design, planning, implementation, review, qa, mechanical"
  )
end

T["parse"]["rejects a secondary list that is not a dense array of names"] = function()
  local metadata, error_message = Phase.parse({ primary = "qa", secondary = "review" })

  MiniTest.expect.equality(metadata, nil)
  MiniTest.expect.equality(error_message, "phase secondary must be an array of phase names")
end

T["parse"]["rejects a secondary phase that repeats the primary"] = function()
  local metadata, error_message = Phase.parse({ primary = "qa", secondary = { "qa" } })

  MiniTest.expect.equality(metadata, nil)
  MiniTest.expect.equality(error_message, "phase secondary must not repeat the primary phase 'qa'")
end

T["parse"]["rejects a repeated secondary phase"] = function()
  local metadata, error_message = Phase.parse({ primary = "qa", secondary = { "review", "review" } })

  MiniTest.expect.equality(metadata, nil)
  MiniTest.expect.equality(error_message, "phase secondary must not repeat 'review'")
end

T["parse"]["rejects an unknown key rather than silently ignoring a typo"] = function()
  local metadata, error_message = Phase.parse({ primary = "qa", secondaries = { "review" } })

  MiniTest.expect.equality(metadata, nil)
  MiniTest.expect.equality(error_message, "unknown phase mapping key 'secondaries'")
end

T["infer"] = MiniTest.new_set()

T["infer"]["reads qa-review as primary qa with secondary review"] = function()
  MiniTest.expect.equality(assert(Phase.infer("qa-review")), {
    primary = "qa",
    secondary = { "review" },
    source = "inferred",
    confidence = 0.5,
  })
end

T["infer"]["infers a single phase from an exact name"] = function()
  MiniTest.expect.equality(assert(Phase.infer("review")), {
    primary = "review",
    secondary = {},
    source = "inferred",
    confidence = 0.5,
  })
end

T["infer"]["lowers confidence for the share of the name it could not read"] = function()
  MiniTest.expect.equality(assert(Phase.infer("security-review")).confidence, 0.25)
  MiniTest.expect.equality(assert(Phase.infer("security-review")).primary, "review")
end

T["infer"]["never reaches the confidence of declared metadata"] = function()
  local inferred = assert(Phase.infer("qa-review"))
  local explicit = assert(Phase.parse("qa"))

  MiniTest.expect.equality(inferred.confidence < explicit.confidence, true)
end

T["infer"]["ignores a repeated phase segment rather than reporting it twice"] = function()
  MiniTest.expect.equality(assert(Phase.infer("review-review")).secondary, {})
end

T["infer"]["declines to guess when no segment names a phase"] = function()
  MiniTest.expect.equality(Phase.infer("grill-me"), nil)
  MiniTest.expect.equality(Phase.infer(""), nil)
end

T["resolve"] = MiniTest.new_set()

T["resolve"]["prefers declared metadata over the name"] = function()
  local metadata = assert(Phase.resolve("design", "qa-review"))

  MiniTest.expect.equality(metadata.primary, "design")
  MiniTest.expect.equality(metadata.source, "explicit")
end

T["resolve"]["falls back to inference when nothing is declared"] = function()
  local metadata = assert(Phase.resolve(nil, "qa-review"))

  MiniTest.expect.equality(metadata.primary, "qa")
  MiniTest.expect.equality(metadata.source, "inferred")
end

T["resolve"]["reports malformed declared metadata instead of falling back"] = function()
  local metadata, error_message = Phase.resolve("testing", "qa-review")

  MiniTest.expect.equality(metadata, nil)
  MiniTest.expect.equality(
    error_message,
    "unknown phase 'testing'; expected one of design, planning, implementation, review, qa, mechanical"
  )
end

T["resolve"]["yields no phase when neither declaration nor name provides one"] = function()
  local metadata, error_message = Phase.resolve(nil, "grill-me")

  MiniTest.expect.equality(metadata, nil)
  MiniTest.expect.equality(error_message, nil)
end

return T
