local MiniTest = require("mini.test")
local Apply = require("louiselm.ui.diff.apply")
local Buffer = require("louiselm.ui.diff.buffer")
local Diff = require("louiselm.ui.diff")

local T = MiniTest.new_set()

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local function temp_file(lines)
  local path = nvim.fn.tempname()
  nvim.fn.writefile(lines, path)
  return path
end

T["apply"] = MiniTest.new_set()

T["apply"]["previews replacement content and applies only to the unchanged file"] = function()
  local path = temp_file({ "before" })
  local preview = assert(Apply.preview({ path = path, content = "after\n" }))

  MiniTest.expect.equality(preview.original, "before\n")
  MiniTest.expect.equality(preview.proposed, "after\n")
  MiniTest.expect.equality(preview.diff:find("%-before", 1, false) ~= nil, true)
  MiniTest.expect.equality(preview.diff:find("%+after", 1, false) ~= nil, true)

  local applied, apply_error = Apply.apply(preview)
  MiniTest.expect.equality({ applied, apply_error }, { true, nil })
  MiniTest.expect.equality(nvim.fn.readfile(path), { "after" })

  nvim.fn.writefile({ "changed" }, path)
  local stale, stale_error = Apply.apply(preview)
  MiniTest.expect.equality(stale, false)
  MiniTest.expect.equality(stale_error, "file changed since diff preview")
  nvim.fn.delete(path)
end

T["apply"]["applies a single-file unified diff"] = function()
  local path = temp_file({ "one", "two" })
  local preview = assert(Apply.preview({
    path = path,
    diff = "--- a/file\n+++ b/file\n@@ -1,2 +1,2 @@\n one\n-two\n+changed\n",
  }))

  MiniTest.expect.equality(preview.proposed, "one\nchanged\n")
  assert(Apply.apply(preview))
  MiniTest.expect.equality(nvim.fn.readfile(path), { "one", "changed" })
  nvim.fn.delete(path)
end

T["apply"]["previews an exact-text replacement and reports a missing one"] = function()
  local path = temp_file({ "one", "two" })
  local preview = assert(Apply.preview({ path = path, replacement = { old = "two", new = "three" } }))
  MiniTest.expect.equality(preview.proposed, "one\nthree\n")

  local missing, missing_error = Apply.preview({ path = path, replacement = { old = "four", new = "five" } })
  MiniTest.expect.equality(missing, nil)
  MiniTest.expect.equality(missing_error, "diff edit replacement text is not in the file")

  local repeated = temp_file({ "x", "x" })
  local first = assert(Apply.preview({ path = repeated, replacement = { old = "x", new = "y" } }))
  MiniTest.expect.equality(first.proposed, "y\nx\n")
  local all = assert(Apply.preview({ path = repeated, replacement = { old = "x", new = "y", all = true } }))
  MiniTest.expect.equality(all.proposed, "y\ny\n")

  nvim.fn.delete(path)
  nvim.fn.delete(repeated)
end

T["buffer"] = MiniTest.new_set()

T["buffer"]["renders a nonmodifiable diff buffer"] = function()
  local path = temp_file({ "before" })
  local preview = assert(Apply.preview({ path = path, content = "after\n" }))
  local buffer = assert(Buffer.open(preview))

  MiniTest.expect.equality(nvim.api.nvim_buf_get_option(buffer, "filetype"), "diff")
  MiniTest.expect.equality(nvim.api.nvim_buf_get_option(buffer, "modifiable"), false)
  MiniTest.expect.equality(nvim.api.nvim_buf_get_lines(buffer, 0, -1, false), {
    "louiselm diff: " .. path,
    "",
    "--- original",
    "+++ proposed",
    "@@ -1 +1 @@",
    "-before",
    "+after",
  })

  Buffer.close(buffer)
  nvim.fn.delete(path)
end

T["review"] = MiniTest.new_set()

T["review"]["shows an edit and sends an allow response"] = function()
  local path = temp_file({ "before" })
  local response
  local diff = Diff.new()
  assert(diff:open({
    operation = { kind = "file_edit", path = path },
    toolCall = { rawInput = { path = path, content = "after\n" } },
    options = { { optionId = "allow-once", kind = "allow_once" }, "deny" },
  }, function(result)
    response = result
    return true
  end))

  local buffer = assert(diff.buffer)
  MiniTest.expect.equality(nvim.api.nvim_buf_get_option(buffer, "modifiable"), false)
  MiniTest.expect.equality(nvim.api.nvim_buf_get_lines(buffer, 2, 3, false), {
    "Review proposed edit:  Esc then a = accept, d/q = reject",
  })
  local mappings = {}
  for _, mapping in ipairs(nvim.api.nvim_buf_get_keymap(buffer, "n")) do
    mappings[mapping.lhs] = mapping.desc
  end
  MiniTest.expect.equality(mappings, {
    a = "Allow louiselm file edit",
    d = "Reject louiselm file edit",
    q = "Reject louiselm file edit",
  })

  nvim.api.nvim_feedkeys("a", "mx", false)

  MiniTest.expect.equality(response, { outcome = { outcome = "selected", optionId = "allow-once" } })
  MiniTest.expect.equality(diff.buffer, nil)
  diff:dispose()
  nvim.fn.delete(path)
end

