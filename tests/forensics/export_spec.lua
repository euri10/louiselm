local MiniTest = require("mini.test")
local Store = require("louiselm.forensics.store")
---@diagnostic disable-next-line: undefined-global -- Neovim runtime.
local nvim = vim
local root
local T = MiniTest.new_set({
  hooks = {
    pre_case = function()
      root = nvim.fn.tempname()
      assert(nvim.fn.mkdir(root, "p", 448) == 1)
    end,
    post_case = function()
      nvim.fn.delete(root, "rf")
    end,
  },
})

local function fixture(lines)
  local source = root .. "/wire.jsonl"
  assert(nvim.fn.writefile(lines or { '{"text":"PRIVATE PROMPT"}' }, source) == 0)
  local record = assert(assert(Store.new(root)):write({
    schema_version = 1,
    id = "private-record",
    observed_at = 100,
    subject = { agent = "private-agent", acp_session_id = "private-session" },
    observations = { cwd = "/private/project", capabilities = { load_session = true }, model = "private-model" },
    evidence_sources = {
      { kind = "acp_log", state = "present", path = source },
      { kind = "agent_transcript", state = "present", path = root .. "/missing" },
      { kind = "acp_log", state = "present", path = root },
    },
  }))
  return record, source
end

local function export(record, selections, destination)
  local done, result, failure, fast = false, nil, nil, nil
  local cancel, start_error = require("louiselm.forensics.export").write(
    record,
    destination or root .. "/export.json",
    selections,
    function(path, err)
      result, failure, fast, done = path, err, nvim.in_fast_event(), true
    end
  )
  assert(cancel, start_error)
  assert(nvim.wait(5000, function()
    return done
  end))
  MiniTest.expect.equality(fast, false)
  return result, failure
end

