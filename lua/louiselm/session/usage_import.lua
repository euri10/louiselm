---@diagnostic disable-next-line: undefined-global -- Neovim owns filesystem and scheduling.
local nvim = vim
local Validation = require("louiselm.session.validation")
local Stream = require("louiselm.session.usage_import_stream")

---@class louiselm.session.ImportProposal
---@field agent string Proposed configured Agent; never inferred from the log.
---@field provider string Proposed access/quota Provider; never inferred from the Model.

---@class louiselm.session.ImportSource
---@field kind "acp"|"native"
---@field path? string Explicit unsupported native source path.
---@field session_id? string ACP Session identifier for an ACP source.
---@field connection_id? string Identifier of one connection observation stream.
---@field connection_path? string Absolute path of its connection JSONL.
---@field session_path? string Absolute path of the paired Session JSONL.
---@field first_line? integer First Session record in this connection segment, inclusive.
---@field last_line? integer Last Session record in this connection segment, inclusive.
---@field proposal? louiselm.session.ImportProposal Unconfirmed human proposal.

---@class louiselm.session.ImportSelection
---@field from string Inclusive UTC dispatch time.
---@field until_time string Exclusive UTC dispatch time.
---@field max_files integer Maximum distinct files, at most 32.
---@field max_bytes integer Maximum combined file bytes, at most 8388608.
---@field sources louiselm.session.ImportSource[] Explicit file and line selections, at most 16.

---@class louiselm.session.ImportError
---@field code "invalid"|"timeout"|"unavailable"
---@field message string Sanitized error; no source contents.

---@class louiselm.session.ImportEvidence
---@field source integer One-based index into the selected sources.
---@field file "connection"|"session" Which selected file contains this observation.
---@field line integer One-based JSONL line number.

---@class louiselm.session.ImportFile
---@field path string Explicit selected path.
---@field status string Read status, including missing, changed, unreadable, unsupported, or byte_limit.
---@field bytes? integer Bytes read, when available.
---@field sha256? string Digest of the entire file as read, when available.

---@class louiselm.session.ImportSourceResult
---@field kind "acp"|"native"
---@field status string Source status; excluded sources have a reason in the exclusions list.
---@field path? string Explicit unsupported native source.
---@field session_id? string Selected ACP Session.
---@field connection_id? string Selected connection file identity.
---@field connection? louiselm.session.ImportFile Connection file evidence.
---@field session? louiselm.session.ImportFile Session file evidence.
---@field first_line? integer Selected inclusive Session line.
---@field last_line? integer Selected inclusive Session line.
---@field proposal? louiselm.session.ImportAttribution Unconfirmed proposed Agent and Provider.

---@class louiselm.session.ImportAttribution: louiselm.session.ImportProposal
---@field confirmation "unconfirmed" Always unconfirmed in this preview.

---@class louiselm.session.ImportCostObservation
---@field observed_at string UTC source timestamp.
---@field cost louiselm.session.Cost|userdata Cumulative amount or explicit JSON null clearing.
---@field evidence louiselm.session.ImportEvidence Source observation.

---@class louiselm.session.ImportCandidateEvidence
---@field dispatch louiselm.session.ImportEvidence Prompt dispatch.
---@field options louiselm.session.ImportEvidence Last complete starting options.
---@field binding louiselm.session.ImportEvidence Connection-to-Session binding.
---@field previous_exit? louiselm.session.ImportEvidence Preceding connection's exit, when resuming a Session file.
---@field cost_baseline? louiselm.session.ImportEvidence Last cumulative cost before dispatch.
---@field response? louiselm.session.ImportEvidence Received response, even when malformed.
---@field outcome? louiselm.session.ImportEvidence Valid terminal response.

---@class louiselm.session.ImportCandidate
---@field session_id string Observed ACP Session identifier.
---@field connection_id string Selected connection file identity.
---@field request_id string|number Request ID scoped to this connection and dispatch evidence.
---@field dispatched_at string UTC prompt-dispatch timestamp.
---@field options table<string, string|boolean> Complete supported starting tuple.
---@field model? string|boolean Observed model-category value.
---@field cost_baseline? louiselm.session.Cost Last observed cumulative cost before dispatch.
---@field costs louiselm.session.ImportCostObservation[] Observed cumulative readings and clearings.
---@field mixed boolean A confirmed option replacement occurred during this turn.
---@field options_uncertain? boolean A malformed option update prevented a complete tuple.
---@field telemetry_gap? boolean Some cost telemetry was malformed or omitted by a report bound.
---@field costs_truncated? boolean The per-turn cost observation limit was reached.
---@field peer_response? boolean A matching terminal response was observed.
---@field outcome? "completed"|"cancelled"|"failed" Absent when completion was not established.
---@field stop_reason? string Valid observed ACP stop reason, when supplied.
---@field usage? louiselm.session.TurnUsage Reported fields only; never estimated.
---@field proposal louiselm.session.ImportAttribution Unconfirmed human attribution.
---@field evidence louiselm.session.ImportCandidateEvidence Source references.

