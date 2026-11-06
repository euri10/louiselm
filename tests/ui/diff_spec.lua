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
    "Review proposed edit:  Esc then a = accept, d/q = reject",
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

T["buffer"]["accepts a standalone proposal into the working tree"] = function()
  local path = temp_file({ "before" })
  local preview = assert(Apply.preview({ path = path, content = "after\n" }))
  local distractor = nvim.api.nvim_create_buf(true, true)
  local origin = nvim.api.nvim_create_buf(false, true)
  nvim.api.nvim_set_current_buf(distractor)
  nvim.api.nvim_set_current_buf(origin)
  local buffer = assert(Buffer.open(preview))

  nvim.api.nvim_feedkeys("a", "mx", false)

  MiniTest.expect.equality(nvim.fn.readfile(path), { "after" })
  MiniTest.expect.equality(nvim.api.nvim_buf_is_valid(buffer), false)
  MiniTest.expect.equality(nvim.api.nvim_get_current_buf(), origin)
  nvim.api.nvim_buf_delete(distractor, { force = true })
  nvim.api.nvim_buf_delete(origin, { force = true })
  nvim.fn.delete(path)
end

T["buffer"]["discards a standalone proposal through d and q"] = function()
  for _, key in ipairs({ "d", "q" }) do
    local path = temp_file({ "before" })
    local preview = assert(Apply.preview({ path = path, content = "after\n" }))
    local distractor = nvim.api.nvim_create_buf(true, true)
    local origin = nvim.api.nvim_create_buf(false, true)
    nvim.api.nvim_set_current_buf(distractor)
    nvim.api.nvim_set_current_buf(origin)
    local buffer = assert(Buffer.open(preview))

    nvim.api.nvim_feedkeys(key, "mx", false)

    MiniTest.expect.equality(nvim.fn.readfile(path), { "before" })
    MiniTest.expect.equality(nvim.api.nvim_buf_is_valid(buffer), false)
    MiniTest.expect.equality(nvim.api.nvim_get_current_buf(), origin)
    nvim.api.nvim_buf_delete(distractor, { force = true })
    nvim.api.nvim_buf_delete(origin, { force = true })
    nvim.fn.delete(path)
  end
end

T["buffer"]["keeps a stale standalone proposal open and reports the conflict"] = function()
  local path = temp_file({ "before" })
  local preview = assert(Apply.preview({ path = path, content = "after\n" }))
  local buffer = assert(Buffer.open(preview))
  nvim.fn.writefile({ "changed" }, path)
  local notification
  local original_notify = nvim.notify
  rawset(nvim, "notify", function(message)
    notification = message
  end)

  nvim.api.nvim_feedkeys("a", "mx", false)

  rawset(nvim, "notify", original_notify)
  MiniTest.expect.equality(nvim.fn.readfile(path), { "changed" })
  MiniTest.expect.equality(nvim.api.nvim_buf_is_valid(buffer), true)
  MiniTest.expect.equality(notification, "louiselm: file changed since diff preview")
  Buffer.close(buffer)
  nvim.fn.delete(path)
end

T["review"] = MiniTest.new_set()

T["review"]["defers every opened request while its Session cannot show UI"] = function()
  local visible = false
  local path = temp_file({ "before" })
  local diff = Diff.new(function()
    return visible
  end)
  local origin = nvim.api.nvim_get_current_buf()
  MiniTest.finally(function()
    diff:dispose()
    nvim.fn.delete(path)
  end)
  for _ = 1, 2 do
    visible = false
    assert(diff:open({
      operation = { kind = "file_edit", path = path },
      toolCall = { rawInput = { content = "after\n" } },
      options = { "allow", "deny" },
    }, function()
      error("hidden review must not answer")
    end))
    MiniTest.expect.equality(nvim.api.nvim_get_current_buf(), origin)
    MiniTest.expect.equality(diff:contains_current(), false)
    visible = true
    assert(diff:show())
    MiniTest.expect.equality(diff:contains_current(), true)
    diff:close()
  end
