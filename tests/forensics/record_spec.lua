local MiniTest = require("mini.test")
local Record = require("louiselm.forensics.record")
local Store = require("louiselm.forensics.store")

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim
local temp_dir
local T = MiniTest.new_set({
  hooks = {
    pre_case = function()
      temp_dir = nvim.fn.tempname()
      assert(nvim.fn.mkdir(temp_dir, "p") == 1)
    end,
    post_case = function()
      nvim.fn.delete(temp_dir, "rf")
      temp_dir = nil
    end,
  },
})

local function input()
  return {
    id = "record-1",
    observed_at = 100,
    subject = { agent = "codex", acp_session_id = "acp-1" },
    diagnosing_session = "claude/acp-diagnoser",
    observations = {
      agent = "Codex",
      agent_version = "1.2.3\nsecret details",
      cwd = "/tmp/project",
      options = { model = "gpt-5", reasoning = true },
      capabilities = { load_session = true },
      dirty_files = { "a.lua", "b.lua" },
    },
    evidence_sources = {
      { kind = "acp_log", state = "present", path = "/tmp/session.log", mutable = true },
    },
  }
end

T["build"] = MiniTest.new_set()

T["build"]["creates a bounded allowlisted record"] = function()
  local record = assert(Record.build(input()))

  MiniTest.expect.equality(record.schema_version, 1)
  MiniTest.expect.equality(record.subject, { agent = "codex", acp_session_id = "acp-1" })
  MiniTest.expect.equality(record.observations.options, { model = "gpt-5", reasoning = true })
  MiniTest.expect.equality(record.observations.agent_version, "1.2.3\nsecret details")
  MiniTest.expect.equality(record.evidence_sources[1].mutable, true)
end

T["build"]["rejects missing subject identity"] = function()
  local value = input()
  value.subject.acp_session_id = nil

  local record, error_message = Record.build(value)

  MiniTest.expect.equality(record, nil)
  MiniTest.expect.equality(error_message, "forensics subject requires an ACP Session ID")
end

T["store"] = MiniTest.new_set()

T["store"]["publishes and reads an immutable private record"] = function()
  local store = assert(Store.new(nvim.fs.joinpath(temp_dir, "forensics")))
  local path = assert(store:write(input()))
  local loaded = assert(store:read(path))

  MiniTest.expect.equality(loaded.subject, { agent = "codex", acp_session_id = "acp-1" })
  MiniTest.expect.equality(nvim.uv.fs_stat(path).mode % 512, 384)

  loaded.subject.agent = "changed"
  local reread = assert(store:read(path))
  MiniTest.expect.equality(reread.subject.agent, "codex")
end

