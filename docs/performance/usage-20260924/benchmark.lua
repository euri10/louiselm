-- Driven by benchmark.py; real public readers, no monkey-patching or live UI.
---@diagnostic disable-next-line: undefined-global -- Neovim benchmark runtime.
local nvim = vim
nvim.opt.rtp:prepend(nvim.fn.getcwd())
local Recording = require("louiselm.session.recording")
local Usage = require("louiselm.ui.usage")
local directory, mode = assert(arg[1]), assert(arg[2])
local store = assert(Recording.new(directory, function() end))
local results, progress = {}, {}
local function checkpoint(step, iteration)
  if mode ~= "measure" then
    return
  end
  progress.step, progress.iteration = step, iteration
  local path = directory .. "/measure-progress.json"
  assert(nvim.fn.writefile({ nvim.json.encode(progress) }, path .. ".tmp") == 0)
  assert(nvim.uv.fs_rename(path .. ".tmp", path))
end
local function fail(category, failure)
  progress.category = category
  -- Only the reader's fixed error codes are safe to retain, never its message.
  local codes = {
    invalid = true,
    unavailable = true,
    permissions = true,
    locked = true,
    corrupt = true,
    conflict = true,
    storage = true,
    attribution = true,
  }
  if failure and codes[failure.code] then
    progress.error_code = failure.code
  end
  error(category, 0)
end
local function await(start)
  local done, result, failure, elapsed
  local before = nvim.uv.hrtime()
  start(function(value, err)
    elapsed = (nvim.uv.hrtime() - before) / 1e6
    done, result, failure = true, value, err
  end)
  if not nvim.wait(15000, function()
    return done
  end, 1) then
    fail("callback_timeout")
  end
  if failure ~= nil then
    fail("query_error", failure)
  end
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
local function run_case(case)
  nvim.env.LOUISELM_BENCH_CASE = case.name
  local times, decode_times, render_times = {}, {}, {}
  progress = {
    case = case.name,
    completed = results,
    partial = { api_ms = times, decode_ms = decode_times, render_ms = render_times },
  }
  checkpoint("baseline")
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
    checkpoint("compare")
    -- Neovim's JSON encoder rounds floating-point values; compare both sides
    -- through that same transport. In-process repeats below use exact values.
    local transported = nvim.json.decode(nvim.json.encode(baseline), { luanil = { object = true } })
    if not nvim.deep_equal(transported, read_json(reference_path)) then
      fail("result_mismatch")
    end
    checkpoint("decode_input")
    local raw = table.concat(nvim.fn.readfile(directory .. "/" .. case.name .. ".out"), "\n")
    raw = raw ~= "" and raw or "[]"
    for iteration = 1, 5 do
      checkpoint("api", iteration)
      local page, elapsed = await(query)
      if not nvim.deep_equal(page, baseline) then
        fail("result_mismatch")
      end
      times[#times + 1] = elapsed
      checkpoint("decode", iteration)
      local before = nvim.uv.hrtime()
      for _ = 1, 100 do
        local decoded = nvim.json.decode(raw, { luanil = { object = true } })
        if not case.cohorts then
          nvim.json.decode(decoded[1].result, { luanil = { object = true } })
        end
      end
      decode_times[#decode_times + 1] = (nvim.uv.hrtime() - before) / 1e8
      checkpoint("decode", iteration)
    end
    if not case.cohorts then
      checkpoint("render_setup")
      local view = assert(Usage.open({
        query = function(_, cb)
          cb(baseline)
        end,
      }))
      assert(nvim.wait(1000, function()
        return view.page ~= nil
      end, 1))
      for iteration = 1, 5 do
        checkpoint("render", iteration)
        local _, elapsed = await(function(cb)
          view:set_query(case.query)
          nvim.schedule(function()
            assert(view.page == baseline)
            cb(true)
          end)
        end)
        render_times[#render_times + 1] = elapsed
        checkpoint("render", iteration)
      end
      checkpoint("render_dispose")
      view:dispose()
    end
    results[case.name] = { api_ms = times, decode_ms = decode_times, render_ms = render_times }
    progress.partial = nil
    checkpoint("complete")
  end
end
for _, case in ipairs(cases) do
  local ok, failure = pcall(run_case, case)
  if not ok then
    -- Classify unexpected errors without copying their potentially private text.
    progress.category = progress.category or "lua_error"
    checkpoint(progress.step, progress.iteration)
    error(mode == "measure" and ("benchmark " .. progress.category) or failure, 0)
  end
end
if mode == "measure" then
  nvim.fn.writefile({ nvim.json.encode(results) }, directory .. "/lua-results.json")
end