T["exports only selected redacted evidence without mutating inputs"] = function()
  local record, source = fixture({
    '{"text":"UNSELECTED"}',
    '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"PRIVATE ID","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"PRIVATE PROMPT bearer TOKEN /private/path"}}},"secret-key":"SECRET"}',
  })
  local before, source_before = nvim.fn.readfile(record), nvim.fn.readfile(source)
  local path = assert(export(record, { "observation:capabilities", "source:1:2:2" }))
  local bytes = table.concat(nvim.fn.readfile(path), "\n")
  local artifact = nvim.json.decode(bytes)
  MiniTest.expect.equality(artifact.schema_version, 1)
  MiniTest.expect.equality(#artifact.items, 2)
  MiniTest.expect.equality(artifact.items[1].value.load_session, true)
  MiniTest.expect.equality(artifact.items[2].state, "exported")
  MiniTest.expect.equality(artifact.items[2].lines[1].value.method, "session/update")
  for _, secret in ipairs({ "PRIVATE", "TOKEN", "private", "UNSELECTED", "SECRET", "secret-key" }) do
    MiniTest.expect.equality(bytes:find(secret, 1, true), nil)
  end
  MiniTest.expect.equality(nvim.fn.readfile(record), before)
  MiniTest.expect.equality(nvim.fn.readfile(source), source_before)
  MiniTest.expect.equality(nvim.uv.fs_stat(path).mode % 512, 384)
end

T["reports unavailable and malformed selections without exporting their content"] = function()
  local record = fixture({ "NOT JSON SECRET" })
  local path = assert(export(record, { "source:2:1:1", "source:3:1:1", "source:1:1:1", "observation:agent_version" }))
  local items = nvim.json.decode(table.concat(nvim.fn.readfile(path), "\n")).items
  MiniTest.expect.equality(
    { items[1].state, items[2].state, items[3].state, items[4].state },
    { "missing", "unreadable", "invalid_json", "missing" }
  )
  for _, item in ipairs(items) do
    MiniTest.expect.equality(item.lines, nil)
  end
end

T["requires bounded explicit selections and never overwrites a destination"] = function()
  local record = fixture()
  local Export = require("louiselm.forensics.export")
  for _, selections in ipairs({
    {},
    { "all" },
    { "source:1:0:1" },
    { "source:1:1:201" },
    { "observation:cwd", "observation:cwd" },
    { [2] = "observation:cwd" },
  }) do
    local started, err = Export.write(record, root .. "/refused.json", selections, function()
      error("must not start")
    end)
    MiniTest.expect.equality(started, nil)
    MiniTest.expect.equality(type(err), "string")
  end
  local before = nvim.fn.readfile(record)
  local path, err = export(record, { "observation:cwd" }, record)
  MiniTest.expect.equality(path, nil)
  MiniTest.expect.equality(type(err), "string")
  MiniTest.expect.equality(nvim.fn.readfile(record), before)
end

T["bounds reads and refuses incomplete ranges without claiming truncated exports"] = function()
  local record = fixture({ '{"text":"' .. string.rep("x", 70000) .. '"}' })
  local path = assert(export(record, { "source:1:1:1", "source:1:2:2" }))
  local items = nvim.json.decode(table.concat(nvim.fn.readfile(path), "\n")).items
  MiniTest.expect.equality({ items[1].state, items[2].state }, { "limit_exceeded", "missing" })
  MiniTest.expect.equality(items[1].lines, nil)
  MiniTest.expect.equality(nvim.uv.fs_stat(path).size < 262144, true)
end

T["cancellation completes once on the editor loop without publishing"] = function()
  local record = fixture()
  local calls, failure = 0, nil
  local cancel = assert(
    require("louiselm.forensics.export").write(
      record,
      root .. "/cancelled.json",
      { "observation:cwd" },
      function(path, err)
        MiniTest.expect.equality(path, nil)
        MiniTest.expect.equality(nvim.in_fast_event(), false)
        calls, failure = calls + 1, err
      end
    )
  )
  cancel()
  cancel()
  assert(nvim.wait(5000, function()
    return calls > 0
  end))
  MiniTest.expect.equality(calls, 1)
  MiniTest.expect.equality(failure, "evidence export cancelled")
  MiniTest.expect.equality(nvim.uv.fs_stat(root .. "/cancelled.json"), nil)
end

T["refuses oversized or invalid records and leaves no temporary output"] = function()
  local record = fixture()
  for _, bytes in ipairs({ "not JSON", '{"schema_version":2}', string.rep(" ", 262145) }) do
    assert(nvim.fn.writefile({ bytes }, record) == 0)
    local path, err = export(record, { "observation:cwd" })
    MiniTest.expect.equality(path, nil)
    MiniTest.expect.equality(type(err), "string")
  end
  MiniTest.expect.equality(nvim.fn.glob(root .. "/export.json*", false, true), {})
end

T["refuses symlinks and unavailable ranges while bounding the source scan"] = function()
  local record, source = fixture({ string.rep(" ", 1048577), "{}" })
  local path = assert(export(record, { "source:1:2:2" }))
  MiniTest.expect.equality(
    nvim.json.decode(table.concat(nvim.fn.readfile(path), "\n")).items[1].state,
    "limit_exceeded"
  )
  assert(nvim.uv.fs_unlink(source))
  assert(nvim.uv.fs_symlink(record, source))
  local linked = assert(export(record, { "source:1:1:1" }, root .. "/linked.json"))
  MiniTest.expect.equality(nvim.json.decode(table.concat(nvim.fn.readfile(linked), "\n")).items[1].state, "unreadable")
end

T["rejects oversized artifacts instead of silently truncating the selection"] = function()
  local lines = {}
  local payload = '{"content":[' .. string.rep('"secret",', 127) .. '"secret"]}'
  for _ = 1, 200 do
    lines[#lines + 1] = payload
  end
  local record = fixture(lines)
  local path, err = export(record, { "source:1:1:200" })
  MiniTest.expect.equality(path, nil)
  MiniTest.expect.equality(err, "Evidence export exceeds 256 KiB; select less evidence")
  MiniTest.expect.equality(nvim.fn.glob(root .. "/export.json*", false, true), {})
end

T["redacts hostile keys, numeric secrets and nested values without mutating the input"] = function()
  local Format = require("louiselm.forensics.export_format")
  local value = {
    id = 123456,
    method = "secret-method",
    params = { content = { text = "secret" }, ["secret-key"] = true },
    result = false,
  }
  local before = nvim.deepcopy(value)
  local redacted = Format.redact(value)
  MiniTest.expect.equality(redacted.id, "[redacted]")
  MiniTest.expect.equality(redacted.method, "[redacted]")
  MiniTest.expect.equality(redacted.result, false)
  MiniTest.expect.equality(redacted.params["secret-key"], nil)
  MiniTest.expect.equality(redacted.params.redacted_fields, 1)
  MiniTest.expect.equality(value, before)
end

T["preserves empty JSON object and array shapes"] = function()
  local record = fixture({ '{"params":{},"result":[]}' })
  local path = assert(export(record, { "source:1:1:1" }))
  local item = nvim.json.decode(table.concat(nvim.fn.readfile(path), "\n")).items[1].lines[1].value
  MiniTest.expect.equality(nvim.json.encode(item.params), "{}")
  MiniTest.expect.equality(nvim.json.encode(item.result), "[]")
end

T["redacts whole over-budget values without choosing fields by iteration order"] = function()
  local Format = require("louiselm.forensics.export_format")
  local value = {}
  for index = 1, 128 do
    value[index] = {}
    for child = 1, 128 do
      value[index][child] = "secret"
    end
  end
  MiniTest.expect.equality(Format.redact(value), "[redacted:limit]")
end

T["refuses an output parent writable by other users"] = function()
  local record = fixture()
  local parent = root .. "/shared"
  assert(nvim.fn.mkdir(parent) == 1)
  assert(nvim.uv.fs_chmod(parent, 511))
  local path, err = export(record, { "observation:cwd" }, parent .. "/export.json")
  MiniTest.expect.equality(path, nil)
  MiniTest.expect.equality(err, "Evidence export directory must be operator-owned and not group/world writable")
end

T["pins relative paths to the invocation directory before asynchronous reads"] = function()
  local record, source = fixture()
  local stored = nvim.json.decode(table.concat(nvim.fn.readfile(record), "\n"))
  stored.evidence_sources[1].path = nvim.fs.basename(source)
  assert(nvim.fn.writefile({ nvim.json.encode(stored) }, record) == 0)
  local previous = nvim.fn.getcwd()
  MiniTest.finally(function()
    nvim.fn.chdir(previous)
  end)
  nvim.fn.chdir(root)
  local done, path, failure = false, nil, nil
  assert(
    require("louiselm.forensics.export").write(
      nvim.fs.basename(record),
      "relative.json",
      { "source:1:1:1" },
      function(result, err)
        done, path, failure = true, result, err
      end
    )
  )
  nvim.fn.chdir(previous)
  assert(nvim.wait(5000, function()
    return done
  end))
  MiniTest.expect.equality(failure, nil)
  MiniTest.expect.equality(path, root .. "/relative.json")
  MiniTest.expect.equality(nvim.json.decode(table.concat(nvim.fn.readfile(path), "\n")).items[1].state, "exported")
end

return T
