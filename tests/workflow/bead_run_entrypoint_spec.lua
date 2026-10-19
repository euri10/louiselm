local MiniTest = require("mini.test")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local T = MiniTest.new_set()
local PRIVATE_FILE_ERROR = "Run selection must be an operator-owned mode-0600 JSON file, at most 64KiB"

local function selection_file(bytes, permissions)
  local path = nvim.fn.tempname()
  MiniTest.finally(function()
    nvim.fn.delete(path)
  end)
  nvim.fn.writefile({ bytes }, path, "b")
  assert(nvim.fn.setfperm(path, permissions or "rw-------") == 1)
  return path
end

local function refusal(path, expected)
  -- Real entrypoint and runtime APIs, in a separate editor; no global stubs.
  -- Every fixture has invalid selection data, so none can authorize a Run.
  local result = nvim
    .system({
      nvim.v.progpath,
      "--headless",
      "--noplugin",
      "-u",
      "NONE",
      "-i",
      "NONE",
      "--cmd",
      "set runtimepath^=" .. nvim.fn.fnameescape(nvim.fn.getcwd()),
      "--cmd",
      "let g:louiselm_bead_run_selection=" .. nvim.fn.string(path),
      "-c",
      "luafile scripts/bead-run.lua",
      "-c",
      "qa!",
    }, { text = true, timeout = 5000 })
    :wait()
  assert(result.code == 0, result.stderr)
  assert(result.stderr:find(expected, 1, true), result.stderr)
end

T["owned private file reaches selection validation through the real entrypoint"] = function()
  -- Regression: the live 0.12.5 entrypoint crashed on vim.uv.os_getuid before
  -- operator.new, which the previous installed/unit tests called directly.
  refusal(selection_file("{}"), "invalid closed operator Run selection or options")
end

T["selection file size ceiling is inclusive"] = function()
  refusal(selection_file(string.rep(" ", 65534) .. "{}"), "invalid closed operator Run selection or options")
  refusal(selection_file(string.rep(" ", 65535) .. "{}"), PRIVATE_FILE_ERROR)
end

T["refuses public or executable selection files before JSON decoding"] = function()
  for _, permissions in ipairs({ "rw-r--r--", "rw-rw----", "rwx------" }) do
    refusal(selection_file("not JSON", permissions), PRIVATE_FILE_ERROR)
  end
end

T["refuses missing paths and directories before selection validation"] = function()
  refusal(nvim.fn.tempname(), PRIVATE_FILE_ERROR)
  refusal(nvim.fn.getcwd(), PRIVATE_FILE_ERROR)
end

T["reports malformed private JSON without a Lua crash"] = function()
  refusal(selection_file("not JSON"), "Run selection is invalid JSON")
end

return T
