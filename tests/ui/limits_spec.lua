local MiniTest = require("mini.test")
local Limits = require("louiselm.ui.limits")

local T = MiniTest.new_set()

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local NOW = 4102437600

local function state(status, buckets, default_bucket_id)
  return {
    agent = "codex",
    status = status,
    snapshot = {
      default_bucket_id = default_bucket_id,
      buckets = buckets,
    },
  }
end

T["summary"] = MiniTest.new_set()

T["summary"]["marks unsupported limits neutrally and explains unobserved Agents"] = function()
  local unsupported = { agent = "stock", status = "unsupported" }
  local text, group = Limits.summary(unsupported, NOW)
  MiniTest.expect.equality(text, "limits n/a")
  MiniTest.expect.equality(group, "Comment")
  MiniTest.expect.equality(
    nvim.tbl_contains(
      Limits.render(unsupported, NOW),
      "Account limits are unavailable: this Agent does not advertise support."
    ),
    true
  )

  local unobserved = { agent = "stock", status = "not_observed" }
  MiniTest.expect.equality({ Limits.summary(unobserved, NOW) }, {})
  MiniTest.expect.equality(
    nvim.tbl_contains(
      Limits.render(unobserved, NOW),
      "Account limits have not been observed: start a Session with this Agent to check support."
    ),
    true
  )
end

T["summary"]["sorts and caps default windows with compact remaining capacity"] = function()
  local value = state("fresh", {
    {
      id = "codex",
      label = "Codex",
      windows = {
        { used_percent = 33, duration_mins = 10080, resets_at = NOW + 345600 },
        { used_percent = 99.5, duration_mins = 60, resets_at = NOW + 1800 },
        { used_percent = 82, duration_mins = 300, resets_at = NOW + 7200 },
      },
    },
  }, "codex")

  local text, group = Limits.summary(value, NOW)
  MiniTest.expect.equality(text, "limits <1%/1h ↻30m 18%/5h ↻2h +1")
  MiniTest.expect.equality(group, "LouiselmStatusError")
end

T["summary"]["preserves meaningful default labels and all alternate bucket labels"] = function()
  local bucket = {
    id = "codex",
    label = "Spark",
    windows = { { used_percent = 91, duration_mins = 300, resets_at = NOW + 7200 } },
  }
  MiniTest.expect.equality(Limits.summary(state("fresh", { bucket }, "codex"), NOW), "limits Spark 9%/5h ↻2h")

  bucket.label = "Codex"
  MiniTest.expect.equality(Limits.summary(state("fresh", { bucket }, "other"), NOW), "limits Codex 9%/5h ↻2h")
end

T["summary"]["lets an urgent additional bucket take precedence and marks stale data"] = function()
  local buckets = {
    { id = "default", windows = { { used_percent = 20, duration_mins = 300, resets_at = NOW + 7200 } } },
    {
      id = "fast",
      label = "Fast",
      windows = { { used_percent = 91, duration_mins = 90, resets_at = NOW + 3600 } },
    },
  }
  local text, group = Limits.summary(state("fresh", buckets, "default"), NOW)
  MiniTest.expect.equality(text, "limits Fast 9%/90m ↻1h")
  MiniTest.expect.equality(group, "LouiselmStatusError")

  text, group = Limits.summary(state("stale", buckets, "default"), NOW)
  MiniTest.expect.equality(text, "limits Fast 9%/90m ↻1h stale")
  MiniTest.expect.equality(group, "LouiselmStatusWarning")
end

T["summary"]["shows loading and detail freshness age explicitly"] = function()
  local value = state("loading", {
    { id = "codex", windows = { { used_percent = 50, duration_mins = 300, resets_at = NOW + 7200 } } },
  }, "codex")
  value.updated_at = NOW - 3700

  local text = Limits.summary(value, NOW)
  MiniTest.expect.equality(text, "limits 50%/5h ↻2h loading")
  MiniTest.expect.equality(Limits.render(value, NOW)[5]:find("(1h ago)", 1, true) ~= nil, true)
end

T["summary"]["advertises available reset credits beside a reached bucket"] = function()
  local value = state("fresh", {
    {
      id = "codex",
      reached_type = "weekly",
      windows = { { used_percent = 100, duration_mins = 10080, resets_at = NOW + 7200 } },
    },
  }, "codex")
  value.snapshot.reset_credits = { available_count = 2 }

  MiniTest.expect.equality(
    nvim.tbl_contains(Limits.render(value, NOW), "Reached: weekly · Reset credits available: 2"),
    true
  )
