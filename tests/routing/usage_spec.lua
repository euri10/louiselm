local MiniTest = require("mini.test")
local Usage = require("louiselm.routing.usage")

---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local directory, path
local T = MiniTest.new_set({
  hooks = {
    pre_case = function()
      directory = nvim.fn.tempname()
      assert(nvim.fn.mkdir(directory, "p") == 1)
      path = directory .. "/usage.json"
    end,
    post_case = function()
      nvim.fn.delete(directory, "rf")
    end,
  },
})

local function fixture(records, turns)
  assert(nvim.fn.writefile({ nvim.json.encode({ version = 1, records = records or {}, turns = turns }) }, path) == 0)
  return assert(Usage.new(path))
end
local function bucket(value, tokens, costs)
  return {
    agent = "claude",
    option = "model",
    value = value,
    samples = 2,
    token_samples = 2,
    total_tokens = tokens,
    costs = costs or {},
    updated_at = 1,
  }
end

-- lc32 retires JSON writes. Fixtures retain the exact pre-rollout format:
-- marginal buckets serve the old picker, while exact turns serve replay only.
T["reads legacy option summaries without combining values or currencies"] = function()
  local store = fixture({
    bucket("small", 300, { USD = { samples = 2, total = 1.5 }, EUR = { samples = 1, total = 2 } }),
    bucket("large", 900),
    bucket(false, 0),
    bucket("false", 42),
  })
  MiniTest.expect.equality(store:summary("claude", "model", "small"), {
    samples = 2,
    token_samples = 2,
    average_tokens = 150,
    costs = { { currency = "EUR", samples = 1, average = 2 }, { currency = "USD", samples = 2, average = 0.75 } },
  })
  MiniTest.expect.equality(assert(store:summary("claude", "model", false)).average_tokens, 0)
  MiniTest.expect.equality(assert(store:summary("claude", "model", "false")).average_tokens, 21)
  MiniTest.expect.equality(store:summary("other", "model", "small"), nil)
  MiniTest.expect.equality(store:summary("claude", "missing", "small"), nil)
end

T["missing measurements stay absent and returned buckets are detached"] = function()
  local record = bucket("small", 0, { USD = { samples = 1, total = 0 } })
  record.token_samples = 0
  local store = fixture({ record })
  MiniTest.expect.equality(assert(store:summary("claude", "model", "small")).average_tokens, nil)
  local records = assert(store:records())
  records[1].costs.USD.total = 100
  MiniTest.expect.equality(assert(store:records())[1].costs.USD.total, 0)
end

T["restores exact usage only for the matching Agent and ACP Session"] = function()
  local turn = {
    agent = "deepseek",
    session_id = "prior-acp",
    turn = 2,
    usage = { input_tokens = 20, thought_tokens = 0, cached_read_tokens = 7, cached_write_tokens = 3 },
  }
  local store = fixture({ bucket("small", 30) }, { turn })
  local before = nvim.fn.readfile(path)
  local rows = assert(store:turns("deepseek", "prior-acp"))
  MiniTest.expect.equality(rows, { turn })
  rows[1].usage.input_tokens = 999
  MiniTest.expect.equality(assert(assert(Usage.new(path)):turns("deepseek", "prior-acp")), { turn })
  MiniTest.expect.equality(store:turns("other", "prior-acp"), {})
  MiniTest.expect.equality(store:turns("deepseek", "other"), {})
  MiniTest.expect.equality(nvim.fn.readfile(path), before)
end

T["missing ledger or pre-exact schema produces no invented annotations"] = function()
  local store = assert(Usage.new(path))
  MiniTest.expect.equality(store:turns("agent", "session"), {})
  MiniTest.expect.equality(nvim.uv.fs_stat(path), nil)
  fixture({ bucket("small", 100) })
  MiniTest.expect.equality(store:turns("agent", "session"), {})
end

T["rejects malformed exact usage and closed-schema fields without rewriting"] = function()
  for _, changes in ipairs({
    { turn = 0 },
    { usage = { total_tokens = "1" } },
    { usage = { input_tokens = -1 } },
    { usage = { total_tokens = 1, secret = true } },
    { unknown = true },
  }) do
    local turn = nvim.tbl_extend(
      "force",
      { agent = "agent", session_id = "session", turn = 1, usage = { total_tokens = 1 } },
      changes
    )
    local store = fixture({}, { turn })
    local before = nvim.fn.readfile(path)
    local records, err = store:turns("agent", "session")
    MiniTest.expect.equality(records, nil)
    MiniTest.expect.equality(err, "usage history has invalid schema")
    MiniTest.expect.equality(nvim.fn.readfile(path), before)
  end
end

T["corrupt and non-file ledgers report errors without changing bytes"] = function()
  nvim.fn.writefile({ "not json" }, path)
  local store = assert(Usage.new(path))
  local records, err = store:records()
  MiniTest.expect.equality(records, nil)
  MiniTest.expect.equality(err, "usage history is not valid JSON")
  MiniTest.expect.equality(nvim.fn.readfile(path), { "not json" })
  local unsafe = assert(Usage.new(directory))
  MiniTest.expect.equality(select(2, unsafe:turns("agent", "session")), "usage history path is not a regular file")
end

T["invalid query identity is rejected"] = function()
  local store = assert(Usage.new(path))
  MiniTest.expect.equality(select(2, store:turns("", "session")), "usage turns require non-empty Agent and Session ids")
  MiniTest.expect.equality(
    select(2, store:summary("agent", "model", {})),
    "usage summary value must be a string or boolean"
  )
end

return T