end

T["review"]["hides without answering or replacing another Session buffer"] = function()
  local path = temp_file({ "before" })
  local responses = {}
  local diff = Diff.new()
  local origin = nvim.api.nvim_get_current_buf()
  local origin_window = nvim.api.nvim_get_current_win()
  local other = nvim.api.nvim_create_buf(false, true)
  MiniTest.finally(function()
    diff:dispose()
    nvim.api.nvim_buf_delete(other, { force = true })
    nvim.fn.delete(path)
  end)
  assert(diff:open({
    operation = { kind = "file_edit", path = path },
    toolCall = { rawInput = { content = "after\n" } },
    options = { "allow", "deny" },
  }, function(result)
    responses[#responses + 1] = result
    return true
  end))
  local buffer = diff.buffer
  MiniTest.expect.equality(nvim.api.nvim_win_get_buf(origin_window), origin)
  nvim.api.nvim_win_set_cursor(0, { 5, 0 })
  diff:hide()
  MiniTest.expect.equality(responses, {})
  MiniTest.expect.equality(nvim.api.nvim_buf_is_valid(buffer), true)
  nvim.api.nvim_set_current_buf(other)
  assert(diff:show())
  MiniTest.expect.equality(nvim.api.nvim_get_current_buf(), buffer)
  MiniTest.expect.equality(nvim.api.nvim_win_get_cursor(0), { 5, 0 })
  diff:hide()
  diff:close()
  MiniTest.expect.equality(nvim.api.nvim_get_current_buf(), other)
  MiniTest.expect.equality(responses, {})
end

T["review"]["file edit review and permission choice retain the Session label"] = function()
  local path = temp_file({ "before" })
  local diff = Diff.new()
  MiniTest.finally(function()
    diff:dispose()
    nvim.fn.delete(path)
  end)
  assert(diff:open({
    operation = { kind = "file_edit", path = path },
    toolCall = { rawInput = { content = "after\n" } },
    options = {
      { optionId = "allow", kind = "allow_once" },
      { optionId = "always", kind = "allow_always" },
      { optionId = "deny", kind = "reject_once" },
    },
  }, function()
    return true
  end, "[codex session-7] Build QA"))
  MiniTest.expect.equality(
    nvim.api.nvim_buf_get_lines(diff.buffer, 2, 3, false),
    { "[codex session-7] Build QA · Review proposed edit:  a = accept, d/q/Esc = reject" }
  )
  assert(diff:accept())
  MiniTest.expect.equality(
    nvim.api.nvim_win_get_config(0).title[1][1],
    "[codex session-7] Build QA · louiselm file edit allow: "
  )
end

T["review"]["previews rawInput replacement fragments without deleting surrounding content"] = function()
  -- old_text/new_text shape and hello.py reproduction from louiselm-hht64's
  -- maintainer report (2026-10-09, acp-llm-adapter 0.9.1). No event order is assumed.
  local old = 'def greet(name):\n    return "Hello, " + name + "!"'
  local new =
    'def greet(name):\n    """Return a friendly greeting for the given name."""\n    return "Hello, " + name + "!"'
  local suffix = '\n\n\nif __name__ == "__main__":\n    print(greet("world"))\n'
  local path = temp_file(nvim.split(old .. suffix, "\n", { plain = true, trimempty = true }))
  MiniTest.finally(function()
    nvim.fn.delete(path)
  end)

  for _, fields in ipairs({ { "old_text", "new_text" }, { "oldText", "newText" }, { "old_string", "new_string" } }) do
    for _, with_entry in ipairs({ false, true }) do
      local raw_input = { path = path, [fields[1]] = old, [fields[2]] = new }
      local diff = Diff.new()
      MiniTest.finally(function()
        diff:dispose()
      end)
      local response
      assert(diff:open({
        operation = { kind = "file_edit", path = path },
        toolCall = {
          rawInput = raw_input,
          content = with_entry and { { type = "diff", path = path, oldText = old, newText = new } } or nil,
        },
        options = { { optionId = "allow-once", kind = "allow_once" } },
      }, function(result)
        response = result
        return true
      end))

      MiniTest.expect.equality(diff.preview.original, old .. suffix)
      MiniTest.expect.equality(diff.preview.proposed, new .. suffix)
      local lines = nvim.api.nvim_buf_get_lines(diff.buffer, 0, -1, false)
      MiniTest.expect.equality(
        nvim.tbl_contains(lines, '+    """Return a friendly greeting for the given name."""'),
        true
      )
      MiniTest.expect.equality(nvim.tbl_contains(lines, '-if __name__ == "__main__":'), false)
      MiniTest.expect.equality(nvim.tbl_contains(lines, "\\ No newline at end of file"), false)
      assert(diff:accept())
      MiniTest.expect.equality(response, { outcome = { outcome = "selected", optionId = "allow-once" } })
      MiniTest.expect.equality(Apply.read(path), old .. suffix)
    end
  end
end

T["review"]["honors rawInput first replacement, replace_all and empty new text"] = function()
  local path = temp_file({ "before", "before", "tail" })
  MiniTest.finally(function()
    nvim.fn.delete(path)
  end)
  for _, case in ipairs({
    { new = "after", all = false, proposed = "after\nbefore\ntail\n" },
    { new = "after", all = true, proposed = "after\nafter\ntail\n" },
    { new = "", all = false, proposed = "\nbefore\ntail\n" },
  }) do
    local diff = Diff.new()
    MiniTest.finally(function()
      diff:dispose()
    end)
    assert(diff:open({
      operation = { kind = "file_edit", path = path },
      toolCall = { rawInput = { path = path, old_text = "before", new_text = case.new, replace_all = case.all } },
      options = { "deny" },
    }, function()
      return true
    end))
    MiniTest.expect.equality(diff.preview.proposed, case.proposed)
    diff:dispose()
  end
end

T["review"]["rejects invalid or absent replacement text without a whole-file preview"] = function()
  local path = temp_file({ "before", "tail" })
  MiniTest.finally(function()
    nvim.fn.delete(path)
  end)
  for _, case in ipairs({
    { old = "absent", new = "after", error = "diff edit replacement text is not in the file" },
    { old = "", new = "after", error = "diff edit replacement must have non-empty old text" },
    { old = 42, new = "after", error = "diff edit replacement must have non-empty old text" },
    { old = false, new = "after", error = "diff edit replacement must have non-empty old text" },
    { old = "before", error = "diff edit replacement must have new text" },
    { old = "before", new = 42, error = "diff edit replacement must have new text" },
  }) do
    local diff = Diff.new()
    MiniTest.finally(function()
      diff:dispose()
    end)
    local opened, open_error = diff:open({
      operation = { kind = "file_edit", path = path },
      toolCall = { rawInput = { path = path, old_text = case.old, new_text = case.new } },
    }, function()
      return true
    end)
    MiniTest.expect.equality({ opened, open_error }, { false, case.error })
    MiniTest.expect.equality(diff.buffer, nil)
  end
end

T["review"]["keeps new text without old text as whole-file content"] = function()
  local path = temp_file({ "before", "tail" })
  MiniTest.finally(function()
    nvim.fn.delete(path)
  end)
  for _, field in ipairs({ "content", "newText", "new_text" }) do
    local diff = Diff.new()
    MiniTest.finally(function()
      diff:dispose()
    end)
    assert(diff:open({
      operation = { kind = "file_edit", path = path },
      toolCall = { rawInput = { path = path, [field] = "after\n" } },
      options = { "deny" },
    }, function()
      return true
    end))
    MiniTest.expect.equality(diff.preview.proposed, "after\n")
    diff:dispose()
  end
end

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
    "Review proposed edit:  a = accept, d/q/Esc = reject",
  })
  local mappings = {}
  for _, mapping in ipairs(nvim.api.nvim_buf_get_keymap(buffer, "n")) do
    mappings[mapping.lhs] = mapping.desc or true
  end
  MiniTest.expect.equality(mappings, {
    a = "Allow louiselm file edit",
    d = "Reject louiselm file edit",
    q = true,
    ["<Esc>"] = true,
  })

  nvim.api.nvim_feedkeys("a", "mx", false)

  MiniTest.expect.equality(response, { outcome = { outcome = "selected", optionId = "allow-once" } })
  MiniTest.expect.equality(diff.buffer, nil)
  diff:dispose()
  nvim.fn.delete(path)
