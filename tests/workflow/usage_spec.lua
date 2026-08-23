local MiniTest = require("mini.test")
local Usage = require("louiselm.workflow.usage")

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim
local temp_dir
local T = MiniTest.new_set({
  hooks = {
    pre_case = function()
      temp_dir = nvim.fn.tempname()
      assert(nvim.fn.mkdir(temp_dir, "p") == 1)
    end,
    post_case = function()
      nvim.fn.delete(temp_dir, "rf")
      temp_dir = nil
    end,
  },
})

local function store_path()
  return nvim.fs.joinpath(temp_dir, "usage.json")
end

local function new_store()
  return assert(Usage.new(store_path()))
end

local function options(value)
  return {
    { id = "model", name = "Model", current_value = value },
    { id = "effort", name = "Effort", current_value = "medium" },
  }
end

local function summary_for(store, option, value)
  return assert(store:summary("claude", option, value))
end

T["record"] = MiniTest.new_set()

T["record"]["attributes measured tokens to every active option value"] = function()
  local store = new_store()

  assert(store:record("claude", options("small"), { total_tokens = 120 }))

  MiniTest.expect.equality(summary_for(store, "model", "small"), {
    samples = 1,
    token_samples = 1,
    average_tokens = 120,
    costs = {},
  })
  MiniTest.expect.equality(summary_for(store, "effort", "medium").average_tokens, 120)
end

T["record"]["keeps option values separate and averages repeated observations"] = function()
  local store = new_store()

  assert(store:record("claude", options("small"), { total_tokens = 100 }))
  assert(store:record("claude", options("small"), { total_tokens = 200 }))
  assert(store:record("claude", options("large"), { total_tokens = 900 }))

  MiniTest.expect.equality(summary_for(store, "model", "small").average_tokens, 150)
  MiniTest.expect.equality(summary_for(store, "model", "small").samples, 2)
  MiniTest.expect.equality(summary_for(store, "model", "large").average_tokens, 900)
end

T["record"]["sums known token fields when total is absent"] = function()
  local store = new_store()

  assert(store:record("claude", options("small"), { input_tokens = 40, output_tokens = 12 }))

  MiniTest.expect.equality(summary_for(store, "model", "small").average_tokens, 52)
end

T["record"]["keeps reported currency costs separate from token measurements"] = function()
  local store = new_store()

  assert(store:record("claude", options("small"), { total_tokens = 100 }, { amount = 0.5, currency = "USD" }))
  assert(store:record("claude", options("small"), { total_tokens = 200 }, { amount = 1, currency = "USD" }))

  MiniTest.expect.equality(summary_for(store, "model", "small").costs, {
    { currency = "USD", samples = 2, average = 0.75 },
  })
end

T["record"]["ignores turns with no measured usage or cost"] = function()
  local store = new_store()

  assert(store:record("claude", options("small"), nil))

  MiniTest.expect.equality(assert(store:records()), {})
end

T["record"]["rejects malformed options and measurements"] = function()
  local store = new_store()

  local ok, error_message = store:record("claude", { { id = "model", current_value = { "small" } } }, {})

  MiniTest.expect.equality(ok, false)
  MiniTest.expect.equality(error_message, "usage options must contain string ids and string or boolean values")
end

T["persistence"] = MiniTest.new_set()

T["persistence"]["survives a restart"] = function()
  local store = new_store()
  assert(store:record("claude", options("small"), { total_tokens = 120 }))

  local reopened = assert(Usage.new(store_path()))
  MiniTest.expect.equality(summary_for(reopened, "model", "small").average_tokens, 120)
end

T["persistence"]["reports corrupt state without overwriting it"] = function()
  assert(nvim.fn.writefile({ "not json" }, store_path()) == 0)
  local store = new_store()

  local records, error_message = store:records()

  MiniTest.expect.equality(records, nil)
  MiniTest.expect.equality(error_message, "usage history is not valid JSON")
  local ok, write_error = store:record("claude", options("small"), { total_tokens = 1 })
  MiniTest.expect.equality(ok, false)
  MiniTest.expect.equality(write_error, "usage history is not valid JSON")
  MiniTest.expect.equality(nvim.fn.readfile(store_path()), { "not json" })
end

T["persistence"]["does not persist option names or other picker metadata"] = function()
  local store = new_store()
  assert(store:record("claude", options("small"), { total_tokens = 120 }))

  local stored = nvim.json.decode(table.concat(nvim.fn.readfile(store_path()), "\n"))
  local record = stored.records[1]
  MiniTest.expect.equality(record.name, nil)
  MiniTest.expect.equality(record.agent, "claude")
  MiniTest.expect.equality(record.option, "effort")
  MiniTest.expect.equality(record.value, "medium")
end

return T