---@class louiselm.session.ImportTransition
---@field session_id string Observed ACP Session identifier.
---@field connection_id string Selected connection file identity.
---@field observed_at string UTC source timestamp.
---@field previous_options table<string, string|boolean> Complete tuple before replacement.
---@field options table<string, string|boolean> Complete tuple after replacement.
---@field source "response"|"notification" Observed source; does not assert causation.
---@field request? { id: string|number, option: string, value: string|boolean } Matched request, only for a response.
---@field request_evidence? louiselm.session.ImportEvidence Matched request location.
---@field evidence louiselm.session.ImportEvidence Replacement location.

---@class louiselm.session.ImportExclusion
---@field source integer One-based index into the selected sources.
---@field reason string Fixed, payload-free reason code.
---@field evidence? louiselm.session.ImportEvidence Offending source record, when known.

---@class louiselm.session.ImportBatch
---@field source integer One-based index of the exact selected source segment.
---@field session_id string Selected ACP Session identifier.
---@field connection_id string Selected connection file identity.
---@field from string Inclusive UTC dispatch bound.
---@field until_time string Exclusive UTC dispatch bound.
---@field proposal louiselm.session.ImportAttribution Unconfirmed human attribution.
---@field status string Source status for this proposed batch.
---@field candidates integer Admitted preview candidates in this segment.
---@field transitions integer Confirmed option replacements in this segment.
---@field exclusions integer Number of exclusion reasons in this segment.
---@field binding? louiselm.session.ImportEvidence Observed binding, when validated.

---@class louiselm.session.ImportReport
---@field selection louiselm.session.ImportSelection Frozen explicit selection.
---@field attribution_confirmation "unconfirmed" All selected attribution remains unconfirmed.
---@field digest string SHA-256 binding selection, source statuses and selected file bytes.
---@field sources louiselm.session.ImportSourceResult[] Per-source status and file evidence.
---@field batches louiselm.session.ImportBatch[] Proposed attribution batches with exact source scope.
---@field candidates louiselm.session.ImportCandidate[] Observed dispatches with normalized, sparse facts.
---@field transitions louiselm.session.ImportTransition[] Confirmed option replacements, without inferred causality.
---@field exclusions louiselm.session.ImportExclusion[] At most 256 bounded reasons and locations.
---@field counts { candidates: integer, exclusions: integer, transitions: integer } Exclusions counts all reasons, including omitted ones.

---@alias louiselm.session.ImportCallback fun(report: louiselm.session.ImportReport?, error: louiselm.session.ImportError?)

local M = {}
local MAX_FILES, MAX_BYTES, MAX_SOURCES = 32, 8 * 1024 * 1024, 16
local TIMEOUT_MS = 5000

local function fields(value, allowed)
  if type(value) ~= "table" then
    return false
  end
  for key in pairs(value) do
    if not allowed[key] then
      return false
    end
  end
  return true
end

local function string_value(value, limit)
  return type(value) == "string" and #value > 0 and #value <= limit and not value:find("[%z\1-\31\127]")
end

local function path(value)
  return string_value(value, 4096) and value:sub(1, 1) == "/"
end

local function integer(value, lower, upper)
  return type(value) == "number" and value % 1 == 0 and value >= lower and value <= upper
end

local selection_fields = { from = true, until_time = true, max_files = true, max_bytes = true, sources = true }
local acp_fields = {
  kind = true,
  session_id = true,
  connection_id = true,
  connection_path = true,
  session_path = true,
  first_line = true,
  last_line = true,
  proposal = true,
}
local native_fields = { kind = true, path = true }
local proposal_fields = { agent = true, provider = true }