end

T["alerts"] = MiniTest.new_set()

T["alerts"]["deduplicates escalating thresholds per reset cycle and ignores stale data"] = function()
  local bucket = {
    id = "codex",
    label = "Codex",
    windows = { { used_percent = 81, duration_mins = 300, resets_at = NOW + 7200 } },
  }
  local value = state("fresh", { bucket }, "codex")
  local alerts, seen = Limits.threshold_alerts(value, {}, NOW)
  MiniTest.expect.equality(#alerts, 1)
  MiniTest.expect.equality(alerts[1].level, "warning")
  MiniTest.expect.equality(alerts[1].message, "codex Codex 5h has 19% remaining; resets in 2h")

  alerts, seen = Limits.threshold_alerts(value, seen, NOW)
  MiniTest.expect.equality(alerts, {})

  bucket.windows[1].used_percent = 91
  alerts, seen = Limits.threshold_alerts(value, seen, NOW)
  MiniTest.expect.equality(#alerts, 1)
  MiniTest.expect.equality(alerts[1].level, "error")

  value.status = "stale"
  bucket.reached_type = "weekly"
  alerts, seen = Limits.threshold_alerts(value, seen, NOW)
  MiniTest.expect.equality(alerts, {})

  value.status = "fresh"
  alerts, seen = Limits.threshold_alerts(value, seen, NOW)
  MiniTest.expect.equality(#alerts, 1)
  MiniTest.expect.equality(alerts[1].message, "codex Codex 5h limit reached (weekly); resets in 2h")

  bucket.reached_type = nil
  bucket.windows[1].resets_at = NOW + 10800
  alerts = Limits.threshold_alerts(value, seen, NOW)
  MiniTest.expect.equality(#alerts, 1)
end

T["countdowns"] = MiniTest.new_set({
  parametrize = {
    { 1, "1m" },
    { 60, "1m" },
    { 61, "2m" },
    { 3599, "60m" },
    { 3600, "1h" },
    { 3601, "2h" },
    { 86399, "24h" },
    { 86400, "1d 0h" },
    { 86401, "1d 0h" },
    { 90000, "1d 1h" },
    { 6 * 86400 + 1800, "6d 0h" },
    { 7 * 86400 - 1, "6d 23h" },
    { 7 * 86400, "7d 0h" },
  },
})

T["countdowns"]["agree across summary, alerts, details and credit expiry using the supplied clock"] = function(
  seconds,
  expected
)
  local resets_at = NOW + seconds
  local value = state("fresh", {
    { id = "codex", windows = { { used_percent = 81, duration_mins = 10080, resets_at = resets_at } } },
  }, "codex")
  value.snapshot.reset_credits = { available_count = 1, credits = { { title = "Reset", expires_at = resets_at } } }

  MiniTest.expect.equality(Limits.summary(value, NOW), "limits 19%/7d ↻" .. expected)
  local alerts = Limits.threshold_alerts(value, {}, NOW)
  MiniTest.expect.equality(alerts[1].message, "codex codex 7d has 19% remaining; resets in " .. expected)
  local lines = Limits.render(value, NOW)
  local reset = "in " .. expected .. " (" .. os.date("%Y-%m-%d %H:%M %Z", resets_at) .. ")"
  MiniTest.expect.equality(nvim.tbl_contains(lines, "Window: 7d · Resets: " .. reset), true)
  MiniTest.expect.equality(nvim.tbl_contains(lines, "- Reset · Expires: " .. reset), true)
end

T["render"] = MiniTest.new_set({ parametrize = { { 0 }, { -1 } } })

T["render"]["marks reset and credit expiry times as passed at the supplied clock"] = function(seconds)
  local resets_at = NOW + seconds
  local value = state("stale", {
    { id = "codex", windows = { { used_percent = 81, duration_mins = 10080, resets_at = resets_at } } },
  }, "codex")
  value.snapshot.reset_credits = { available_count = 1, credits = { { title = "Reset", expires_at = resets_at } } }

  local lines = Limits.render(value, NOW)
  local reset = "reset time passed (" .. os.date("%Y-%m-%d %H:%M %Z", resets_at) .. ")"
  MiniTest.expect.equality(nvim.tbl_contains(lines, "Window: 7d · Resets: " .. reset), true)
  MiniTest.expect.equality(nvim.tbl_contains(lines, "- Reset · Expires: " .. reset), true)
end

return T
