local MiniTest = require("mini.test")
local Diagnostics = require("louiselm.ui.context.diagnostics")

local T = MiniTest.new_set()

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local ns = nvim.api.nvim_create_namespace("louiselm_diagnostics_spec")

local function new_buffer(name)
  local buffer = nvim.api.nvim_create_buf(false, true)
  if name then
    nvim.api.nvim_buf_set_name(buffer, name)
  end
  nvim.api.nvim_buf_set_lines(buffer, 0, -1, false, { "line one", "line two", "line three" })
  return buffer
end

T["snapshot"] = MiniTest.new_set({
  hooks = {
    post_case = function()
      for _, buffer in ipairs(nvim.api.nvim_list_bufs()) do
        pcall(nvim.diagnostic.reset, nil, buffer)
      end
    end,
  },
})

T["snapshot"]["normalizes error and warning diagnostics into one-based positions"] = function()
  local buffer = new_buffer("/tmp/diagnostics.lua")
  nvim.diagnostic.set(ns, buffer, {
    {
      lnum = 1,
      col = 2,
      severity = nvim.diagnostic.severity.WARN,
      message = "unused local",
      source = "luals",
      code = "unused-local",
    },
    {
      lnum = 0,
      col = 0,
      severity = nvim.diagnostic.severity.ERROR,
      message = "syntax error",
    },
  })

  local snapshot = Diagnostics.snapshot(buffer)

  MiniTest.expect.equality(snapshot.entries, {
    { path = "/tmp/diagnostics.lua", lnum = 1, col = 1, severity = "ERROR", message = "syntax error" },
    {
      path = "/tmp/diagnostics.lua",
      lnum = 2,
      col = 3,
      severity = "WARN",
      source = "luals",
      code = "unused-local",
      message = "unused local",
    },
  })
  MiniTest.expect.equality(snapshot.truncated_count, 0)
  MiniTest.expect.equality(snapshot.truncated_messages, 0)
  MiniTest.expect.equality(snapshot.changedtick, nvim.api.nvim_buf_get_changedtick(buffer))
  MiniTest.expect.equality(type(snapshot.observed_at), "number")

  nvim.api.nvim_buf_delete(buffer, { force = true })
end

T["snapshot"]["excludes info and hint severities"] = function()
  local buffer = new_buffer("/tmp/diagnostics-severity.lua")
  nvim.diagnostic.set(ns, buffer, {
    { lnum = 0, col = 0, severity = nvim.diagnostic.severity.INFO, message = "info" },
    { lnum = 0, col = 0, severity = nvim.diagnostic.severity.HINT, message = "hint" },
  })

  local snapshot = Diagnostics.snapshot(buffer)

  MiniTest.expect.equality(snapshot.entries, {})
  nvim.api.nvim_buf_delete(buffer, { force = true })
end

T["snapshot"]["reports [No Name] for an unnamed buffer"] = function()
  local buffer = new_buffer(nil)
  nvim.diagnostic.set(ns, buffer, {
    { lnum = 0, col = 0, severity = nvim.diagnostic.severity.ERROR, message = "boom" },
  })

  local snapshot = Diagnostics.snapshot(buffer)

  MiniTest.expect.equality(snapshot.entries[1].path, "[No Name]")
  nvim.api.nvim_buf_delete(buffer, { force = true })
end

T["snapshot"]["caps total entries and reports the truncated count"] = function()
  local buffer = new_buffer("/tmp/diagnostics-cap.lua")
  local diagnostics = {}
  for i = 1, 5 do
    diagnostics[i] = { lnum = i - 1, col = 0, severity = nvim.diagnostic.severity.ERROR, message = "error " .. i }
  end
  nvim.diagnostic.set(ns, buffer, diagnostics)

  local snapshot = Diagnostics.snapshot(buffer, { max_entries = 2 })

  MiniTest.expect.equality(#snapshot.entries, 2)
  MiniTest.expect.equality(snapshot.entries[1].message, "error 1")
  MiniTest.expect.equality(snapshot.entries[2].message, "error 2")
  MiniTest.expect.equality(snapshot.truncated_count, 3)
  nvim.api.nvim_buf_delete(buffer, { force = true })
end

T["snapshot"]["caps message length and reports the truncated message count"] = function()
  local buffer = new_buffer("/tmp/diagnostics-message.lua")
  nvim.diagnostic.set(ns, buffer, {
    { lnum = 0, col = 0, severity = nvim.diagnostic.severity.ERROR, message = "abcdefghij" },
  })

  local snapshot = Diagnostics.snapshot(buffer, { max_message_length = 4 })

  MiniTest.expect.equality(snapshot.entries[1].message, "abcd")
  MiniTest.expect.equality(snapshot.truncated_messages, 1)
  nvim.api.nvim_buf_delete(buffer, { force = true })
end

T["snapshot"]["does not refresh diagnostics or mutate the buffer"] = function()
  local buffer = new_buffer("/tmp/diagnostics-changedtick.lua")
  local before = nvim.api.nvim_buf_get_changedtick(buffer)

  Diagnostics.snapshot(buffer)

  MiniTest.expect.equality(nvim.api.nvim_buf_get_changedtick(buffer), before)
  nvim.api.nvim_buf_delete(buffer, { force = true })
end

return T