local function valid(selection)
  if not fields(selection, selection_fields) then
    return false
  end
  local from, until_time = Validation.timestamp_key(selection.from), Validation.timestamp_key(selection.until_time)
  if
    not from
    or not until_time
    or from >= until_time
    or not integer(selection.max_files, 1, MAX_FILES)
    or not integer(selection.max_bytes, 1, MAX_BYTES)
    or type(selection.sources) ~= "table"
    or #selection.sources < 1
    or #selection.sources > MAX_SOURCES
    or not nvim.islist(selection.sources)
  then
    return false
  end
  local connection_ids, connection_paths, session_agents, session_paths, ranges, file_paths = {}, {}, {}, {}, {}, {}
  for _, source in ipairs(selection.sources) do
    if type(source) ~= "table" then
      return false
    end
    if source.kind == "native" then
      if not fields(source, native_fields) or not path(source.path) then
        return false
      end
    elseif source.kind == "acp" then
      if
        not fields(source, acp_fields)
        or not string_value(source.session_id, 512)
        or not string_value(source.connection_id, 512)
        or not path(source.connection_path)
        or not source.connection_id:match("^[%w_.-]+$")
        or source.connection_id == "."
        or source.connection_id == ".."
        or nvim.fs.basename(source.connection_path) ~= source.connection_id .. ".jsonl"
        or not path(source.session_path)
        or source.connection_path == source.session_path
        or not integer(source.first_line, 1, 1000000)
        or not integer(source.last_line, source.first_line, 1000000)
        or source.last_line - source.first_line >= 10000
        or not fields(source.proposal, proposal_fields)
        or not string_value(source.proposal.agent, 128)
        or not string_value(source.proposal.provider, 128)
        or connection_ids[source.connection_id]
      then
        return false
      end
      if session_agents[source.session_id] and session_agents[source.session_id] ~= source.proposal.agent then
        return false
      end
      if session_paths[source.session_id] and session_paths[source.session_id] ~= source.session_path then
        return false
      end
      connection_ids[source.connection_id] = true
      connection_paths[source.connection_path] = true
      session_agents[source.session_id] = source.proposal.agent
      session_paths[source.session_id] = source.session_path
      ranges[source.session_path] = ranges[source.session_path] or {}
      for _, range in ipairs(ranges[source.session_path]) do
        if
          range.session_id ~= source.session_id or (source.first_line <= range.last and source.last_line >= range.first)
        then
          return false
        end
      end
      ranges[source.session_path][#ranges[source.session_path] + 1] =
        { first = source.first_line, last = source.last_line, session_id = source.session_id }
      file_paths[source.connection_path], file_paths[source.session_path] = true, true
    else
      return false
    end
  end
  local count = 0
  for _, filename in pairs(session_paths) do
    if connection_paths[filename] then
      return false
    end
  end
  for _ in pairs(file_paths) do
    count = count + 1
  end
  return count <= selection.max_files
end

local function same_file(a, b)
  return a
    and b
    and a.type == "file"
    and b.type == "file"
    and a.dev == b.dev
    and a.ino == b.ino
    and a.size == b.size
    and a.mtime.sec == b.mtime.sec
    and a.mtime.nsec == b.mtime.nsec
    and a.ctime.sec == b.ctime.sec
    and a.ctime.nsec == b.ctime.nsec
end

-- Every completion crosses the main-loop boundary; open descriptors are closed
-- even when the owner has cancelled or a timeout has already fired.
local function read_file(filename, remaining, callback)
  local uv = nvim.uv
  uv.fs_lstat(
    filename,
    nvim.schedule_wrap(function(stat_error, before)
      if stat_error or not before then
        callback(nil, stat_error and stat_error:match("ENOENT") and "missing" or "unreadable")
        return
      end
      if before.type ~= "file" then
        callback(nil, "unsupported")
        return
      end
      if before.size > remaining then
        callback(nil, "byte_limit")
        return
      end
      -- A raced replacement with a FIFO must not block a libuv worker on open.
      local flags = uv.constants.O_RDONLY + (uv.constants.O_NONBLOCK or 0)
      uv.fs_open(
        filename,
        flags,
        0,
        nvim.schedule_wrap(function(open_error, fd)
          if open_error or not fd then
            callback(nil, "unreadable")
            return
          end
          local function finish(bytes, status)
            uv.fs_close(
              fd,
              nvim.schedule_wrap(function(close_error)
                if close_error then
                  callback(nil, "unreadable")
                else
                  callback(bytes, status)
                end
              end)
            )
          end
          uv.fs_fstat(
            fd,
            nvim.schedule_wrap(function(open_stat_error, opened)
              if open_stat_error or not same_file(before, opened) then
                finish(nil, "changed")
                return
              end
              uv.fs_read(
                fd,
                before.size,
                0,
                nvim.schedule_wrap(function(read_error, bytes)
                  if read_error or bytes == nil then
                    finish(nil, "unreadable")
                    return
                  end
                  uv.fs_fstat(
                    fd,
                    nvim.schedule_wrap(function(end_error, after)
                      if end_error or not same_file(before, after) or #bytes ~= before.size then
                        finish(nil, "changed")
                        return
                      end
                      uv.fs_lstat(
                        filename,
                        nvim.schedule_wrap(function(path_error, current)
                          if path_error or not same_file(before, current) then
                            finish(nil, "changed")
                            return
                          end
                          finish(bytes, "ok")
                        end)
                      )
                    end)
                  )
                end)
              )
            end)
          )
        end)
      )
    end)
  )
end

local function fingerprint(selection, files)
  local parts = {}
  local function add(value)
    local s = tostring(value)
    parts[#parts + 1] = #s .. ":" .. s
  end
  add("louiselm-acp-usage-preview-v1")
  add(selection.from)
  add(selection.until_time)
  add(selection.max_files)
  add(selection.max_bytes)
  for _, source in ipairs(selection.sources) do
    add(source.kind)
    if source.kind == "native" then
      add(source.path)
      add("unsupported")
    else
      for _, key in ipairs({
        "session_id",
        "connection_id",
        "connection_path",
        "session_path",
        "first_line",
        "last_line",
      }) do
        add(source[key])
      end
      add(source.proposal.agent)
      add(source.proposal.provider)
      for _, filename in ipairs({ source.connection_path, source.session_path }) do
        local file = files[filename]
        add(file.status)
        add(file.sha256 or "")
      end
    end
  end
  return "sha256:" .. nvim.fn.sha256(table.concat(parts))
end

---Preview explicitly selected historical ACP wire records without writing usage.
---The callback runs once on the main loop, unless cancelled. The returned cancel
---function suppresses delivery; already-started bounded reads still close their files.
---@param selection louiselm.session.ImportSelection Exact source, line, time and resource bounds.
---@param callback louiselm.session.ImportCallback Report or sanitized validation/timeout failure.
---@return fun() cancel Suppresses the callback without changing any source or store.
function M.preview(selection, callback)
  if type(callback) ~= "function" then
    error("import preview requires a callback")
  end
  local valid_selection = valid(selection)
  local frozen = valid_selection and nvim.deepcopy(selection) or nil
  local cancelled, done = false, false
  local timer = nvim.uv.new_timer()
  local function finish(report, err)
    if done then
      return
    end
    done = true
    if timer then
      timer:stop()
      timer:close()
    end
    if not cancelled then
      callback(report, err)
    end
  end
  nvim.schedule(function()
    if cancelled then
      finish()
      return
    end
    if not timer then
      finish(nil, { code = "unavailable", message = "Import preview timer is unavailable" })
      return
    end
    if not valid_selection or not frozen then
      finish(nil, { code = "invalid", message = "Invalid import selection or resource bounds" })
      return
    end
    local filenames, seen = {}, {}
    for _, source in ipairs(frozen.sources) do
      if source.kind == "acp" then
        for _, filename in ipairs({ source.connection_path, source.session_path }) do
          if not seen[filename] then
            filenames[#filenames + 1], seen[filename] = filename, true
          end
        end
      end
    end
    local files, index, bytes_used = {}, 1, 0
    local function next_file()
      if done then
        return
      end
      if cancelled then
        finish()
        return
      end
      local filename = filenames[index]
      if not filename then
        Stream.report(frozen, files, function(report)
          if not done then
            report.digest = fingerprint(frozen, files)
            finish(report)
          end
        end, function()
          return done or cancelled
        end)
        return
      end
      index = index + 1
      read_file(filename, frozen.max_bytes - bytes_used, function(bytes, status)
        if done then
          return
        end
        if bytes and bytes:find("\0", 1, true) then
          bytes, status = nil, "malformed"
        end
        files[filename] = { status = status, bytes = bytes, sha256 = bytes and nvim.fn.sha256(bytes) or nil }
        if bytes then
          bytes_used = bytes_used + #bytes
        end
        next_file()
      end)
    end
    next_file()
  end)
  if timer then
    local started = timer:start(
      TIMEOUT_MS,
      0,
      nvim.schedule_wrap(function()
        finish(nil, { code = "timeout", message = "Import preview exceeded its time limit" })
      end)
    )
    if not started then
      nvim.schedule(function()
        finish(nil, { code = "unavailable", message = "Import preview timer is unavailable" })
      end)
    end
  end
  return function()
    cancelled = true
    finish()
  end
end

return M