T["review"]["rejects through both advertised keys"] = function()
  local path = temp_file({ "before" })
  for _, key in ipairs({ "d", "q" }) do
    local responses = {}
    local diff = Diff.new()
    assert(diff:open({
      operation = { kind = "file_edit", path = path },
      toolCall = { rawInput = { path = path, content = "after\n" } },
      options = { { optionId = "allow-once", kind = "allow_once" }, { optionId = "reject", kind = "reject_once" } },
    }, function(result)
      responses[#responses + 1] = result
      return true
    end))

    nvim.api.nvim_feedkeys(key, "mx", false)

    MiniTest.expect.equality(responses, { { outcome = { outcome = "selected", optionId = "reject" } } })
    MiniTest.expect.equality(diff.buffer, nil)
    diff:dispose()
  end
  nvim.fn.delete(path)
end

T["review"]["returns to the buffer focused before the review"] = function()
  local path = temp_file({ "before" })
  local origin = nvim.api.nvim_create_buf(false, true)
  nvim.api.nvim_set_current_buf(origin)
  local diff = Diff.new()
  assert(diff:open({
    operation = { kind = "file_edit", path = path },
    toolCall = { rawInput = { path = path, content = "after\n" } },
    options = { { optionId = "allow-once", kind = "allow_once" } },
  }, function()
    return true
  end))
  MiniTest.expect.equality(nvim.api.nvim_get_current_buf(), diff.buffer)

  assert(diff:accept())

  MiniTest.expect.equality(nvim.api.nvim_get_current_buf(), origin)
  diff:dispose()
  nvim.api.nvim_buf_delete(origin, { force = true })
  nvim.fn.delete(path)
end

T["review"]["reviews an edit carried only in ACP diff content"] = function()
  local path = temp_file({ "hello" })
  local response
  local diff = Diff.new()
  -- Shaped like a claude-agent-acp Edit request: tool arguments verbatim in rawInput,
  -- reviewable text only in the ACP diff content entry.
  assert(diff:open({
    operation = { kind = "file_edit", path = path },
    toolCall = {
      kind = "edit",
      rawInput = { file_path = path, old_string = "hello", new_string = "hello world", replace_all = false },
      content = { { type = "diff", path = path, oldText = "hello", newText = "hello world" } },
    },
    options = { { optionId = "allow-once", kind = "allow_once" }, { optionId = "reject", kind = "reject_once" } },
  }, function(result)
    response = result
    return true
  end))

  local lines = nvim.api.nvim_buf_get_lines(diff.buffer, 0, -1, false)
  MiniTest.expect.equality(lines[#lines - 1], "-hello")
  MiniTest.expect.equality(lines[#lines], "+hello world")
  assert(diff:accept())
  MiniTest.expect.equality(response, { outcome = { outcome = "selected", optionId = "allow-once" } })

  diff:dispose()
  nvim.fn.delete(path)
end

T["review"]["reviews a whole-file ACP diff content entry without old text"] = function()
  local path = temp_file({ "hello" })
  local diff = Diff.new()
  assert(diff:open({
    operation = { kind = "file_edit", path = path },
    toolCall = {
      kind = "write",
      rawInput = { file_path = path },
      content = { { type = "diff", path = path, newText = "replaced\n" } },
    },
    options = { { optionId = "allow-once", kind = "allow_once" } },
  }, function()
    return true
  end))

  MiniTest.expect.equality(diff.preview.proposed, "replaced\n")
  diff:dispose()
  nvim.fn.delete(path)
end

T["review"]["cancels the permission request when its review buffer is wiped"] = function()
  local path = temp_file({ "before" })
  local responses = {}
  local diff = Diff.new()
  assert(diff:open({
    operation = { kind = "file_edit", path = path },
    toolCall = { rawInput = { path = path, content = "after\n" } },
    options = { { optionId = "allow-once", kind = "allow_once" }, { optionId = "reject", kind = "reject_once" } },
  }, function(result)
    responses[#responses + 1] = result
    return true
  end))

  nvim.api.nvim_buf_delete(diff.buffer, { force = true })

  MiniTest.expect.equality(responses, { { outcome = { outcome = "cancelled" } } })
  MiniTest.expect.equality(diff.buffer, nil)
  diff:dispose()
  MiniTest.expect.equality(#responses, 1)
  nvim.fn.delete(path)
end

T["review"]["asks which lifetime to use when an edit has multiple allow options"] = function()
  local path = temp_file({ "before" })
  local response
  local labels
  local original_select = nvim.ui.select
  nvim.ui.select = function(options, config, callback)
    labels = nvim.tbl_map(config.format_item, options)
    callback(options[2])
  end
  local diff = Diff.new()
  assert(diff:open({
    operation = { kind = "file_edit", path = path },
    toolCall = { rawInput = { path = path, content = "after\n" } },
    options = {
      { optionId = "allow-once", name = "Allow Once", kind = "allow_once" },
      { optionId = "allow-always", name = "Always Allow This File", kind = "allow_always" },
      { optionId = "reject", name = "Reject", kind = "reject_once" },
    },
  }, function(result)
    response = result
    return true
  end))

  assert(diff:accept())

  nvim.ui.select = original_select
  MiniTest.expect.equality(labels, { "Allow Once", "Always Allow This File" })
  MiniTest.expect.equality(response, { outcome = { outcome = "selected", optionId = "allow-always" } })
  MiniTest.expect.equality(diff.buffer, nil)
  diff:dispose()
  nvim.fn.delete(path)
end

return T
