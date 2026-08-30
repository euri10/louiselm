---End-to-end generated-work accounting against a real capture-service ledger.
---
---Every other executor and ledger spec answers from a double. This one drives
---the real `louiselm-capture` binary over a real Run record, because the claim
---under test is that the workflow executor and the Agent's `br` shim charge the
---*same* budget — a claim no fake can make on their behalf.

local MiniTest = require("mini.test")
local Workflow = require("louiselm.workflow")
---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local RUN_ID = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"

---Locate the built capture service, if there is one.
---@return string? path
local function capture_binary()
  local built = nvim.fn.getcwd() .. "/capture-service/target/debug/louiselm-capture"
  if nvim.fn.executable(built) == 1 then
    return built
  end
  local found = nvim.fn.exepath("louiselm-capture")
  if found ~= "" then
    return found
  end
  return nil
end

---@class louiselm.test.LedgerFixture
---@field capture string
---@field token string
---@field environment table<string, string>
---@field shim string
---@field database string
---@field fake_br string

---Admit and attach one real Run with the requested ceiling.
---@param ceiling integer
---@return louiselm.test.LedgerFixture
local function admitted_run(ceiling)
  local capture = capture_binary()
  if capture == nil then
    MiniTest.skip("louiselm-capture is not built; run `cargo build` in capture-service/")
  end
  local root = nvim.fn.tempname()
  nvim.fn.mkdir(root, "p")
  local environment = {
    LOUISELM_CAPTURE_DATA_DIR = root .. "/data",
    LOUISELM_CAPTURE_STATE_DIR = root .. "/state",
    LOUISELM_CAPTURE_CONFIG_DIR = root .. "/state",
  }
  -- Test setup blocks deliberately: `:wait()` is forbidden in plugin code
  -- because it stalls the user's editor, which is not a concern here.
  local admitted = nvim
    .system({
      capture,
      "run",
      "admit",
      "--id",
      RUN_ID,
      "--generated-work-max",
      tostring(ceiling),
      "--park-ttl-ms",
      "3600000",
    }, { text = true, env = environment })
    :wait()
  MiniTest.expect.equality(admitted.code, 0)
  local token = nvim.json.decode(admitted.stdout).token
  local attached = nvim
    .system({
      capture,
      "run",
      "attach",
      "--id",
      RUN_ID,
      "--session-id",
      "claude/ledger-integration",
      "--agent",
      "claude",
      "--acp-session-id",
      "acp-1",
      "--cwd",
      root,
      "--load-session",
      "true",
    }, { text = true, env = environment })
    :wait()
  MiniTest.expect.equality(attached.code, 0)

  -- A fake `br` keeps the real Beads workspace out of the test while still
  -- exercising the broker's own reserve/create/confirm path.
  local fake_br = root .. "/fake-br"
  local counter = root .. "/create-count"
  local script = table.concat({
    "#!/bin/sh",
    "set -eu",
    'case "$1" in',
    "  create)",
    "    count=0",
    "    if [ -f '" .. counter .. "' ]; then count=$(cat '" .. counter .. "'); fi",
    "    count=$((count + 1))",
    "    printf '%s' \"$count\" > '" .. counter .. "'",
    '    printf \'{"id":"generated-%s"}\\n\' "$count"',
    "    ;;",
    "  list) printf '[]\\n' ;;",
    "  *) exit 2 ;;",
    "esac",
  }, "\n")
  local handle = assert(io.open(fake_br, "w"))
  handle:write(script)
  handle:close()
  nvim.fn.setfperm(fake_br, "rwx------")

  return {
    capture = capture,
    token = token,
    environment = environment,
    shim = nvim.fn.getcwd() .. "/scripts/run-tools/br",
    database = root .. "/beads.db",
    fake_br = fake_br,
  }
end

---Wait for one asynchronous executor operation.
local function await(operation)
  local fired, value, message = false, nil, nil
  local started, start_error = operation(function(first, second)
    fired, value, message = true, first, second
  end)
  if not started then
    return false, nil, start_error
  end
  nvim.wait(5000, function()
    return fired
  end)
  MiniTest.expect.equality(fired, true)
  return true, value, message
end

