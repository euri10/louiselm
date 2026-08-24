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
  MiniTest.expect.equality(text, "limits Codex <1%/1h ↻30m 18%/5h ↻2h +1")
  MiniTest.expect.equality(group, "LouiselmStatusError")
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
  MiniTest.expect.equality(text, "limits codex 50%/5h ↻2h loading")
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

return T
