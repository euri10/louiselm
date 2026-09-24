-- Driven by benchmark.py; real public readers, no monkey-patching or live UI.
---@diagnostic disable-next-line: undefined-global -- Neovim benchmark runtime.
local nvim = vim
nvim.opt.rtp:prepend(nvim.fn.getcwd())
local Recording = require("louiselm.session.recording")
local Usage = require("louiselm.ui.usage")
local directory, mode = assert(arg[1]), assert(arg[2])
local store = assert(Recording.new(directory, function() end))
local function await(start)
  local done, result, failure, elapsed
  local before = nvim.uv.hrtime()
  start(function(value, err)
    elapsed = (nvim.uv.hrtime() - before) / 1e6
    done, result, failure = true, value, err
  end)
  assert(
    nvim.wait(15000, function()
      return done
    end, 1),
    "benchmark callback deadline"
  )
  assert(failure == nil, failure and failure.message)
  return result, elapsed
end
if mode == "seed" then
  await(function(cb)
    store:flush(function(err)
      cb(true, err)
    end)
  end)
  return
end
local function read_json(path)
  return nvim.json.decode(table.concat(nvim.fn.readfile(path), "\n"), { luanil = { object = true } })
end
local cases = read_json(directory .. "/cases.json")
local results = {}
for _, case in ipairs(cases) do
  nvim.env.LOUISELM_BENCH_CASE = case.name
  if mode == "capture" then
    nvim.fn.writefile(
      { nvim.json.encode({ case = case.name, completed = results }) },
      directory .. "/capture-progress.json"
    )
  end
  local function query(cb)
    if case.cohorts then
      store:usage_summaries(case.cohorts, cb)
    else
      store:usage_query(case.query, cb)
    end
  end
  local baseline, capture_ms = await(query)
  local reference_path = directory .. "/" .. case.name .. ".page.json"
  if mode == "capture" then
    nvim.fn.writefile({ nvim.json.encode(baseline) }, reference_path)
    results[case.name] = { capture_ms = capture_ms }
  else
    -- Neovim's JSON encoder rounds floating-point values; compare both sides
    -- through that same transport. In-process repeats below use exact values.
    local transported = nvim.json.decode(nvim.json.encode(baseline), { luanil = { object = true } })
    assert(nvim.deep_equal(transported, read_json(reference_path)), "API result changed")
    local times, decode_times, render_times = {}, {}, {}
    local raw = table.concat(nvim.fn.readfile(directory .. "/" .. case.name .. ".out"), "\n")
    raw = raw ~= "" and raw or "[]"
    for _ = 1, 5 do
      local page, elapsed = await(query)
      assert(nvim.deep_equal(page, baseline), "repeat result changed")
      times[#times + 1] = elapsed
      local before = nvim.uv.hrtime()
      for _ = 1, 100 do
        local decoded = nvim.json.decode(raw, { luanil = { object = true } })
        if not case.cohorts then
          nvim.json.decode(decoded[1].result, { luanil = { object = true } })
        end
      end
      decode_times[#decode_times + 1] = (nvim.uv.hrtime() - before) / 1e8
    end
    if not case.cohorts then
      local view = assert(Usage.open({
        query = function(_, cb)
          cb(baseline)
        end,
      }))
      assert(nvim.wait(1000, function()
        return view.page ~= nil
      end, 1))
      for _ = 1, 5 do
        local _, elapsed = await(function(cb)
          view:set_query(case.query)
          nvim.schedule(function()
            assert(view.page == baseline)
            cb(true)
          end)
        end)
        render_times[#render_times + 1] = elapsed
      end
      view:dispose()
    end
    results[case.name] = { api_ms = times, decode_ms = decode_times, render_ms = render_times }
  end
end
if mode == "measure" then
  nvim.fn.writefile({ nvim.json.encode(results) }, directory .. "/lua-results.json")
end
