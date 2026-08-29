local MiniTest = require("mini.test")

local Vintage = require("louiselm.provenance.vintage")

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim
local T = MiniTest.new_set()

T["parse"] = MiniTest.new_set()

T["parse"]["preserves custom statuses and reports issue-level changes"] = function()
  local historical = assert(
    Vintage.parse_issues(
      nvim.json.encode({ id = "one", status = "custom", title = "old" })
        .. "\n"
        .. nvim.json.encode({ id = "removed", status = "closed" })
        .. "\n"
    )
  )
  local current = assert(
    Vintage.parse_issues(
      nvim.json.encode({ id = "one", status = "custom", title = "new" })
        .. "\n"
        .. nvim.json.encode({ id = "added", status = "open" })
        .. "\n"
    )
  )

  MiniTest.expect.equality(historical[1].status, "custom")
  MiniTest.expect.equality(Vintage.diff(historical, current), {
    added = { "added" },
    removed = { "removed" },
    changed = { "one" },
  })
end

T["parse"]["rejects malformed and duplicate issue records"] = function()
  local _, malformed = Vintage.parse_issues("not json\n")
  assert(malformed ~= nil)
  MiniTest.expect.equality(malformed.code, "invalid_snapshot")
  local _, duplicate = Vintage.parse_issues('{"id":"one"}\n{"id":"one"}\n')
  assert(duplicate ~= nil)
  MiniTest.expect.equality(duplicate.code, "invalid_snapshot")
end

T["load"] = MiniTest.new_set()

T["load"]["extracts a ref and compares it with the current file"] = function()
  local root = nvim.fn.tempname()
  assert(nvim.fn.mkdir(nvim.fs.joinpath(root, ".beads"), "p") == 1)
  local current_path = nvim.fs.joinpath(root, ".beads", "issues.jsonl")
  assert(nvim.fn.writefile({ '{"id":"now","status":"open"}' }, current_path) == 0)
  local original_system = nvim.system
  local original_schedule = nvim.schedule
  local process
  local scheduled
  rawset(nvim, "system", function(command, options, callback)
    process = { command = command, options = options, callback = callback }
    return process
  end)
  rawset(nvim, "schedule", function(callback)
    scheduled = callback
  end)

  local result
  local started, start_error = Vintage.load(root, "HEAD~2", function(value, error_value)
    result = { value = value, error_value = error_value }
  end)
  MiniTest.expect.equality(started, true)
  MiniTest.expect.equality(start_error, nil)
  MiniTest.expect.equality(process.command, { "git", "show", "HEAD~2:.beads/issues.jsonl" })
  process.callback({ code = 0, stdout = '{"id":"then","status":"custom"}\n', stderr = "" })
  scheduled()

  MiniTest.expect.equality(result.error_value, nil)
  MiniTest.expect.equality(result.value.issues[1].status, "custom")
  MiniTest.expect.equality(result.value.diff, {
    added = { "now" },
    removed = { "then" },
    changed = {},
  })
  rawset(nvim, "system", original_system)
  rawset(nvim, "schedule", original_schedule)
  nvim.fn.delete(root, "rf")
end

return T