T["store"]["rejects a record replaced with a non-file before open"] = function()
  local store = assert(Store.new(nvim.fs.joinpath(temp_dir, "forensics")))
  local path = assert(store:write(input()))
  local content = table.concat(nvim.fn.readfile(path), "\n")
  local original_open = nvim.uv.fs_open
  local original_fstat = nvim.uv.fs_fstat
  local original_read = nvim.uv.fs_read
  local original_close = nvim.uv.fs_close
  local replacement_descriptor = 987654
  local open_flags
  MiniTest.finally(function()
    rawset(nvim.uv, "fs_open", original_open)
    rawset(nvim.uv, "fs_fstat", original_fstat)
    rawset(nvim.uv, "fs_read", original_read)
    rawset(nvim.uv, "fs_close", original_close)
  end)
  rawset(nvim.uv, "fs_open", function(target, flags, mode)
    if target == path then
      open_flags = flags
      return replacement_descriptor
    end
    return original_open(target, flags, mode)
  end)
  rawset(nvim.uv, "fs_fstat", function(file)
    if file == replacement_descriptor then
      return { type = "fifo", size = #content }
    end
    return original_fstat(file)
  end)
  rawset(nvim.uv, "fs_read", function(file, size, offset)
    if file == replacement_descriptor then
      return content
    end
    return original_read(file, size, offset)
  end)
  rawset(nvim.uv, "fs_close", function(file)
    if file == replacement_descriptor then
      return true
    end
    return original_close(file)
  end)

  local loaded, read_error = store:read(path)

  MiniTest.expect.equality(open_flags, nvim.uv.constants.O_RDONLY + nvim.uv.constants.O_NONBLOCK)
  MiniTest.expect.equality(loaded, nil)
  MiniTest.expect.equality(read_error, "forensics record is not a regular file")
end

T["store"]["inspects current evidence availability by property without changing the record"] = function()
  local store = assert(Store.new(nvim.fs.joinpath(temp_dir, "forensics")))
  local transcript_path = nvim.fs.joinpath(temp_dir, "agent-transcript.jsonl")
  local missing_log_path = nvim.fs.joinpath(temp_dir, "missing-acp-log.jsonl")
  assert(nvim.fn.writefile({ '{"message":"available"}' }, transcript_path) == 0)
  local value = input()
  value.evidence_sources = {
    { kind = "agent_transcript", state = "present", path = transcript_path, mutable = true },
    { kind = "acp_log", state = "present", path = missing_log_path, mutable = true },
    { kind = "git", state = "present", mutable = true },
  }
  local path = assert(store:write(value))
  local before = assert(nvim.uv.fs_stat(path))
  local before_lines = nvim.fn.readfile(path)

  local inspected = assert(store:inspect(path))

  MiniTest.expect.equality(inspected.evidence_availability, {
    conversation_content = "available",
    repository_state = "available",
    wire_ordering = "missing",
  })
  MiniTest.expect.equality(nvim.fn.readfile(path), before_lines)
  MiniTest.expect.equality(nvim.fn.readfile(transcript_path), { '{"message":"available"}' })
  MiniTest.expect.equality(assert(nvim.uv.fs_stat(path)).mtime, before.mtime)
  MiniTest.expect.equality(assert(store:read(path)).evidence_sources[2].state, "present")
end

T["store"]["reports an existing referent that cannot be opened as unreadable"] = function()
  local store = assert(Store.new(nvim.fs.joinpath(temp_dir, "forensics")))
  local unreadable_path = nvim.fs.joinpath(temp_dir, "unreadable-acp-log.jsonl")
  assert(nvim.fn.writefile({ "{}" }, unreadable_path) == 0)
  local value = input()
  value.evidence_sources[1].path = unreadable_path
  local path = assert(store:write(value))
  local original_open = nvim.uv.fs_open
  MiniTest.finally(function()
    rawset(nvim.uv, "fs_open", original_open)
  end)
  rawset(nvim.uv, "fs_open", function(target, flags, mode)
    if target == unreadable_path then
      return nil, "EACCES: permission denied", "EACCES"
    end
    return original_open(target, flags, mode)
  end)

  local inspected = assert(store:inspect(path))

  MiniTest.expect.equality(inspected.evidence_availability, {
    conversation_content = "unreadable",
    wire_ordering = "unreadable",
  })
end

T["store"]["reports a referent removed before open as missing"] = function()
  local store = assert(Store.new(nvim.fs.joinpath(temp_dir, "forensics")))
  local removed_path = nvim.fs.joinpath(temp_dir, "removed-acp-log.jsonl")
  assert(nvim.fn.writefile({ "{}" }, removed_path) == 0)
  local value = input()
  value.evidence_sources[1].path = removed_path
  local path = assert(store:write(value))
  local original_open = nvim.uv.fs_open
  MiniTest.finally(function()
    rawset(nvim.uv, "fs_open", original_open)
  end)
  rawset(nvim.uv, "fs_open", function(target, flags, mode)
    if target == removed_path then
      return nil, "ENOENT: no such file or directory", "ENOENT"
    end
    return original_open(target, flags, mode)
  end)

  local inspected = assert(store:inspect(path))

  MiniTest.expect.equality(inspected.evidence_availability, {
    conversation_content = "missing",
    wire_ordering = "missing",
  })
end

T["store"]["reports a referent replaced before open as unreadable"] = function()
  local store = assert(Store.new(nvim.fs.joinpath(temp_dir, "forensics")))
  local replaced_path = nvim.fs.joinpath(temp_dir, "replaced-acp-log.jsonl")
  assert(nvim.fn.writefile({ "{}" }, replaced_path) == 0)
  local value = input()
  value.evidence_sources[1].path = replaced_path
  local path = assert(store:write(value))
  local original_open = nvim.uv.fs_open
  local original_fstat = nvim.uv.fs_fstat
  local original_close = nvim.uv.fs_close
  local replacement_descriptor = 987654
  local open_flags
  MiniTest.finally(function()
    rawset(nvim.uv, "fs_open", original_open)
    rawset(nvim.uv, "fs_fstat", original_fstat)
    rawset(nvim.uv, "fs_close", original_close)
  end)
  rawset(nvim.uv, "fs_open", function(target, flags, mode)
    if target == replaced_path then
      open_flags = flags
      return replacement_descriptor
    end
    return original_open(target, flags, mode)
  end)
  rawset(nvim.uv, "fs_fstat", function(file)
    if file == replacement_descriptor then
      return { type = "fifo" }
    end
    return original_fstat(file)
  end)
  rawset(nvim.uv, "fs_close", function(file)
    if file == replacement_descriptor then
      return true
    end
    return original_close(file)
  end)

  local inspected = assert(store:inspect(path))

  MiniTest.expect.equality(open_flags, nvim.uv.constants.O_RDONLY + nvim.uv.constants.O_NONBLOCK)
  MiniTest.expect.equality(inspected.evidence_availability, {
    conversation_content = "unreadable",
    wire_ordering = "unreadable",
  })
end

T["store"]["does not let an unknown source kind claim a known evidence property"] = function()
  local store = assert(Store.new(nvim.fs.joinpath(temp_dir, "forensics")))
  local unknown_path = nvim.fs.joinpath(temp_dir, "unknown.jsonl")
  local missing_log_path = nvim.fs.joinpath(temp_dir, "missing-acp-log.jsonl")
  assert(nvim.fn.writefile({ "{}" }, unknown_path) == 0)
  local value = input()
  value.evidence_sources = {
    { kind = "acp_log", state = "present", path = missing_log_path, mutable = true },
    { kind = "conversation_content", state = "present", path = unknown_path, mutable = true },
  }
  local path = assert(store:write(value))

  local inspected = assert(store:inspect(path))

  MiniTest.expect.equality(inspected.evidence_availability, {
    conversation_content = "missing",
    wire_ordering = "missing",
  })
end

return T
