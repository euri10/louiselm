---Bounded export provenance derived from the Control broker, never from Agent content.
---@diagnostic disable-next-line: undefined-global -- Neovim runtime.
local nvim = vim
local M = {}

---@alias louiselm.OutputProvenanceCode "session_output_tainted"|"untainted"|"not_managed"|"unknown"

---@class louiselm.OutputProvenance
---@field schema "louiselm.session.output-provenance/1"
---@field code louiselm.OutputProvenanceCode
---@field taint_digest string|userdata JSON null unless tainted.
---@field clean_review_refs string[] Empty until exact-use acceptance exists.

---@class louiselm.ProvenanceBinding
---@field kind "broker"|"not_managed"
---@field session_id? string Private Control broker Session ID, never exported.

local SCHEMA = "louiselm.session.output-provenance/1"
local BROKER_SCHEMA = "louiselm.workspace.output-provenance/1"
local MARKDOWN_SCHEMA = "louiselm.transcript-provenance/1"
local COMMAND = "/usr/local/lib/louiselm/current/bin/louiselm-control"
local LIMIT = 1048576

---@param value unknown
---@return boolean
local function identifier(value)
  return type(value) == "string" and #value > 0 and #value <= 128 and value:match("^[A-Za-z0-9_%-]+$") ~= nil
end

---@param value unknown
---@return boolean
local function digest(value)
  return type(value) == "string" and value:match("^sha256:[0-9a-f]+$") ~= nil and #value == 71
end

---@param value unknown
---@param keys table<string, boolean>
---@return boolean
local function exact(value, keys)
  if type(value) ~= "table" then
    return false
  end
  for key in pairs(keys) do
    if value[key] == nil then
      return false
    end
  end
  for key in pairs(value) do
    if not keys[key] then
      return false
    end
  end
  return true
end

local PROJECTION_KEYS = { schema = true, code = true, taint_digest = true, clean_review_refs = true }

---@param code louiselm.OutputProvenanceCode
---@param taint_digest? string
---@return louiselm.OutputProvenance
local function projection(code, taint_digest)
  return {
    schema = SCHEMA,
    code = code,
    taint_digest = taint_digest or nvim.NIL,
    clean_review_refs = {},
  }
end

---Return the fail-closed portable state.
---@return louiselm.OutputProvenance
function M.unknown()
  return projection("unknown")
end

---Validate a private broker binding; absent bindings are legacy/unknown.
---@param binding unknown
---@return boolean valid
function M.valid_binding(binding)
  if exact(binding, { kind = true }) then
    return binding.kind == "not_managed"
  end
  return exact(binding, { kind = true, session_id = true })
    and binding.kind == "broker"
    and identifier(binding.session_id)
end

---Initial state before a current broker read; only explicit non-management is known.
---@param binding unknown
---@return louiselm.OutputProvenance
function M.initial(binding)
  if M.valid_binding(binding) and binding.kind == "not_managed" then
    return projection("not_managed")
  end
  return M.unknown()
end

---Reject unknown fields, contradictory digests and unsupported acceptance refs.
---@param value unknown
---@return boolean valid
function M.valid(value)
  if not exact(value, PROJECTION_KEYS) or value.schema ~= SCHEMA then
    return false
  end
  if type(value.clean_review_refs) ~= "table" or next(value.clean_review_refs) ~= nil then
    return false
  end
  if value.code == "session_output_tainted" then
    return digest(value.taint_digest)
  end
  return (value.code == "untainted" or value.code == "not_managed" or value.code == "unknown")
    and value.taint_digest == nvim.NIL
end

---Normalize untrusted or stale portable metadata without granting clean status.
---@param value unknown
---@return louiselm.OutputProvenance
function M.normalize(value)
  if not M.valid(value) then
    return M.unknown()
  end
  return projection(value.code, value.code == "session_output_tainted" and value.taint_digest or nil)
end

---Project only validated broker provenance for the exact requested Session.
---@param value unknown Decoded `session retention ID --json` response.
---@param session_id string Requested broker Session ID.
---@return louiselm.OutputProvenance
function M.from_broker(value, session_id)
  if
    type(value) ~= "table"
    or type(value.record) ~= "table"
    or type(value.record.launch) ~= "table"
    or value.record.launch.session_id ~= session_id
    or type(value.quarantined) ~= "boolean"
  then
    return M.unknown()
  end
  local source = value.output_provenance
  if not exact(source, PROJECTION_KEYS) or source.schema ~= BROKER_SCHEMA then
    return M.unknown()
  end
  if type(source.clean_review_refs) ~= "table" or next(source.clean_review_refs) ~= nil then
    return M.unknown()
  end
  if source.code == "session_output_tainted" and value.quarantined and digest(source.taint_digest) then
    return projection("session_output_tainted", source.taint_digest)
  end
  if source.code == "untainted" and not value.quarantined and source.taint_digest == nvim.NIL then
    return projection("untainted")
  end
  return M.unknown()
