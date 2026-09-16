-- Run from the repository root with nvim --headless --noplugin -u NONE -l <this file>.
-- Synthetic target benchmark: no Agent, network, payload capture or dependencies.
---@diagnostic disable-next-line: undefined-global -- Neovim injects its runtime API.
local nvim = vim
nvim.opt.rtp:prepend(nvim.fn.getcwd())
local Transcript = require("louiselm.session.transcript")
local ChatBuffer = require("louiselm.ui.chat.buffer")
local warmups, samples = 2, 9

local function noop() end

local function summarize(values)
  local sorted = nvim.list_extend({}, values)
  table.sort(sorted)
  return { median = sorted[5], min = sorted[1], max = sorted[9], spread = sorted[9] - sorted[1] }
end

local results = {}
for _, bytes in ipairs({ 8192, 65536, 262144 }) do
  local events, chunks = {}, {}
  for index = 1, bytes / 32 do
    -- Unique chunks avoid measuring repeated-string interning; a newline every
    -- four chunks bounds the line-rewrite cost independently of response length.
    local text = string.format("%08d %s%s", index, string.rep("x", 22), index % 4 == 0 and "\n" or " ")
    chunks[index] = text
    events[index] = { type = "chunk", session_id = "bench", data = { text = text } }
  end
  local expected = table.concat(chunks)
  assert(#expected == bytes)
  for _, mode in ipairs({ "recorder", "renderer", "scheduled_pair" }) do
    local times, heaps = {}, {}
    for sample = 1, warmups + samples do
      local transcript = mode ~= "renderer" and Transcript.new() or nil
      local renderer
      if mode ~= "recorder" then
        renderer = ChatBuffer.new({
          id = "bench",
          name = "bench",
          source = "new",
          agent = "synthetic",
          status = "ready",
          working_dir = nvim.fn.getcwd(),
          current_turn = 1,
          turn_options_changed = false,
          recording_pending = false,
          config_options = {},
          commands = {},
          compactions = {},
          skills_policy = "off",
          embedded_context = false,
        }, {
          markdown_highlighting = false,
          on_prompt_edit = noop,
          prompt_prefix = function()
            return ""
          end,
          on_enter = noop,
          submit = noop,
        })
        renderer:show(nvim.api.nvim_get_current_win(), false)
      end
      collectgarbage("collect")
      local initial_heap = collectgarbage("count")
      local peak_heap = initial_heap
      local started = nvim.uv.hrtime()
      for index, event in ipairs(events) do
        if transcript ~= nil then
          transcript:record(event)
        end
        if renderer ~= nil then
          if mode == "scheduled_pair" then
            nvim.schedule(function()
              renderer:reconcile()
              renderer:render(event)
            end)
          else
            renderer:reconcile()
            renderer:render(event)
          end
        end
        if index % 128 == 0 then
          if mode == "scheduled_pair" then
            local drained = false
            nvim.schedule(function()
              drained = true
            end)
            assert(nvim.wait(10000, function()
              return drained
            end, 1))
          end
          peak_heap = math.max(peak_heap, collectgarbage("count"))
        end
      end
      if renderer ~= nil then
        -- Include deferred terminal-completion matching in the measured work.
        renderer:render({
          type = "tool_call_finished",
          session_id = "bench",
          data = {
            toolCallId = "completion",
            title = "task_complete",
            status = "completed",
            rawOutput = { content = expected },
          },
        })
        renderer:finish_turn()
      end
      local snapshot = transcript and transcript:snapshot() or nil
      peak_heap = math.max(peak_heap, collectgarbage("count"))
      local elapsed = (nvim.uv.hrtime() - started) / 1e6
      if snapshot ~= nil then
        assert(#snapshot == 1 and snapshot[1].text == expected)
      end
      if renderer ~= nil then
        local rendered = table.concat(nvim.api.nvim_buf_get_lines(renderer.buffer, 0, -1, false), "\n")
        local first, last = rendered:find(expected, 1, true)
        assert(first ~= nil and last ~= nil and rendered:find(expected, last + 1, true) == nil)
        renderer:dispose()
      end
      if sample > warmups then
        times[#times + 1] = elapsed
        heaps[#heaps + 1] = peak_heap - initial_heap
      end
    end
    results[#results + 1] = {
      mode = mode,
      bytes = bytes,
      chunks = #events,
      milliseconds = times,
      sampled_peak_lua_kib = heaps,
      time = summarize(times),
      heap = summarize(heaps),
    }
  end
end
io.write(nvim.json.encode({
  neovim = nvim.version(),
  warmups = warmups,
  samples = samples,
  chunk_bytes = 32,
  scheduled_batch = 128,
  results = results,
}) .. "\n")