end

T["review"]["rejects through d q and Escape"] = function()
  local path = temp_file({ "before" })
  for _, key in ipairs({ "d", "q", "<Esc>" }) do
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

    nvim.api.nvim_feedkeys(nvim.api.nvim_replace_termcodes(key, true, false, true), "mx", false)

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
  -- With no text in rawInput, review the ACP diff content entry.
  assert(diff:open({
    operation = { kind = "file_edit", path = path },
    toolCall = {
      kind = "edit",
      rawInput = { file_path = path },
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

T["review"]["terminal close and disposal retire the review without answering"] = function()
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

  local buffer = diff.buffer
  diff:close()

  MiniTest.expect.equality(nvim.api.nvim_buf_is_valid(buffer), false)
  MiniTest.expect.equality(responses, {})
  MiniTest.expect.equality(diff.buffer, nil)
  diff:dispose()
  MiniTest.expect.equality(#responses, 0)
  nvim.fn.delete(path)
end

T["review"]["asks which lifetime to use when an edit has multiple allow options"] = function()
  local path = temp_file({ "before" })
  local response
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

  MiniTest.expect.equality(
    nvim.api.nvim_buf_get_lines(diff.buffer, 0, -1, false),
    { "Allow Once", "Always Allow This File" }
  )
  nvim.api.nvim_win_set_cursor(0, { 2, 0 })
  local picker_buffer = diff.buffer
  diff:hide()
  MiniTest.expect.equality(response, nil)
  assert(diff:show())
  MiniTest.expect.equality(diff.buffer, picker_buffer)
  MiniTest.expect.equality(nvim.api.nvim_win_get_cursor(0), { 2, 0 })
  nvim.api.nvim_feedkeys(nvim.api.nvim_replace_termcodes("<CR>", true, false, true), "mx", false)
  MiniTest.expect.equality(response, { outcome = { outcome = "selected", optionId = "allow-always" } })
  MiniTest.expect.equality(diff.buffer, nil)
  diff:dispose()
  nvim.fn.delete(path)
end

T["review"]["cancelled lifetime selection returns to the review and stale choices cannot answer a new request"] = function()
  local path = temp_file({ "before" })
  local diff = Diff.new()
  MiniTest.finally(function()
    diff:dispose()
    nvim.fn.delete(path)
  end)
  local request = {
    operation = { kind = "file_edit", path = path },
    toolCall = { rawInput = { content = "after\n" } },
    options = {
      { optionId = "once", kind = "allow_once" },
      { optionId = "always", kind = "allow_always" },
      { optionId = "deny", kind = "reject_once" },
    },
  }
  local responses = {}
  local function response(result)
    responses[#responses + 1] = result
    return true
  end
  assert(diff:open(request, response))
  local prompt = rawget(diff, "prompt")
  local select = prompt.select
  local old_callback
  prompt.select = function(owner, items, options, callback)
    old_callback = callback
    return select(owner, items, options, callback)
  end
  assert(diff:accept())
  local picker = diff.buffer
  nvim.api.nvim_feedkeys("q", "mx", false)
  MiniTest.expect.equality(responses, {})
  MiniTest.expect.equality(nvim.api.nvim_buf_is_valid(picker), false)
  MiniTest.expect.equality(nvim.api.nvim_get_option_value("filetype", { buf = diff.buffer }), "diff")
  old_callback(request.options[2])
  MiniTest.expect.equality(responses, {})
  assert(diff:accept())
  local retired_callback = old_callback
  diff:close()
  assert(diff:open(request, response))
  retired_callback(request.options[2])
  MiniTest.expect.equality(responses, {})
  assert(diff:reject())
  MiniTest.expect.equality(responses, { { outcome = { outcome = "selected", optionId = "deny" } } })
end

T["review"]["lifetime callbacks schedule editor work from fast events"] = function()
  local path = temp_file({ "before" })
  local diff = Diff.new()
  MiniTest.finally(function()
    diff:dispose()
    nvim.fn.delete(path)
  end)
  local responses = {}
  local request = {
    operation = { kind = "file_edit", path = path },
    toolCall = { rawInput = { content = "after\n" } },
    options = {
      { optionId = "once", kind = "allow_once" },
      { optionId = "always", kind = "allow_always" },
    },
  }
  assert(diff:open(request, function(result)
    assert(not nvim.in_fast_event())
    responses[#responses + 1] = result
    return true
  end))
  local callback
  rawget(diff, "prompt").select = function(_, _, _, selected)
    callback = selected
    return true
  end
  assert(diff:accept())
  local timer = assert(nvim.uv.new_timer())
  MiniTest.finally(function()
    if not timer:is_closing() then
      timer:close()
    end
  end)
  timer:start(0, 0, function()
    assert(nvim.in_fast_event())
    callback(request.options[2])
    MiniTest.expect.equality(responses, {})
    timer:close()
  end)
  assert(nvim.wait(1000, function()
    return #responses == 1
  end, 5))
  MiniTest.expect.equality(responses, { { outcome = { outcome = "selected", optionId = "always" } } })
  MiniTest.expect.equality(diff.buffer, nil)
end

T["review"]["failed lifetime presentation keeps rejection usable and reports a sanitized mapping error"] = function()
  local path = temp_file({ "before" })
  local diff = Diff.new()
  local responses, messages = {}, {}
  local notify = nvim.notify
  MiniTest.finally(function()
    diff:dispose()
    rawset(nvim, "notify", notify)
    nvim.fn.delete(path)
  end)
  rawset(nvim, "notify", function(message)
    messages[#messages + 1] = message
  end)
  assert(diff:open({
    operation = { kind = "file_edit", path = path },
    toolCall = { rawInput = { content = "after\n" } },
    options = {
      { optionId = "once", kind = "allow_once" },
      { optionId = "always", kind = "allow_always" },
      { optionId = "deny", kind = "reject_once" },
    },
  }, function(result)
    responses[#responses + 1] = result
    return true
  end))
  rawget(diff, "prompt").select = function()
    return false, "private request contents"
  end
  nvim.api.nvim_feedkeys("a", "mx!", false)
  nvim.api.nvim_feedkeys("q", "mx!", false)
  MiniTest.expect.equality(responses, { { outcome = { outcome = "selected", optionId = "deny" } } })
  MiniTest.expect.equality(messages, { "louiselm: diff decision failed" })
end

T["review"]["restored preview rejects saved custom mappings from before it was hidden"] = function()
  local path = temp_file({ "before" })
  local diff = Diff.new()
  local responses = {}
  MiniTest.finally(function()
    diff:dispose()
    nvim.fn.delete(path)
  end)
  assert(diff:open({
    operation = { kind = "file_edit", path = path },
    toolCall = { rawInput = { content = "after\n" } },
    options = { "allow", "deny" },
  }, function(result)
    responses[#responses + 1] = result
    return true
  end))
  local saved = {}
  for _, item in ipairs(nvim.api.nvim_buf_get_keymap(diff.buffer, "n")) do
    if item.lhs == "a" or item.lhs == "d" then
      saved[#saved + 1] = item.callback
    end
  end
  diff:hide()
  assert(diff:show())
  for _, callback in ipairs(saved) do
    callback()
  end
  MiniTest.expect.equality(responses, {})
  nvim.api.nvim_feedkeys("d", "mx!", false)
  MiniTest.expect.equality(responses, { { outcome = { outcome = "selected", optionId = "deny" } } })
end

return T