---Build an executor bound to the real ledger for this Run.
---@param fixture louiselm.test.LedgerFixture
---@param manifest table
local function executor_over(fixture, manifest)
  local ledger = assert(Workflow.new_ledger({ id = RUN_ID, token = fixture.token }, {
    capture = fixture.capture,
    system = function(command, options, done)
      -- The ledger owns the Run capability; the sandbox directories are the
      -- test's business and ride along in the inherited environment.
      local merged = nvim.tbl_extend("force", {}, fixture.environment, options.env or {})
      return nvim.system(command, nvim.tbl_extend("force", options, { env = merged }), done)
    end,
  }))
  return assert(Workflow.new_executor("reference", manifest, { ledger = ledger }))
end

local function loop_manifest()
  return {
    entry = {
      workflow = "reference",
      entry = true,
      ["generated-work"] = { max = 5 },
      ["park-expiry"] = "1h",
      outcomes = { { name = "review", to = "review" } },
    },
    review = {
      workflow = "reference",
      outcomes = {
        {
          name = "retry",
          to = "entry",
          ["back-edge"] = true,
          resolver = "agent",
          ["max-iterations"] = 4,
          ["on-exhausted"] = "accepted",
        },
        { name = "accepted", terminal = true },
      },
    },
  }
end

local T = MiniTest.new_set()

T["an executor back-edge and an Agent br create charge one shared budget"] = function()
  local fixture = admitted_run(2)
  local executor = executor_over(fixture, loop_manifest())

  -- One unit through the Agent's supported path.
  local created = nvim
    .system({ fixture.shim, "create", "--title", "Generated" }, {
      text = true,
      env = nvim.tbl_extend("force", {}, fixture.environment, {
        LOUISELM_RUN_ID = RUN_ID,
        LOUISELM_RUN_TOKEN = fixture.token,
        LOUISELM_CAPTURE = fixture.capture,
        LOUISELM_REAL_BR = fixture.fake_br,
        BEADS_DB = fixture.database,
      }),
    })
    :wait()
  MiniTest.expect.equality(created.code, 0)

  -- One unit through the workflow executor.
  assert(await(function(callback)
    return executor:advance("review", nil, callback)
  end))
  local started, result = await(function(callback)
    return executor:advance("retry", "11111111-2222-4333-8444-555555555555", callback)
  end)
  assert(started)
  MiniTest.expect.equality(assert(result).to, "entry")

  -- The ceiling of two is now spent by one unit from each mechanism, so the
  -- next traversal must be refused. If the two kept separate books this would
  -- succeed and the Run-budget contract would be worthless.
  assert(await(function(callback)
    return executor:advance("review", nil, callback)
  end))
  local exhausted_started, exhausted_result, exhausted_error = await(function(callback)
    return executor:advance("retry", "22222222-3333-4444-8555-666666666666", callback)
  end)
  assert(exhausted_started)
  MiniTest.expect.equality(exhausted_result, nil)
  MiniTest.expect.equality(exhausted_error, "workflow Run budget exhausted")
  MiniTest.expect.equality(executor:inspect().status, "parked")
end

T["a skill Generator reserves its maximum and returns the remainder"] = function()
  local fixture = admitted_run(5)
  local manifest = loop_manifest()
  manifest.review.generates = { max = 3 }
  local executor = executor_over(fixture, manifest)
  assert(await(function(callback)
    return executor:advance("review", nil, callback)
  end))

  assert(await(function(callback)
    return executor:begin_generator("11111111-2222-4333-8444-555555555555", callback)
  end))
  assert(await(function(callback)
    return executor:record_generator_output("generated-1", callback)
  end))
  assert(await(function(callback)
    return executor:end_generator(callback)
  end))

  -- One of five units is consumed and the other two reservations are back. A
  -- second Generator of the same declared maximum only fits if the remainder
  -- was really released: 3 + 1 consumed is under the ceiling, 3 + 2 stranded
  -- reservations + 1 consumed would not be.
  local started, opened, error_message = await(function(callback)
    return executor:begin_generator("22222222-3333-4444-8555-666666666666", callback)
  end)
  assert(started)
  MiniTest.expect.equality(error_message, nil)
  MiniTest.expect.equality(opened, true)
end

return T
