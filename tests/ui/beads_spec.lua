local MiniTest = require("mini.test")
local Beads = require("louiselm.ui.beads")

local T = MiniTest.new_set()

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim
local original_executable = nvim.fn.executable
local original_expand = nvim.fn.expand
local original_glob = nvim.fn.glob
local original_input = nvim.ui.input
local original_schedule = nvim.schedule
local original_system = nvim.system
local source_buffers = {}

local function delete_test_buffers()
  for _, buffer in ipairs(nvim.api.nvim_list_bufs()) do
    local name = nvim.api.nvim_buf_get_name(buffer)
    if name:match("^louiselm://beads/") then
      nvim.api.nvim_buf_delete(buffer, { force = true })
    end
  end
  for _, buffer in ipairs(source_buffers) do
    if nvim.api.nvim_buf_is_valid(buffer) then
      nvim.api.nvim_buf_delete(buffer, { force = true })
    end
  end
  source_buffers = {}
end

---@param name string
---@return integer? buffer
local function find_buffer(name)
  for _, buffer in ipairs(nvim.api.nvim_list_bufs()) do
    if nvim.api.nvim_buf_get_name(buffer) == name then
      return buffer
    end
  end
end

---@param lines string[]
---@param column integer Zero-based cursor column.
---@return integer buffer
local function source_buffer(lines, column)
  local buffer = nvim.api.nvim_create_buf(false, true)
  source_buffers[#source_buffers + 1] = buffer
  nvim.api.nvim_set_option_value("filetype", "louiselm-session", { buf = buffer })
  nvim.api.nvim_buf_set_lines(buffer, 0, -1, false, lines)
  nvim.api.nvim_set_current_buf(buffer)
  nvim.api.nvim_win_set_cursor(0, { 1, column })
  return buffer
end

---@return table[] calls
local function fake_system()
  local calls = {}
  nvim.fn.executable = function()
    return 1
  end
  rawset(nvim, "system", function(command, options, on_exit)
    calls[#calls + 1] = { command = command, options = options, on_exit = on_exit }
    return {}
  end)
  return calls
end

local function complete_where(calls, scheduled, prefix)
  calls[1].on_exit({ code = 0, signal = 0, stdout = '{"prefix":"' .. prefix .. '"}', stderr = "" })
  scheduled[1]()
end

---Fake sibling `.beads/beads.db` discovery: `db_paths` is returned verbatim
---for every glob call, regardless of the root pattern requested.
---@param db_paths string[]
local function fake_glob(db_paths)
  nvim.fn.expand = function(value)
    return value
  end
  nvim.fn.glob = function(_, _, list)
    if list then
      return db_paths
    end
    return table.concat(db_paths, "\n")
  end
end

T["beads"] = MiniTest.new_set({
  hooks = {
    post_case = function()
      nvim.fn.executable = original_executable
      nvim.fn.expand = original_expand
      nvim.fn.glob = original_glob
      nvim.ui.input = original_input
      rawset(nvim, "schedule", original_schedule)
      rawset(nvim, "system", original_system)
      delete_test_buffers()
    end,
  },
})

T["beads"]["opens the Beads issue under the cursor after the process callback is scheduled"] = function()
  local buffer = source_buffer({ "Fix louiselm-kpod today" }, 8)
  local calls = fake_system()
  local scheduled = {}
  rawset(nvim, "schedule", function(callback)
    scheduled[#scheduled + 1] = callback
  end)

  assert(Beads.inspect(buffer))

  MiniTest.expect.equality(calls[1].command, { "br", "where", "--json" })
  complete_where(calls, scheduled, "louiselm")
  MiniTest.expect.equality(calls[2].command, { "br", "show", "louiselm-kpod", "--json" })
  MiniTest.expect.equality(calls[2].options, { text = true, cwd = nvim.fn.getcwd() })
  calls[2].on_exit({
    code = 0,
    signal = 0,
    stdout = '[{"id":"louiselm-kpod","title":"Usage spacing","status":"open","priority":2,"labels":["ui"],"description":"Add blank lines."}]',
    stderr = "",
  })

  MiniTest.expect.equality(nvim.api.nvim_get_current_buf(), buffer)
  MiniTest.expect.equality(#scheduled, 2)
  scheduled[2]()

  local popup = assert(find_buffer("louiselm://beads/louiselm-kpod"))
  MiniTest.expect.equality(nvim.api.nvim_buf_get_lines(popup, 0, -1, false), {
    "# Usage spacing",
    "",
    "ID: louiselm-kpod",
    "Status: open",
    "Priority: 2",
    "Labels: ui",
    "",
    "Add blank lines.",
  })
  MiniTest.expect.equality(nvim.api.nvim_get_option_value("modifiable", { buf = popup }), false)
  local close_mapping
  for _, value in ipairs(nvim.api.nvim_buf_get_keymap(popup, "n")) do
    if value.desc == "Close Beads issue" then
      close_mapping = value
      break
    end
  end
  MiniTest.expect.equality(close_mapping ~= nil, true)
end

T["beads"]["uses the Beads workspace prefix for issue lookup"] = function()
  local buffer = source_buffer({ "Fix daa-nk1l today" }, 8)
  local calls = fake_system()
  local scheduled = {}
  rawset(nvim, "schedule", function(callback)
    scheduled[#scheduled + 1] = callback
  end)

  assert(Beads.inspect(buffer, { cwd = "/home/lotso/code/acp-llm-adapter" }))

  MiniTest.expect.equality(calls[1].command, { "br", "where", "--json" })
  calls[1].on_exit({
    code = 0,
    signal = 0,
    stdout = '{"path":"/home/lotso/code/acp-llm-adapter/.beads","prefix":"daa"}',
    stderr = "",
  })
  scheduled[1]()

  MiniTest.expect.equality(calls[2].command, { "br", "show", "daa-nk1l", "--json" })
  calls[2].on_exit({
    code = 0,
    signal = 0,
    stdout = '[{"id":"daa-nk1l","title":"Upgrade ACP","status":"open","priority":2,"description":"Migrate the protocol."}]',
    stderr = "",
  })
  scheduled[2]()

  MiniTest.expect.equality(find_buffer("louiselm://beads/daa-nk1l") ~= nil, true)
end

T["beads"]["opens an issue whose JSON omits the labels key entirely"] = function()
  -- `br show --json` omits `labels` rather than emitting `[]` when an issue
  -- has none (louiselm-t6j2) -- a missing key must not be treated as malformed.
  local buffer = source_buffer({ "Fix louiselm-kpod today" }, 8)
  local calls = fake_system()
  local scheduled = {}
  rawset(nvim, "schedule", function(callback)
    scheduled[#scheduled + 1] = callback
  end)

  assert(Beads.inspect(buffer))
  complete_where(calls, scheduled, "louiselm")
  calls[2].on_exit({
    code = 0,
    signal = 0,
    stdout = '[{"id":"louiselm-kpod","title":"Usage spacing","status":"open","priority":2,"description":"Add blank lines."}]',
    stderr = "",
  })
  scheduled[2]()

  local popup = assert(find_buffer("louiselm://beads/louiselm-kpod"))
  MiniTest.expect.equality(nvim.api.nvim_buf_get_lines(popup, 0, -1, false), {
    "# Usage spacing",
    "",
    "ID: louiselm-kpod",
    "Status: open",
    "Priority: 2",
    "Labels: none",
    "",
    "Add blank lines.",
  })
end

T["beads"]["opens an issue whose JSON omits the description key entirely (louiselm-kk27)"] = function()
  -- `br show --json` omits `description` rather than emitting `""` when an
  -- issue has none (e.g. a closed fixture) -- a missing key must not be
  -- treated as malformed, mirroring the `labels` handling above.
  local buffer = source_buffer({ "Fix louiselm-qced today" }, 8)
  local calls = fake_system()
  local scheduled = {}
  rawset(nvim, "schedule", function(callback)
    scheduled[#scheduled + 1] = callback
  end)

  assert(Beads.inspect(buffer))
  complete_where(calls, scheduled, "louiselm")
  calls[2].on_exit({
    code = 0,
    signal = 0,
    stdout = '[{"id":"louiselm-qced","title":"Disposable fixture","status":"closed","priority":4}]',
    stderr = "",
  })
  scheduled[2]()

  local popup = assert(find_buffer("louiselm://beads/louiselm-qced"))
  MiniTest.expect.equality(nvim.api.nvim_buf_get_lines(popup, 0, -1, false), {
    "# Disposable fixture",
    "",
    "ID: louiselm-qced",
    "Status: closed",
    "Priority: 4",
    "Labels: none",
    "",
    "",
  })
end

T["beads"]["prompts for a Beads issue ID when the cursor has none"] = function()
  local buffer = source_buffer({ "No issue here" }, 0)
  local calls = fake_system()
  local scheduled = {}
  rawset(nvim, "schedule", function(callback)
    scheduled[#scheduled + 1] = callback
  end)
  nvim.ui.input = function(options, callback)
    MiniTest.expect.equality(options.prompt, "louiselm Beads issue id: ")
    callback("louiselm-zmab")
  end

  assert(Beads.inspect(buffer))
  complete_where(calls, scheduled, "louiselm")

  MiniTest.expect.equality(calls[2].command, { "br", "show", "louiselm-zmab", "--json" })
end

T["beads"]["prepends the workspace prefix to a bare id typed at the prompt"] = function()
  local buffer = source_buffer({ "No issue here" }, 0)
  local calls = fake_system()
  local scheduled = {}
  rawset(nvim, "schedule", function(callback)
    scheduled[#scheduled + 1] = callback
  end)
  nvim.ui.input = function(_, callback)
    callback("zmab")
  end

  assert(Beads.inspect(buffer, { cwd = "/home/lotso/code/acp-llm-adapter" }))
  complete_where(calls, scheduled, "daa")

  MiniTest.expect.equality(calls[2].command, { "br", "show", "daa-zmab", "--json" })
end

T["beads"]["prompts instead of guessing when one line contains multiple Beads issue IDs"] = function()
  local buffer = source_buffer({ "Compare louiselm-kpod with louiselm-zmab" }, 10)
  local calls = fake_system()
  local scheduled = {}
  rawset(nvim, "schedule", function(callback)
    scheduled[#scheduled + 1] = callback
  end)
  nvim.ui.input = function(_, callback)
    callback("louiselm-zmab")
  end

  assert(Beads.inspect(buffer))
  complete_where(calls, scheduled, "louiselm")

  MiniTest.expect.equality(calls[2].command, { "br", "show", "louiselm-zmab", "--json" })
end

T["beads"]["opens the cursor issue directly when the line also contains ordinary hyphenated words (louiselm-04cy)"] = function()
  -- A prefix-agnostic matcher previously counted plain hyphenated English
  -- compounds ("time-travel", "vocabulary-drop") as candidate issue IDs,
  -- inflating the ambiguity count so a line with exactly one real issue ID
  -- still fell back to the manual prompt.
  local buffer = source_buffer({
    "still open is louiselm-f8r1, the same time-travel vocabulary-drop bug",
  }, 14)
  local calls = fake_system()
  local scheduled = {}
  rawset(nvim, "schedule", function(callback)
    scheduled[#scheduled + 1] = callback
  end)
  nvim.ui.input = function()
    error("must not prompt when the cursor is on the sole real issue ID")
  end

  assert(Beads.inspect(buffer))
  complete_where(calls, scheduled, "louiselm")

  MiniTest.expect.equality(calls[2].command, { "br", "show", "louiselm-f8r1", "--json" })
end

T["beads"]["opens the local issue when the cursor is on a bare suffix with no prefix"] = function()
  local buffer = source_buffer({ "look at 04cy please" }, 9)
  local calls = fake_system()
  local scheduled = {}
  rawset(nvim, "schedule", function(callback)
    scheduled[#scheduled + 1] = callback
  end)
  nvim.ui.input = function()
    error("must not prompt when a bare-suffix guess resolves")
  end

  assert(Beads.inspect(buffer))
  complete_where(calls, scheduled, "louiselm")

  MiniTest.expect.equality(calls[2].command, { "br", "show", "louiselm-04cy", "--json" })
  calls[2].on_exit({
    code = 0,
    signal = 0,
    stdout = '[{"id":"louiselm-04cy","title":"Bare suffix","status":"open","priority":2,"description":"desc"}]',
    stderr = "",
  })
  scheduled[2]()

  MiniTest.expect.equality(find_buffer("louiselm://beads/louiselm-04cy") ~= nil, true)
end

T["beads"]["falls back to the manual prompt when a bare-suffix guess does not resolve"] = function()
  local buffer = source_buffer({ "look at nope please" }, 9)
  local calls = fake_system()
  local scheduled = {}
  rawset(nvim, "schedule", function(callback)
    scheduled[#scheduled + 1] = callback
  end)
  local prompted
  nvim.ui.input = function(options, callback)
    prompted = options.prompt
    callback("louiselm-zmab")
  end

  assert(Beads.inspect(buffer))
  complete_where(calls, scheduled, "louiselm")

  MiniTest.expect.equality(calls[2].command, { "br", "show", "louiselm-nope", "--json" })
  calls[2].on_exit({ code = 1, signal = 0, stdout = "", stderr = "not found" })
  scheduled[2]()

  MiniTest.expect.equality(prompted, "louiselm Beads issue id: ")
  MiniTest.expect.equality(calls[3].command, { "br", "show", "louiselm-zmab", "--json" })
end

for _, entry in ipairs({ "bare cursor", "prefixed cursor", "manual prompt" }) do
  T["beads"]["opens the canonical slugged issue from a short reference: " .. entry] = function()
    local line = entry == "bare cursor" and "w1ez" or "louiselm-w1ez"
    local buffer = source_buffer({ entry == "manual prompt" and "" or line }, 0)
    local calls = fake_system()
    local scheduled = {}
    local errors = {}
    local prompts = 0
    rawset(nvim, "schedule", function(callback)
      scheduled[#scheduled + 1] = callback
    end)
    nvim.ui.input = function(_, callback)
      prompts = prompts + 1
      if entry == "manual prompt" and prompts == 1 then
        callback("w1ez")
      end
    end

    assert(Beads.inspect(buffer, {
      on_error = function(message)
        errors[#errors + 1] = message
      end,
    }))
    complete_where(calls, scheduled, "louiselm")
    MiniTest.expect.equality(calls[2].command, { "br", "show", "louiselm-w1ez", "--json" })

    -- Captured from br 0.5.10 `show louiselm-w1ez --json`, 2026-09-06,
    -- codex/01a07709-0512-7a31-a9ec-9770ffa13fba (louiselm-6dpl).
    -- Omit unconsumed fields; preserve the requested/resolved ID difference.
    local callback_was_fast
    local timer = assert(nvim.uv.new_timer())
    timer:start(0, 0, function()
      timer:stop()
      timer:close()
      callback_was_fast = nvim.in_fast_event()
      calls[2].on_exit({
        code = 0,
        signal = 0,
        stdout = '[{"id":"louiselm-relay-failure-terminal-receipt-w1ez","title":"Record a causal terminal receipt for relay failure","status":"in_progress","priority":2,"labels":["rust","security"]}]',
        stderr = "",
      })
    end)
    assert(nvim.wait(1000, function()
      return #scheduled == 2
    end))
    MiniTest.expect.equality(callback_was_fast, true)
    MiniTest.expect.equality(nvim.api.nvim_get_current_buf(), buffer)
    scheduled[2]()

    MiniTest.expect.equality(errors, {})
    MiniTest.expect.equality(prompts, entry == "manual prompt" and 1 or 0)
    local popup = assert(find_buffer("louiselm://beads/louiselm-relay-failure-terminal-receipt-w1ez"))
    MiniTest.expect.equality(nvim.api.nvim_get_current_buf(), popup)
    MiniTest.expect.equality(
      nvim.api.nvim_buf_get_lines(popup, 2, 3, false),
      { "ID: louiselm-relay-failure-terminal-receipt-w1ez" }
    )
  end
end

for _, returned_id in ipairs({
  "louiselm-unrelated-abcd",
  "foreign-relay-w1ez",
  "louiselm-relay-aw1ez",
  "louiselm-relay-w1ez1",
  "louiselm-relay-w1ez.1",
}) do
  T["beads"]["rejects an unrelated canonical issue: " .. returned_id] = function()
    local buffer = source_buffer({ "louiselm-w1ez" }, 0)
    local calls = fake_system()
    local scheduled = {}
    local error_message
    rawset(nvim, "schedule", function(callback)
      scheduled[#scheduled + 1] = callback
    end)

    assert(Beads.inspect(buffer, {
      on_error = function(message)
        error_message = message
      end,
    }))
    complete_where(calls, scheduled, "louiselm")
    calls[2].on_exit({
      code = 0,
      signal = 0,
      stdout = nvim.json.encode({ { id = returned_id, title = "Unrelated", status = "open", priority = 2 } }),
      stderr = "",
    })
    scheduled[2]()

    MiniTest.expect.equality(error_message, "br returned malformed issue data")
    MiniTest.expect.equality(nvim.api.nvim_get_current_buf(), buffer)
    MiniTest.expect.equality(find_buffer("louiselm://beads/" .. returned_id), nil)
  end
end

T["beads"]["ignores a resolved short reference queued before chat disposal"] = function()
  local buffer = source_buffer({ "louiselm-w1ez" }, 0)
  local calls = fake_system()
  local scheduled = {}
  local active = true
  rawset(nvim, "schedule", function(callback)
    scheduled[#scheduled + 1] = callback
  end)

  assert(Beads.inspect(buffer, {
    is_active = function()
      return active
    end,
  }))
  complete_where(calls, scheduled, "louiselm")
  calls[2].on_exit({
    code = 0,
    signal = 0,
    stdout = '[{"id":"louiselm-relay-failure-terminal-receipt-w1ez","title":"Relay failure","status":"in_progress","priority":2}]',
    stderr = "",
  })
  active = false
  scheduled[2]()

  MiniTest.expect.equality(nvim.api.nvim_get_current_buf(), buffer)
  MiniTest.expect.equality(find_buffer("louiselm://beads/louiselm-relay-failure-terminal-receipt-w1ez"), nil)
end

T["beads"]["does not guess a bare suffix for a token that is already a full prefixed ID"] = function()
  -- Ambiguous multi-ID lines must keep prompting, not silently retry the
  -- cursor's own full ID as if it were a bare suffix (which would double it
  -- into "<prefix>-<prefix>-<suffix>").
  local buffer = source_buffer({ "Compare louiselm-kpod with louiselm-zmab" }, 10)
  local calls = fake_system()
  local scheduled = {}
  rawset(nvim, "schedule", function(callback)
    scheduled[#scheduled + 1] = callback
  end)
  nvim.ui.input = function(_, callback)
    callback("louiselm-zmab")
  end

  assert(Beads.inspect(buffer))
  complete_where(calls, scheduled, "louiselm")

  MiniTest.expect.equality(calls[2].command, { "br", "show", "louiselm-zmab", "--json" })
end

T["beads"]["sweeps configured sibling workspaces for a foreign-prefixed cursor ID"] = function()
  local buffer = source_buffer({ "see codex-acp-a1b2 for details" }, 5)
  local calls = fake_system()
  fake_glob({ "/home/lotso/code/codex-acp/.beads/beads.db", "/home/lotso/code/other/.beads/beads.db" })
  local scheduled = {}
  rawset(nvim, "schedule", function(callback)
    scheduled[#scheduled + 1] = callback
  end)
  nvim.ui.input = function()
    error("must not prompt when a sibling resolves the ID")
  end

  assert(Beads.inspect(buffer, { sibling_roots = { "~/code" } }))
  complete_where(calls, scheduled, "louiselm")

  MiniTest.expect.equality(
    calls[2].command,
    { "br", "--db", "/home/lotso/code/codex-acp/.beads/beads.db", "show", "codex-acp-a1b2", "--json" }
  )
  calls[2].on_exit({ code = 1, signal = 0, stdout = "", stderr = "not found" })
  scheduled[2]()

  MiniTest.expect.equality(
    calls[3].command,
    { "br", "--db", "/home/lotso/code/other/.beads/beads.db", "show", "codex-acp-a1b2", "--json" }
  )
  calls[3].on_exit({
    code = 0,
    signal = 0,
    stdout = '[{"id":"codex-acp-a1b2","title":"Cross repo","status":"open","priority":2,"description":"desc"}]',
    stderr = "",
  })
  scheduled[3]()

  MiniTest.expect.equality(find_buffer("louiselm://beads/codex-acp-a1b2") ~= nil, true)
end

T["beads"]["falls back to the manual prompt when no sibling resolves the foreign ID"] = function()
  local buffer = source_buffer({ "see codex-acp-a1b2 for details" }, 5)
  local calls = fake_system()
  fake_glob({ "/home/lotso/code/codex-acp/.beads/beads.db" })
  local scheduled = {}
  rawset(nvim, "schedule", function(callback)
    scheduled[#scheduled + 1] = callback
  end)
  local prompted
  nvim.ui.input = function(options, callback)
    prompted = options.prompt
    callback("louiselm-zmab")
  end

  assert(Beads.inspect(buffer, { sibling_roots = { "~/code" } }))
  complete_where(calls, scheduled, "louiselm")

  MiniTest.expect.equality(
    calls[2].command,
    { "br", "--db", "/home/lotso/code/codex-acp/.beads/beads.db", "show", "codex-acp-a1b2", "--json" }
  )
  calls[2].on_exit({ code = 1, signal = 0, stdout = "", stderr = "not found" })
  scheduled[2]()

  MiniTest.expect.equality(prompted, "louiselm Beads issue id: ")
  MiniTest.expect.equality(calls[3].command, { "br", "show", "louiselm-zmab", "--json" })
end

T["beads"]["does not sweep siblings when sibling_roots is unset"] = function()
  local buffer = source_buffer({ "see codex-acp-a1b2 for details" }, 5)
  local calls = fake_system()
  local scheduled = {}
  rawset(nvim, "schedule", function(callback)
    scheduled[#scheduled + 1] = callback
  end)
  nvim.ui.input = function(_, callback)
    callback("louiselm-zmab")
  end

  assert(Beads.inspect(buffer))
  complete_where(calls, scheduled, "louiselm")

  MiniTest.expect.equality(#calls, 2)
  MiniTest.expect.equality(calls[2].command, { "br", "show", "louiselm-zmab", "--json" })
end

T["beads"]["rejects an invalid prompted ID without starting br"] = function()
  local buffer = source_buffer({ "No issue here" }, 0)
  local calls = fake_system()
  local scheduled = {}
  rawset(nvim, "schedule", function(callback)
    scheduled[#scheduled + 1] = callback
  end)
  local error_message
  nvim.ui.input = function(_, callback)
    callback("codex/session-123")
  end

  assert(Beads.inspect(buffer, {
    on_error = function(message)
      error_message = message
    end,
  }))
  complete_where(calls, scheduled, "louiselm")

  MiniTest.expect.equality(#calls, 1)
  MiniTest.expect.equality(error_message, "Beads issue id must start with louiselm-")
end

T["beads"]["reports when br is unavailable without starting a lookup"] = function()
  local buffer = source_buffer({ "Fix louiselm-kpod today" }, 8)
  local calls = fake_system()
  local error_message
  nvim.fn.executable = function()
    return 0
  end

  local started, inspect_error = Beads.inspect(buffer, {
    on_error = function(message)
      error_message = message
    end,
  })

  MiniTest.expect.equality(started, false)
  MiniTest.expect.equality(inspect_error, "br is not available")
  MiniTest.expect.equality(#calls, 0)
  MiniTest.expect.equality(error_message, nil)
end

T["beads"]["reports malformed br JSON without opening a popup"] = function()
  local buffer = source_buffer({ "Fix louiselm-kpod today" }, 8)
  local calls = fake_system()
  local scheduled = {}
  local error_message
  rawset(nvim, "schedule", function(callback)
    scheduled[#scheduled + 1] = callback
  end)

  assert(Beads.inspect(buffer, {
    on_error = function(message)
      error_message = message
    end,
  }))
  complete_where(calls, scheduled, "louiselm")
  calls[2].on_exit({ code = 0, signal = 0, stdout = "not json", stderr = "" })
  scheduled[2]()

  MiniTest.expect.equality(error_message, "br returned malformed issue data")
  MiniTest.expect.equality(nvim.api.nvim_get_current_buf(), buffer)
end

T["beads"]["reports a failed br lookup without opening a popup"] = function()
  local buffer = source_buffer({ "Fix louiselm-kpod today" }, 8)
  local calls = fake_system()
  local scheduled = {}
  local error_message
  rawset(nvim, "schedule", function(callback)
    scheduled[#scheduled + 1] = callback
  end)

  assert(Beads.inspect(buffer, {
    on_error = function(message)
      error_message = message
    end,
  }))
  complete_where(calls, scheduled, "louiselm")
  calls[2].on_exit({ code = 1, signal = 0, stdout = "", stderr = "not found" })
  scheduled[2]()

  MiniTest.expect.equality(error_message, "could not read Beads issue louiselm-kpod")
  MiniTest.expect.equality(nvim.api.nvim_get_current_buf(), buffer)
end

return T
