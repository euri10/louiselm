---The acceptance `louiselm-qbr.3.2` exists for: a generating loop stops itself.
---
---A qa-review workflow whose stage both files findings and asks for another
---round is unbounded by construction — the graph stays acyclic while the work
---grows without limit. This spec drives that loop through the real executor,
---the real capture-service ledger, and a real Beads workspace, and proves the
---Run *budget* is what stops it: `br dep cycles` stays empty throughout, so
---cycle detection can take no credit for the halt.

local MiniTest = require("mini.test")
local Acp = require("louiselm.acp")
local Workflow = require("louiselm.workflow")
local Reference = require("louiselm.dev.reference_workflow")
---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local RUN_ID = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"

---@return string? capture
---@return string? br
local function executables()
  local built = nvim.fn.getcwd() .. "/capture-service/target/debug/louiselm-capture"
  local capture = nvim.fn.executable(built) == 1 and built or nvim.fn.exepath("louiselm-capture")
  local beads = nvim.fn.exepath("br")
  if capture == "" or beads == "" then
    return nil, nil
  end
  return capture, beads
end

---@class louiselm.test.BudgetFixture
---@field capture string
---@field br string
---@field shim string
---@field database string
---@field token string
---@field environment table<string, string>

---Admit a Run over a freshly initialized, throwaway Beads workspace.
---@param ceiling integer
---@return louiselm.test.BudgetFixture
local function admitted(ceiling)
  local capture, beads = executables()
  if capture == nil or beads == nil then
    MiniTest.skip("louiselm-capture and br must both be available for budget acceptance")
  end
  local root = nvim.fn.tempname()
  nvim.fn.mkdir(root, "p")
  -- A real workspace, so `br dep cycles` has something real to answer about.
  local initialized = nvim.system({ beads, "init" }, { cwd = root, text = true }):wait()
  MiniTest.expect.equality(initialized.code, 0)
  local environment = {
    LOUISELM_CAPTURE_DATA_DIR = root .. "/data",
    LOUISELM_CAPTURE_STATE_DIR = root .. "/state",
    LOUISELM_CAPTURE_CONFIG_DIR = root .. "/state",
  }
  local admission = nvim
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
  MiniTest.expect.equality(admission.code, 0)
  local attachment = nvim
    .system({
      capture,
      "run",
      "attach",
      "--id",
      RUN_ID,
      "--session-id",
      "claude/budget-acceptance",
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
  MiniTest.expect.equality(attachment.code, 0)
  return {
    capture = capture,
    br = beads,
    shim = nvim.fn.getcwd() .. "/scripts/run-tools/br",
    database = root .. "/.beads/beads.db",
    token = nvim.json.decode(admission.stdout).token,
    environment = environment,
  }
end

---File one finding the way an Agent does: `br create` through the Run shim.
---@param fixture louiselm.test.BudgetFixture
---@param title string
---@return table result Completed `vim.system` result.
local function agent_files(fixture, title)
  return nvim
    .system({ fixture.shim, "create", title }, {
      text = true,
      env = nvim.tbl_extend("force", {}, fixture.environment, {
        LOUISELM_RUN_ID = RUN_ID,
        LOUISELM_RUN_TOKEN = fixture.token,
        LOUISELM_CAPTURE = fixture.capture,
        LOUISELM_REAL_BR = fixture.br,
        BEADS_DB = fixture.database,
      }),
    })
    :wait()
end

---@param fixture louiselm.test.BudgetFixture
---@return integer count Issues currently in the workspace.
local function issue_count(fixture)
  local listed = nvim.system({ fixture.br, "list", "--db", fixture.database, "--json" }, { text = true }):wait()
  MiniTest.expect.equality(listed.code, 0)
  local decoded = nvim.json.decode(listed.stdout)
  local issues = decoded.issues or decoded
  local count = 0
  for _ in pairs(issues) do
    count = count + 1
  end
  return count
end

---@param fixture louiselm.test.BudgetFixture
---@return integer cycles
local function cycle_count(fixture)
  local cycles = nvim
    .system({ fixture.br, "dep", "cycles", "--db", fixture.database, "--json" }, { text = true })
    :wait()
  MiniTest.expect.equality(cycles.code, 0)
  return nvim.json.decode(cycles.stdout).count
end

---@param fixture louiselm.test.BudgetFixture
---@param manifest table
local function executor_over(fixture, manifest)
  local ledger = assert(Workflow.new_ledger({ id = RUN_ID, token = fixture.token }, {
    capture = fixture.capture,
    system = function(command, options, done)
      local merged = nvim.tbl_extend("force", {}, fixture.environment, options.env or {})
      return nvim.system(command, nvim.tbl_extend("force", options, { env = merged }), done)
    end,
  }))
  return assert(Workflow.new_executor("qa-review", manifest, { ledger = ledger }))
end

---@param operation fun(callback: fun(first: unknown, second: string?)): boolean, string?
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

local T = MiniTest.new_set()

T["a generating loop Parks at its ceiling with exactly that many issues"] = function()
  -- Four units buy two full rounds: each round files one finding (one unit)
  -- and takes the back-edge for another round (one unit).
  local fixture = admitted(4)
  local executor = executor_over(fixture, Reference.qa_review({ generated_work_max = 4, max_iterations = 8 }))
  MiniTest.expect.equality(issue_count(fixture), 0)

  for round = 1, 2 do
    assert(await(function(callback)
      return executor:advance("review", nil, callback)
    end))
    MiniTest.expect.equality(agent_files(fixture, "Finding " .. round).code, 0)
    local started, result = await(function(callback)
      return executor:advance("another_round", string.format("1111111%d-2222-4333-8444-555555555555", round), callback)
    end)
    assert(started)
    MiniTest.expect.equality(assert(result).to, "execute")
    -- The budget, not cycle detection, is the only thing bounding this.
    MiniTest.expect.equality(cycle_count(fixture), 0)
  end

  MiniTest.expect.equality(issue_count(fixture), 2)

  -- The third round has nothing left to spend.
  assert(await(function(callback)
    return executor:advance("review", nil, callback)
  end))
  local refused = agent_files(fixture, "Finding 3")
  MiniTest.expect.equality(refused.code ~= 0, true)

  -- Exactly the ceiling's worth of issues exist, and no more appear after the
  -- Park: at-most and at-least are both wrong answers here.
  MiniTest.expect.equality(issue_count(fixture), 2)
  MiniTest.expect.equality(agent_files(fixture, "Finding 4").code ~= 0, true)
  MiniTest.expect.equality(issue_count(fixture), 2)

  -- The workflow Parks rather than erroring, and the graph never had a cycle.
  local started, result, error_message = await(function(callback)
    return executor:advance("another_round", "99999999-2222-4333-8444-555555555555", callback)
  end)
  assert(started)
  MiniTest.expect.equality(result, nil)
  MiniTest.expect.equality(error_message, "workflow Run budget exhausted")
  MiniTest.expect.equality(executor:inspect().status, "parked")
  MiniTest.expect.equality(cycle_count(fixture), 0)
end

T["a real Agent generating over ACP is bounded by the same budget"] = function()
  -- The other cases drive the shim directly. This one puts a whole Agent
  -- process behind it — ACP session, prompt, and all — because the contract
  -- being sold is that an *Agent* cannot outrun its Run's budget, and the
  -- layers between the Agent and the broker are part of that claim.
  local fixture = admitted(2)
  local project_root = nvim.fn.getcwd()
  local reply
  local client = assert(Acp.connect({
    command = nvim.v.progpath,
    args = {
      "--headless",
      "--noplugin",
      "-u",
      project_root .. "/tests/mock/init.lua",
      "-c",
      "lua require('louiselm.dev.mock_agent').run()",
    },
    env = nvim.tbl_extend("force", {}, fixture.environment, {
      LOUISELM_MOCK_MODE = "echo",
      -- Ask for one more than the ceiling allows.
      LOUISELM_MOCK_GENERATE_COUNT = "3",
      LOUISELM_RUN_ID = RUN_ID,
      LOUISELM_RUN_TOKEN = fixture.token,
      LOUISELM_CAPTURE = fixture.capture,
      LOUISELM_REAL_BR = fixture.br,
      BEADS_DB = fixture.database,
      PATH = project_root .. "/scripts/run-tools:" .. (nvim.env.PATH or ""),
    }),
  }, {
    on_notification = function(message)
      local update = message.params and message.params.update
      if update ~= nil and update.sessionUpdate == "agent_message_chunk" then
        reply = update.content.text
      end
    end,
  }))

  local initialized
  assert(client:initialize(nil, function(_, err)
    initialized = { error = err }
  end))
  MiniTest.expect.equality(
    nvim.wait(5000, function()
      return initialized ~= nil
    end, 10),
    true
  )
  MiniTest.expect.equality(initialized.error, nil)

  local created
  assert(client:new_session({ cwd = project_root, mcpServers = {} }, function(result, err)
    created = { result = result, error = err }
  end))
  MiniTest.expect.equality(
    nvim.wait(5000, function()
      return created ~= nil
    end, 10),
    true
  )
  MiniTest.expect.equality(created.error, nil)

  local completed
  assert(client:prompt({
    sessionId = created.result.sessionId,
    prompt = { { type = "text", text = "review this" } },
  }, function(result, err)
    completed = { result = result, error = err }
  end))
  MiniTest.expect.equality(
    nvim.wait(15000, function()
      return completed ~= nil
    end, 10),
    true
  )
  MiniTest.expect.equality(completed.error, nil)
  assert(client:close())

  local summary = nvim.json.decode(assert(reply))

  -- The Agent asked for three and got two. The refusal is the broker's, not a
  -- crash, and the workspace holds exactly the approved number of issues.
  MiniTest.expect.equality(#summary.created, 2)
  MiniTest.expect.equality(type(summary.refused), "string")
  MiniTest.expect.equality(issue_count(fixture), 2)
  MiniTest.expect.equality(cycle_count(fixture), 0)
end

T["cancelling mid-round releases the outstanding reservation"] = function()
  local fixture = admitted(4)
  local executor = executor_over(fixture, Reference.qa_review({ generated_work_max = 4, generates_max = 3 }))
  assert(await(function(callback)
    return executor:advance("review", nil, callback)
  end))
  assert(await(function(callback)
    return executor:begin_generator("11111111-2222-4333-8444-555555555555", callback)
  end))

  assert(await(function(callback)
    return executor:dispose(callback)
  end))

  -- Three of four units were reserved and none consumed. If disposal stranded
  -- them, a fresh Generator of the same size could not start.
  local resumed = executor_over(fixture, Reference.qa_review({ generated_work_max = 4, generates_max = 3 }))
  assert(await(function(callback)
    return resumed:advance("review", nil, callback)
  end))
  local started, opened, error_message = await(function(callback)
    return resumed:begin_generator("22222222-3333-4444-8555-666666666666", callback)
  end)
  assert(started)
  MiniTest.expect.equality(error_message, nil)
  MiniTest.expect.equality(opened, true)
end

T["a restart cannot spend a pending reservation twice"] = function()
  local fixture = admitted(2)
  local executor = executor_over(fixture, Reference.qa_review({ generated_work_max = 2, generates_max = 2 }))
  assert(await(function(callback)
    return executor:advance("review", nil, callback)
  end))
  local mutation = "11111111-2222-4333-8444-555555555555"
  assert(await(function(callback)
    return executor:begin_generator(mutation, callback)
  end))

  -- A new executor over the same durable Run, as after an editor restart.
  local restarted = executor_over(fixture, Reference.qa_review({ generated_work_max = 2, generates_max = 2 }))
  assert(await(function(callback)
    return restarted:advance("review", nil, callback)
  end))
  local started, opened, error_message = await(function(callback)
    return restarted:begin_generator(mutation, callback)
  end)

  -- Replaying the same identity must recognize the existing reservation rather
  -- than reserving a second two units against a ceiling of two.
  assert(started)
  MiniTest.expect.equality(opened, false)
  MiniTest.expect.equality(error_message, "Generator budget reservation is pending")
end

return T
