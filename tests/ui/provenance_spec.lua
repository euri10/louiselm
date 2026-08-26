local MiniTest = require("mini.test")
local Provenance = require("louiselm.ui.provenance")

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim
local original_schedule = nvim.schedule
local original_system = nvim.system
local source_buffers = {}

local T = MiniTest.new_set({
  hooks = {
    post_case = function()
      nvim.schedule = original_schedule
      rawset(nvim, "system", original_system)
      for _, buffer in ipairs(nvim.api.nvim_list_bufs()) do
        if nvim.api.nvim_buf_get_name(buffer):match("^louiselm://provenance/commit/") then
          nvim.api.nvim_buf_delete(buffer, { force = true })
        end
      end
      for _, buffer in ipairs(source_buffers) do
        if nvim.api.nvim_buf_is_valid(buffer) then
          nvim.api.nvim_buf_delete(buffer, { force = true })
        end
      end
      source_buffers = {}
    end,
  },
})

---@param line string
---@param column integer
---@return integer buffer
local function source_buffer(line, column)
  local buffer = nvim.api.nvim_create_buf(false, true)
  source_buffers[#source_buffers + 1] = buffer
  nvim.api.nvim_buf_set_lines(buffer, 0, -1, false, { line })
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

---@param calls table[]
---@param message string
local function finish(calls, message)
  local scheduled = {}
  nvim.schedule = function(callback)
    scheduled[#scheduled + 1] = callback
  end
  calls[1].on_exit({
    code = 0,
    stdout = "0123456789abcdef0123456789abcdef01234567" .. string.char(0) .. message .. string.char(0) .. string.char(
      30
    ),
    stderr = "",
  })
  MiniTest.expect.equality(#scheduled, 1)
  scheduled[1]()
end

T["commit Provenance"] = MiniTest.new_set()

T["commit Provenance"]["renders commit references and unresolved Sessions"] = function()
  local buffer = source_buffer("commit 0123456789abcdef0123456789abcdef01234567", 10)
  local calls = fake_system()

  assert(Provenance.inspect(buffer, { definitions = {} }))
  MiniTest.expect.equality(calls[1].command, {
    "git",
    "log",
    "--no-decorate",
    "--no-color",
    "--format=%H%x00%B%x00%x1e",
    "0123456789abcdef0123456789abcdef01234567^..0123456789abcdef0123456789abcdef01234567",
  })
  finish(calls, "fix: preserve intent\n\nRefs louiselm-kpod\nRefs codex/session-123\n")

  local popup = nvim.api.nvim_get_current_buf()
  MiniTest.expect.equality(
    nvim.api.nvim_buf_get_name(popup),
    "louiselm://provenance/commit/0123456789abcdef0123456789abcdef01234567"
  )
  MiniTest.expect.equality(nvim.api.nvim_buf_get_lines(popup, 0, -1, false), {
    "# fix: preserve intent",
    "",
    "Commit: 0123456789abcdef0123456789abcdef01234567",
    "",
    "Issues served:",
    "- louiselm-kpod",
    "",
    "Sessions:",
    "- codex/session-123 → unresolved (no transcript layout is configured)",
  })
  MiniTest.expect.equality(nvim.api.nvim_get_option_value("modifiable", { buf = popup }), false)
end

T["commit Provenance"]["renders a partial answer when the commit has no trailers"] = function()
  local buffer = source_buffer("0123456789abcdef0123456789abcdef01234567", 10)
  local calls = fake_system()
  assert(Provenance.inspect(buffer))
  finish(calls, "chore: tidy history\n")

  MiniTest.expect.equality(nvim.api.nvim_buf_get_lines(nvim.api.nvim_get_current_buf(), 0, -1, false), {
    "# chore: tidy history",
    "",
    "Commit: 0123456789abcdef0123456789abcdef01234567",
    "",
    "Issues served:",
    "- none recorded",
    "",
    "Sessions:",
    "- none recorded",
  })
end

T["commit Provenance"]["ignores a scheduled result after the view becomes inactive"] = function()
  local buffer = source_buffer("0123456789abcdef0123456789abcdef01234567", 10)
  local calls = fake_system()
  local scheduled = {}
  nvim.schedule = function(callback)
    scheduled[#scheduled + 1] = callback
  end

  assert(Provenance.inspect(buffer, {
    is_active = function()
      return false
    end,
  }))
  calls[1].on_exit({ code = 0, stdout = "", stderr = "" })
  MiniTest.expect.equality(#scheduled, 1)
  scheduled[1]()
  MiniTest.expect.equality(#nvim.api.nvim_list_bufs(), #source_buffers + 1)
end

return T
