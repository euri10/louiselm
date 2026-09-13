local Format = require("louiselm.forensics.export_format")
local Record = require("louiselm.forensics.record")
---@diagnostic disable-next-line: undefined-global -- Neovim runtime.
local nvim = vim
local uv = nvim.uv
local M = {}
local MAX_RECORD = 262144
local MAX_SOURCE = 1048576
local MAX_OUTPUT = 262144

---@alias louiselm.forensics.ExportCallback fun(path: string?, error_message: string?)

---Export explicitly selected observations or JSONL ranges to a new private file.
---All I/O is asynchronous; callback runs once on the editor loop. Source records
---are read-only. Parent directories must exist. Returns cancellation owned by the
---caller; cancellation before publication leaves no output, but cannot undo an
---already admitted publication. A path plus error means published with cleanup trouble.
---@param record_path string Forensics record path.
---@param output_path string New destination; existing files are never replaced.
---@param selectors string[] observation:FIELD or source:INDEX:FIRST:LAST.
---@param callback louiselm.forensics.ExportCallback Completion, including cancellation.
---@return fun()? cancel
---@return string? error_message Invalid arguments; no work started.
function M.write(record_path, output_path, selectors, callback)
  for _, path in ipairs({ record_path, output_path }) do
    if type(path) ~= "string" or path == "" or #path > 4096 or path:find("%z") then
      return nil, "record and output paths must be non-empty bounded paths"
    end
  end
  if type(record_path) ~= "string" or type(output_path) ~= "string" or type(callback) ~= "function" then
    return nil, "record, output, selections and callback are required"
  end
  local selections, selection_error = Format.selections(selectors)
  if not selections then
    return nil, selection_error
  end
  record_path, output_path = nvim.fs.abspath(record_path), nvim.fs.abspath(output_path)
  local invocation_directory = uv.cwd()
  local cancelled, finished = false, false
  local descriptor, temporary
  local worker, resume

  -- One coroutine owns the operation's descriptors. Every libuv continuation is
  -- marshalled to the editor loop, including failures and final cleanup.
  local function await(operation)
    operation(function(err, value, extra)
      nvim.schedule(function()
        resume(err, value, extra)
      end)
    end)
    return coroutine.yield()
  end

  local function read(path, limit)
    local path_error, before = await(function(done)
      uv.fs_lstat(path, done)
    end)
    if path_error then
      return nil, (path_error:match("^ENOENT") or path_error:match("^ENOTDIR")) and "missing" or "unreadable"
    end
    if before.type ~= "file" then
      return nil, "unreadable"
    end
    local err, file = await(function(done)
      uv.fs_open(path, uv.constants.O_RDONLY + uv.constants.O_NONBLOCK, 384, done)
    end)
    if err then
      return nil, (err:match("^ENOENT") or err:match("^ENOTDIR")) and "missing" or "unreadable"
    end
    descriptor = file
    local stat_error, stat = await(function(done)
      uv.fs_fstat(file, done)
    end)
    local content, read_error
    if not stat_error and stat.type == "file" and stat.dev == before.dev and stat.ino == before.ino then
      read_error, content = await(function(done)
        uv.fs_read(file, math.min(stat.size, limit + 1), 0, done)
      end)
    end
    local after_error, after = await(function(done)
      uv.fs_fstat(file, done)
    end)
    local close_error = await(function(done)
      uv.fs_close(file, done)
    end)
    descriptor = nil
    if stat_error or not stat or not content or read_error or close_error or after_error then
      return nil, "unreadable"
    end
    if stat.size ~= after.size or stat.mtime.sec ~= after.mtime.sec or stat.mtime.nsec ~= after.mtime.nsec then
      return nil, "changed"
    end
    if #content ~= math.min(stat.size, limit + 1) then
      return nil, "unreadable"
    end
    return content
  end

  local function source_item(record, selection)
    local source = record.evidence_sources[selection.source]
    local item = { selection = selection.selector, state = "missing" }
    if not source then
      return item
    end
    if source.kind ~= "acp_log" and source.kind ~= "agent_transcript" then
      item.state = "unsupported"
      return item
    end
    if not source.path then
      if source.state == "inaccessible" then
        item.state = "unreadable"
      end
      return item
    end
    local source_path = source.path:sub(1, 1) == "/" and source.path
      or nvim.fs.joinpath(invocation_directory, source.path)
    local bytes, err = read(source_path, MAX_SOURCE)
    if not bytes then
      item.state = err
      return item
    end
    local lines, start, number = {}, 1, 1
    while start <= #bytes and number <= selection.last do
      local boundary = bytes:find("\n", start, true)
      local stop = boundary and boundary - 1 or #bytes
      if number >= selection.first then
        if stop - start + 1 > 65536 or (not boundary and #bytes > MAX_SOURCE) then
          item.state = "limit_exceeded"
          return item
        end
        local decoded, value = pcall(nvim.json.decode, bytes:sub(start, stop))
        if not decoded then
          item.state = "invalid_json"
          return item
        end
        lines[#lines + 1] = { line = number, value = Format.redact(value) }
      end
      start, number = stop + 2, number + 1
    end
    if number <= selection.last then
      item.state = #bytes > MAX_SOURCE and "limit_exceeded" or "missing"
      return item
    end
    item.state, item.lines = "exported", lines
    return item
  end

  local function run()
    if cancelled then
      return nil, "evidence export cancelled"
    end
    local parent_error, parent = await(function(done)
      uv.fs_lstat(nvim.fs.dirname(output_path), done)
    end)
    if parent_error or parent.type ~= "directory" then
      return nil, "Evidence export directory must already exist and not be a symlink"
    end
    if parent.uid ~= uv.getuid() or math.floor(parent.mode / 16) % 2 ~= 0 or math.floor(parent.mode / 2) % 2 ~= 0 then
      return nil, "Evidence export directory must be operator-owned and not group/world writable"
    end
    local bytes, read_error = read(record_path, MAX_RECORD)
    if not bytes then
      return nil, "Forensics record is " .. read_error
    end
    if #bytes > MAX_RECORD then
      return nil, "Forensics record exceeds 256 KiB"
    end
    local decoded, value = pcall(nvim.json.decode, bytes)
    if not decoded then
      return nil, "Forensics record is invalid JSON"
    end
    if type(value) ~= "table" or value.schema_version ~= Record.version() then
      return nil, "unsupported Forensics record schema"
    end
    local record = Record.build(value)
    if not record then
      return nil, "invalid Forensics record"
    end
    local artifact = { schema_version = 1, kind = "evidence_export", redaction = "structure-only-v1", items = {} }
    for _, selection in ipairs(selections) do
      if cancelled then
        return nil, "evidence export cancelled"
      end
      local item
      if selection.observation then
        local observation = record.observations[selection.observation]
        item = {
          selection = selection.selector,
          state = observation == nil and "missing" or "exported",
          value = observation ~= nil and Format.redact(observation) or nil,
        }
      else
        item = source_item(record, selection)
      end
      artifact.items[#artifact.items + 1] = item
    end
    local encoded, content = pcall(nvim.json.encode, artifact)
    if not encoded then
      return nil, "could not encode Evidence export"
    end
    if #content > MAX_OUTPUT then
      return nil, "Evidence export exceeds 256 KiB; select less evidence"
    end
    if cancelled then
      return nil, "evidence export cancelled"
    end
    local create_error, file, path = await(function(done)
      uv.fs_mkstemp(output_path .. ".tmp-XXXXXX", done)
    end)
    if create_error then
      return nil, "could not create private Evidence export"
    end
    descriptor, temporary = file, path
    local write_error, size = await(function(done)
      uv.fs_write(file, content, 0, done)
    end)
    if write_error or size ~= #content then
      return nil, "could not write Evidence export"
    end
    if await(function(done)
      uv.fs_fsync(file, done)
    end) then
      return nil, "could not sync Evidence export"
    end
    local close_error = await(function(done)
      uv.fs_close(file, done)
    end)
    descriptor = nil
    if close_error then
      return nil, "could not close Evidence export"
    end
    if cancelled then
      return nil, "evidence export cancelled"
    end
    -- Atomic no-replace publication also refuses aliases of the source record.
    if await(function(done)
      uv.fs_link(temporary, output_path, done)
    end) then
      return nil, "could not publish Evidence export (destination must be new)"
    end
    return output_path
  end

  local function finish(path, err)
    if finished then
      return
    end
    finished = true
    local function remove_temporary()
      if temporary then
        uv.fs_unlink(temporary, function(unlink_error)
          nvim.schedule(function()
            callback(path, unlink_error and "Evidence export temporary cleanup failed" or err)
          end)
        end)
      else
        callback(path, err)
      end
    end
    if descriptor then
      uv.fs_close(descriptor, function(close_error)
        nvim.schedule(function()
          if close_error then
            err = "Evidence export descriptor cleanup failed"
          end
          remove_temporary()
        end)
      end)
    else
      remove_temporary()
    end
  end
  worker = coroutine.create(run)
  resume = function(...)
    local ok, path, err = coroutine.resume(worker, ...)
    if not ok then
      finish(nil, "Evidence export failed internally")
    elseif coroutine.status(worker) == "dead" then
      finish(path, err)
    end
  end
  nvim.schedule(function()
    resume()
  end)
  return function()
    cancelled = true
  end
end

return M
