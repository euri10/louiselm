local MiniTest = require("mini.test")
local Beads = require("louiselm.ui.beads")
local Command = require("louiselm.ui.chat.command")

local T = MiniTest.new_set()

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim
local original_executable = nvim.fn.executable
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
  rawset(nvim, "system", function(command, options, on_exit)
    calls[#calls + 1] = { command = command, options = options, on_exit = on_exit }
    return {}
  end)
  return calls
end

T["beads"] = MiniTest.new_set({
  hooks = {
    post_case = function()
      nvim.fn.executable = original_executable
      nvim.ui.input = original_input
      nvim.schedule = original_schedule
      rawset(nvim, "system", original_system)
      delete_test_buffers()
    end,
  },
})

T["beads"]["opens the Beads issue under the cursor after the process callback is scheduled"] = function()
  local buffer = source_buffer({ "Fix louiselm-kpod today" }, 8)
  local calls = fake_system()
  local scheduled = {}
  nvim.schedule = function(callback)
    scheduled[#scheduled + 1] = callback
  end

  assert(Beads.inspect(buffer))

  MiniTest.expect.equality(calls[1].command, { "br", "show", "louiselm-kpod", "--json" })
  MiniTest.expect.equality(calls[1].options, { text = true, cwd = nvim.fn.getcwd() })
  calls[1].on_exit({
    code = 0,
    signal = 0,
    stdout = '[{"id":"louiselm-kpod","title":"Usage spacing","status":"open","priority":2,"labels":["ui"],"description":"Add blank lines."}]',
    stderr = "",
  })

  MiniTest.expect.equality(nvim.api.nvim_get_current_buf(), buffer)
  MiniTest.expect.equality(#scheduled, 1)
  scheduled[1]()

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

T["beads"]["opens an issue whose JSON omits the labels key entirely"] = function()
  -- `br show --json` omits `labels` rather than emitting `[]` when an issue
  -- has none (louiselm-t6j2) -- a missing key must not be treated as malformed.
  local buffer = source_buffer({ "Fix louiselm-kpod today" }, 8)
  local calls = fake_system()
  local scheduled = {}
  nvim.schedule = function(callback)
    scheduled[#scheduled + 1] = callback
  end

  assert(Beads.inspect(buffer))
  calls[1].on_exit({
    code = 0,
    signal = 0,
    stdout = '[{"id":"louiselm-kpod","title":"Usage spacing","status":"open","priority":2,"description":"Add blank lines."}]',
    stderr = "",
  })
  scheduled[1]()

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

T["beads"]["prompts for a Beads issue ID when the cursor has none"] = function()
  local buffer = source_buffer({ "No issue here" }, 0)
  local calls = fake_system()
  nvim.ui.input = function(options, callback)
    MiniTest.expect.equality(options.prompt, "louiselm Beads issue id: ")
    callback("louiselm-zmab")
  end

  assert(Beads.inspect(buffer))

  MiniTest.expect.equality(calls[1].command, { "br", "show", "louiselm-zmab", "--json" })
end

T["beads"]["prompts instead of guessing when one line contains multiple Beads issue IDs"] = function()
  local buffer = source_buffer({ "Compare louiselm-kpod with louiselm-zmab" }, 10)
  local calls = fake_system()
  nvim.ui.input = function(_, callback)
    callback("louiselm-zmab")
  end

  assert(Beads.inspect(buffer))

  MiniTest.expect.equality(calls[1].command, { "br", "show", "louiselm-zmab", "--json" })
end

T["beads"]["rejects an invalid prompted ID without starting br"] = function()
  local buffer = source_buffer({ "No issue here" }, 0)
  local calls = fake_system()
  local error_message
  nvim.ui.input = function(_, callback)
    callback("codex/session-123")
  end

  assert(Beads.inspect(buffer, {
    on_error = function(message)
      error_message = message
    end,
  }))

  MiniTest.expect.equality(#calls, 0)
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
  nvim.schedule = function(callback)
    scheduled[#scheduled + 1] = callback
  end

  assert(Beads.inspect(buffer, {
    on_error = function(message)
      error_message = message
    end,
  }))
  calls[1].on_exit({ code = 0, signal = 0, stdout = "not json", stderr = "" })
  scheduled[1]()

  MiniTest.expect.equality(error_message, "br returned malformed issue data")
  MiniTest.expect.equality(nvim.api.nvim_get_current_buf(), buffer)
end

T["beads"]["reports a failed br lookup without opening a popup"] = function()
  local buffer = source_buffer({ "Fix louiselm-kpod today" }, 8)
  local calls = fake_system()
  local scheduled = {}
  local error_message
  nvim.schedule = function(callback)
    scheduled[#scheduled + 1] = callback
  end

  assert(Beads.inspect(buffer, {
    on_error = function(message)
      error_message = message
    end,
  }))
  calls[1].on_exit({ code = 1, signal = 0, stdout = "", stderr = "not found" })
  scheduled[1]()

  MiniTest.expect.equality(error_message, "could not read Beads issue louiselm-kpod")
  MiniTest.expect.equality(nvim.api.nvim_get_current_buf(), buffer)
end

T["beads"]["installs the inspector map for chat buffers"] = function()
  Command.register()
  local buffer = nvim.api.nvim_create_buf(false, true)
  source_buffers[#source_buffers + 1] = buffer
  nvim.api.nvim_set_option_value("filetype", "louiselm-session", { buf = buffer })

  local mapping
  for _, value in ipairs(nvim.api.nvim_buf_get_keymap(buffer, "n")) do
    if value.desc == "Inspect Beads issue" then
      mapping = value
      break
    end
  end
  MiniTest.expect.equality(mapping ~= nil, true)
end

return T