end

---@param session_id string
---@param callback fun(payload: string?)
---@return fun() cancel
local function fetch_broker(session_id, callback)
  local chunks, size, failed = {}, 0, false
  local process
  local ok, result = pcall(nvim.system, { COMMAND, "session", "retention", session_id, "--json" }, {
    cwd = "/",
    env = {},
    clear_env = true,
    text = true,
    timeout = 30000,
    stderr = false,
    stdout = function(err, data)
      if err then
        failed = true
      elseif data ~= nil then
        size = size + #data
        if size > LIMIT then
          failed, chunks = true, {}
          if process ~= nil then
            process:kill(9)
          end
        elseif not failed then
          chunks[#chunks + 1] = data
        end
      end
    end,
  }, function(exit)
    local payload = not failed and exit.code == 0 and exit.signal == 0 and table.concat(chunks) or nil
    nvim.schedule(function()
      callback(payload)
    end)
  end)
  if not ok then
    nvim.schedule(function()
      callback(nil)
    end)
    return function() end
  end
  process = result
  return function()
    process:kill(9)
  end
end

---Read current broker provenance asynchronously; failure stays unknown.
---The optional fetch callback is an in-memory test seam with the production shape.
---@param binding unknown Private binding from a Forensics record or Session owner.
---@param callback fun(value: louiselm.OutputProvenance) Scheduled once on the editor loop.
---@param fetch? fun(session_id: string, callback: fun(payload: string?)): fun() Test double.
---@return fun() cancel Completes with unknown and stops pending broker work.
function M.read(binding, callback, fetch)
  local done = false
  local cancel_fetch
  local function finish(value)
    if done then
      return
    end
    done = true
    nvim.schedule(function()
      callback(value)
    end)
  end
  if not M.valid_binding(binding) or binding.kind == "not_managed" then
    finish(M.initial(binding))
  else
    local worker = fetch or fetch_broker
    local ok, cancel = pcall(worker, binding.session_id, function(payload)
      local decoded_ok, value = pcall(nvim.json.decode, payload or "")
      finish(decoded_ok and M.from_broker(value, binding.session_id) or M.unknown())
    end)
    if ok and type(cancel) == "function" then
      cancel_fetch = cancel
    else
      finish(M.unknown())
    end
  end
  return function()
    if done then
      return
    end
    if cancel_fetch ~= nil then
      cancel_fetch()
    end
    finish(M.unknown())
  end
end

---Prefix export metadata without changing the rendered transcript body.
---@param markdown string Transcript rendered by `louiselm.session.transcript`.
---@param value louiselm.OutputProvenance Current export-time projection.
---@return string marked
function M.markdown(markdown, value)
  local manifest = {
    schema = MARKDOWN_SCHEMA,
    body_digest = "sha256:" .. nvim.fn.sha256(markdown),
    output_provenance = M.normalize(value),
  }
  return "<!-- louiselm-provenance: " .. nvim.json.encode(manifest) .. " -->\n\n" .. markdown
end

---Inspect a Markdown export; detached, changed or malformed metadata is unknown.
---@param marked string Full exported Markdown bytes.
---@return louiselm.OutputProvenance
function M.inspect_markdown(marked)
  if type(marked) ~= "string" then
    return M.unknown()
  end
  local encoded, body = marked:match("^<!%-%- louiselm%-provenance: ([^\n]+) %-%->\n\n(.*)$")
  if encoded == nil or #encoded > 4096 then
    return M.unknown()
  end
  local ok, manifest = pcall(nvim.json.decode, encoded)
  if
    not ok
    or not exact(manifest, { schema = true, body_digest = true, output_provenance = true })
    or manifest.schema ~= MARKDOWN_SCHEMA
    or manifest.body_digest ~= "sha256:" .. nvim.fn.sha256(body)
  then
    return M.unknown()
  end
  local observed = M.normalize(manifest.output_provenance)
  -- A portable file proves what was observed at export, not that a broker still
  -- reports clean now. Positive taint and explicit non-management are durable.
  if observed.code == "untainted" then
    return M.unknown()
  end
  return observed
end

return M
