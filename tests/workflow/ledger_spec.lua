local MiniTest = require("mini.test")
local Workflow = require("louiselm.workflow")
---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local T = MiniTest.new_set()

local RUN_ID = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"
local TOKEN = "generate-token-1234"

---Build a ledger over a recording fake `vim.system`.
---@param replies table[] Ordered `vim.system` results to return.
local function recorded(replies)
  local calls = {}
  local index = 0
  local ledger = assert(Workflow.new_ledger({ id = RUN_ID, token = TOKEN }, {
    capture = "/usr/bin/louiselm-capture",
    system = function(command, options, done)
      index = index + 1
      calls[#calls + 1] = { command = command, options = options }
      done(replies[index] or { code = 1, stdout = "", stderr = "unexpected ledger call" })
      return true
    end,
  }))
  return ledger, calls
end

---Wait for one asynchronous ledger reply.
---@param operation fun(callback: fun(first: unknown, second: string?))
local function await(operation)
  local fired, value, message = false, nil, nil
  operation(function(first, second)
    fired, value, message = true, first, second
  end)
  nvim.wait(1000, function()
    return fired
  end)
  MiniTest.expect.equality(fired, true)
  return value, message
end

---@param state string
---@param reserved integer
---@param consumed integer
local function reply(state, reserved, consumed)
  return {
    code = 0,
    stderr = "",
    stdout = string.format(
      '{"id":"%s","state":"%s","generated_work":{"ceiling":5,"consumed":%d,"reserved":%d}}',
      RUN_ID,
      state,
      consumed,
      reserved
    ),
  }
end

T["carries the Run capability in the environment, never in the argument vector"] = function()
  local ledger, calls = recorded({ reply("reserved", 3, 0) })

  MiniTest.expect.equality(
    await(function(callback)
      ledger.reserve("mutation-1", "skill_generator", 3, callback)
    end),
    "reserved"
  )

  MiniTest.expect.equality(calls[1].command, {
    "/usr/bin/louiselm-capture",
    "run",
    "reserve",
    "--mutation-id",
    "mutation-1",
    "--kind",
    "skill_generator",
    "--units",
    "3",
  })
  MiniTest.expect.equality(calls[1].options.env, {
    LOUISELM_RUN_ID = RUN_ID,
    LOUISELM_RUN_TOKEN = TOKEN,
  })
  -- A token in argv is readable from the process table by any other local user.
  for _, argument in ipairs(calls[1].command) do
    MiniTest.expect.equality(argument == TOKEN, false)
  end
end

T["charges a back-edge as a reservation confirmed against its own identity"] = function()
  local ledger, calls = recorded({ reply("reserved", 1, 0), reply("confirmed", 0, 1) })

  MiniTest.expect.equality(
    await(function(callback)
      ledger.consume("mutation-1", "back_edge", 1, callback)
    end),
    "consumed"
  )

  MiniTest.expect.equality(#calls, 2)
  MiniTest.expect.equality(calls[1].command[3], "reserve")
  MiniTest.expect.equality(calls[2].command, {
    "/usr/bin/louiselm-capture",
    "run",
    "confirm",
    "--mutation-id",
    "mutation-1",
    "--issue-id",
    "mutation-1",
  })
end

T["reports an exhausted back-edge without confirming anything"] = function()
  local ledger, calls = recorded({ reply("exhausted", 0, 5) })

  local state = await(function(callback)
    ledger.consume("mutation-1", "back_edge", 1, callback)
  end)

  MiniTest.expect.equality(state, "exhausted")
  MiniTest.expect.equality(#calls, 1)
end

T["treats an already-consumed replay as consumed without a second confirm"] = function()
  local ledger, calls = recorded({ reply("consumed", 0, 1) })

  MiniTest.expect.equality(
    await(function(callback)
      ledger.consume("mutation-1", "back_edge", 1, callback)
    end),
    "consumed"
  )

  MiniTest.expect.equality(#calls, 1)
end

T["surfaces a ledger failure without inventing a verdict"] = function()
  local ledger = recorded({ { code = 1, stdout = "", stderr = "generate token is malformed\n" } })

  local state, error_message = await(function(callback)
    ledger.reserve("mutation-1", "skill_generator", 1, callback)
  end)

  MiniTest.expect.equality(state, nil)
  MiniTest.expect.equality(error_message, "generate token is malformed")
end

T["rejects unparseable ledger output rather than guessing"] = function()
  local ledger = recorded({ { code = 0, stdout = "not json", stderr = "" } })

  local state, error_message = await(function(callback)
    ledger.release("mutation-1", callback)
  end)

  MiniTest.expect.equality(state, false)
  MiniTest.expect.equality(error_message, "Run ledger returned invalid data")
end

T["refuses construction without a Run identity or capability"] = function()
  MiniTest.expect.equality({ Workflow.new_ledger({ id = "", token = TOKEN }, { capture = "/x" }) }, {
    nil,
    "Run ledger id must be a non-empty string",
  })
  MiniTest.expect.equality({ Workflow.new_ledger({ id = RUN_ID, token = "" }, { capture = "/x" }) }, {
    nil,
    "Run ledger token must be a non-empty string",
  })
  MiniTest.expect.equality({ Workflow.new_ledger({ id = RUN_ID, token = TOKEN }, { capture = "" }) }, {
    nil,
    "Run ledger requires the louiselm-capture executable",
  })
end

return T
