---Presentation-only artifact preflight. Never authorizes or starts an Agent.
local Posture = require("louiselm.posture")
---@diagnostic disable-next-line: undefined-global -- Neovim runtime.
local nvim = vim
local M = {}

---@class louiselm.PreflightIdentity
---@field field string Closed identity label.
---@field value string|userdata Proposed identity or JSON null when unresolved.

---@class louiselm.PreflightChange
---@field field string Closed identity label.
---@field before string Selected prior identity.
---@field after string Proposed identity.

---@class louiselm.PreflightComparison
---@field state "not_requested"|"different_agent"|"inputs_unavailable"|"compared"
---@field previous_request_digest string|userdata Prior request identity or JSON null.
---@field changes louiselm.PreflightChange[]
---@field unresolved string[] Fields not known on both sides.

---@class louiselm.Preflight
---@field schema "louiselm.launch.preflight/1"
---@field notice string Fixed prospective-only disclosure.
---@field request_digest string Exact request future launch integration must bind.
---@field manifest_state "missing"|"contradictory"|"matched"
---@field proposed louiselm.PreflightIdentity[]
---@field comparison louiselm.PreflightComparison
---@field posture louiselm.Posture

---@class louiselm.PreflightOptions
---@field request string Canonical launch request file.
---@field manifest? string Canonical Session input manifest file.
---@field previous_request? string Explicit prior request file; requires previous_manifest.
---@field previous_manifest? string Explicit prior manifest file; requires previous_request.
---@field store? string Existing supply store directory.
---@field registry? string Root-trusted registry directory.
---@field command? string Trusted louiselm-skills executable; never a shell command.
---@field cwd? string Working directory; defaults to Neovim's current directory.

---@class louiselm.PreflightRead
---@field dispose fun() Cancel owned work and suppress even already-scheduled completion.

local LIMIT = 65536
local NOTICE =
  "Prospective artifact snapshot only; not authorization or live Session state. Launch must recheck and bind this exact request digest. Proposed identities do not prove enforcement."
local FIELDS = {
  "agent",
  "generation",
  "instruction_view",
  "runtime",
  "input_manifest",
  "project_instructions",
  "tool_schemas",
  "plugin_schemas",
  "policy",
  "isolation_contract",
  "isolation_receipt",
  "envelope",
  "envelope_revision",
  "network_scope",
  "provider_disclosure",
}
local REQUIRED = { agent = true, generation = true, input_manifest = true, envelope = true, envelope_revision = true }
local UNPROVEN = {
  native_supply = "native_supply_uncertain",
  isolation = "evidence_missing",
  network = "evidence_missing",
  provider_disclosure = "provider_disclosure_missing",
}

local function exact(value, names)
  if type(value) ~= "table" then
    return false
  end
  local allowed = {}
  for _, name in ipairs(names) do
    if value[name] == nil then
      return false
    end
    allowed[name] = true
  end
  for name in pairs(value) do
    if not allowed[name] then
      return false
    end
  end
  return true
end

local function array(value, maximum)
  if type(value) ~= "table" or #value > maximum then
    return false
  end
  local count = 0
  for _ in ipairs(value) do
    count = count + 1
  end
  for key in pairs(value) do
    if type(key) ~= "number" or key < 1 or key > count or key % 1 ~= 0 then
      return false
    end
  end
  return true
end

local function digest(value)
  return type(value) == "string" and #value == 71 and value:match("^sha256:[a-f0-9]+$") ~= nil
end

local function identifier(value)
  return type(value) == "string" and #value > 0 and #value <= 128 and value:match("^[%w_.:-]+$") ~= nil
end

local function identity(field, value)
  if value == nvim.NIL then
    return not REQUIRED[field]
  end
  if field == "agent" or field == "envelope" or field == "isolation_receipt" then
    return identifier(value)
  end
  if field == "network_scope" then
    return false
  end
  if field == "isolation_contract" then
    return value == "louiselm.isolation/2"
  end
  if field == "envelope_revision" then
    return type(value) == "string"
      and (value == "0" or value:match("^[1-9][0-9]*$") ~= nil)
      and (#value < 20 or (#value == 20 and value <= "18446744073709551615"))
  end
  return digest(value)
end

local function comparison_valid(comparison, values, matched)
  if
    not exact(comparison, { "state", "previous_request_digest", "changes", "unresolved" })
    or not array(comparison.changes, #FIELDS)
    or not array(comparison.unresolved, #FIELDS)
  then
    return false
  end
  if comparison.state == "not_requested" then
    return comparison.previous_request_digest == nvim.NIL and #comparison.changes == 0 and #comparison.unresolved == 0
  end
  if not digest(comparison.previous_request_digest) then
    return false
  end
  if comparison.state == "different_agent" or comparison.state == "inputs_unavailable" then
    return #comparison.changes == 0 and #comparison.unresolved == 0
  end
  if comparison.state ~= "compared" or not matched then
    return false
  end
  local seen = {}
  for _, change in ipairs(comparison.changes) do
    if
      not exact(change, { "field", "before", "after" })
      or values[change.field] == nil
      or seen[change.field]
      or change.before == nvim.NIL
      or change.after == nvim.NIL
      or not identity(change.field, change.before)
      or change.after ~= values[change.field]
      or change.before == change.after
    then
      return false
    end
    seen[change.field] = true
  end
  for _, field in ipairs(comparison.unresolved) do
    if type(field) ~= "string" or values[field] == nil or seen[field] then
      return false
    end
    seen[field] = true
  end
  for field, value in pairs(values) do
    if value == nvim.NIL and not seen[field] then
      return false
    end
  end
  return true
end

local function validate(value)
  if
    not exact(value, { "schema", "notice", "request_digest", "manifest_state", "proposed", "comparison", "posture" })
    or value.schema ~= "louiselm.launch.preflight/1"
    or value.notice ~= NOTICE
    or not digest(value.request_digest)
    or not array(value.proposed, #FIELDS)
    or #value.proposed ~= #FIELDS
  then
    return false
  end
  if
    value.manifest_state ~= "missing"
    and value.manifest_state ~= "contradictory"
    and value.manifest_state ~= "matched"
  then
    return false
  end
  local values = {}
  for index, entry in ipairs(value.proposed) do
    if
      not exact(entry, { "field", "value" })
      or entry.field ~= FIELDS[index]
      or not identity(entry.field, entry.value)
    then
      return false
    end
    if value.manifest_state ~= "matched" and not REQUIRED[entry.field] and entry.value ~= nvim.NIL then
      return false
    end
    values[entry.field] = entry.value
  end
  local encoded_ok, encoded = pcall(nvim.json.encode, value.posture)
  if not encoded_ok then
    return false
  end
  local posture, err = Posture.decode(encoded)
  if posture == nil or err ~= nil or posture.state ~= "unverified" then
    return false
  end
  for name, code in pairs(UNPROVEN) do
    local dimension = posture.dimensions[name]
    if dimension.state ~= "failed" or dimension.failure_code ~= code or #dimension.evidence ~= 0 then
      return false
    end
  end
  for name, field in pairs({ managed_supply = "generation", runtime = "runtime" }) do
    local dimension = posture.dimensions[name]
    if dimension.state == "waived" then
      return false
    end
    if dimension.state == "verified" then
      if value.manifest_state ~= "matched" or #dimension.evidence ~= 1 or dimension.evidence[1].id ~= values[field] then
        return false
      end
    elseif #dimension.evidence ~= 0 then
      return false
    end
  end
  return comparison_valid(value.comparison, values, value.manifest_state == "matched")
end

---Decode bounded trusted-tool output for presentation, never for authorization.
---@param payload string JSON preflight record.
---@return louiselm.Preflight? preview
---@return string? error_message Fixed diagnostic without payload content.
function M.decode(payload)
  if type(payload) ~= "string" or #payload > LIMIT then
    return nil, "invalid preflight payload size"
  end
  local ok, value = pcall(nvim.json.decode, payload)
  if not ok or not validate(value) then
    return nil, "invalid or contradictory preflight record"
  end
  return value
end

---Render the normalized proposed identities, diff and posture as health entries.
---@param preview louiselm.Preflight
---@return louiselm.PostureHealthItem[]? items
---@return string? error_message Fixed diagnostic on invalid input.
function M.health_items(preview)
  if not validate(preview) then
    return nil, "invalid or contradictory preflight record"
  end
  local items = {
    { level = "warn", message = preview.notice },
    { level = "info", message = "Request digest: " .. preview.request_digest },
    { level = "info", message = "Manifest: " .. preview.manifest_state },
  }
  for _, entry in ipairs(preview.proposed) do
    items[#items + 1] = {
      level = "info",
      message = "proposed " .. entry.field .. ": " .. (entry.value == nvim.NIL and "unresolved" or entry.value),
    }
  end
  local comparison = preview.comparison
  items[#items + 1] = {
    level = "info",
    message = "Prior input comparison: " .. comparison.state .. " (selected artifacts only; not evidence of execution)",
  }
  if comparison.previous_request_digest ~= nvim.NIL then
    items[#items + 1] = { level = "info", message = "prior request digest: " .. comparison.previous_request_digest }
  end
  for _, change in ipairs(comparison.changes) do
    items[#items + 1] = { level = "info", message = change.field .. ": " .. change.before .. " -> " .. change.after }
  end
  for _, field in ipairs(comparison.unresolved) do
    items[#items + 1] = { level = "warn", message = field .. ": comparison unresolved" }
  end
  local posture_items, err = Posture.health_items(preview.posture)
  if posture_items == nil then
    return nil, err
  end
  for _, item in ipairs(posture_items) do
    items[#items + 1] = item
  end
  return items
end

local PATH_OPTIONS = { "request", "manifest", "previous_request", "previous_manifest", "store", "registry" }

local function arguments(options)
  if type(options) ~= "table" then
    return nil
  end
  local allowed = { command = true, cwd = true }
  for _, key in ipairs(PATH_OPTIONS) do
    allowed[key] = true
  end
  for key, value in pairs(options) do
    if not allowed[key] or type(value) ~= "string" or value == "" or value:find("%z") then
      return nil
    end
  end
  if options.request == nil or (options.previous_request == nil) ~= (options.previous_manifest == nil) then
    return nil
  end
  local result = { options.command or "louiselm-skills", "preflight", "--robot-json" }
  for _, key in ipairs(PATH_OPTIONS) do
    if options[key] ~= nil then
      result[#result + 1] = "--" .. key:gsub("_", "-")
      result[#result + 1] = options[key]
    end
  end
  return result
end

---Asynchronously run artifact preflight, with bounded stdout and discarded stderr.
---Completion is scheduled once on the main loop. Disposal suppresses completion.
---@param options louiselm.PreflightOptions Caller-owned table, only read.
---@param callback fun(preview: louiselm.Preflight?, error_message: string?)
---@return louiselm.PreflightRead? handle
---@return string? error_message Fixed validation/spawn error; callback is not called.
function M.read(options, callback)
  local command = arguments(options)
  if command == nil or type(callback) ~= "function" then
    return nil, "invalid preflight read options"
  end
  local disposed, completed = false, false
  local chunks, size, failure = {}, 0, nil
  local process
  local ok, result = pcall(nvim.system, command, {
    cwd = options.cwd or nvim.fn.getcwd(),
    env = {},
    text = true,
    timeout = 30000,
    stdout = function(err, data)
      if disposed or completed or failure then
        return
      end
      if err then
        failure = "cannot read preflight output"
        return
      end
      if data == nil then
        return
      end
      size = size + #data
      if size > LIMIT then
        chunks = {}
        failure = "preflight output exceeds size limit"
        if process then
          process:kill(9)
        end
      else
        chunks[#chunks + 1] = data
      end
    end,
    stderr = false,
  }, function(exit)
    if disposed or completed then
      return
    end
    completed = true
    local payload = table.concat(chunks)
    chunks = {}
    nvim.schedule(function()
      if disposed then
        return
      end
      if failure then
        callback(nil, failure)
        return
      end
      if exit.code ~= 2 or exit.signal ~= 0 then
        callback(nil, "preflight command failed or timed out")
        return
      end
      local preview, err = M.decode(payload)
      callback(preview, err)
    end)
  end)
  if not ok then
    disposed = true
    return nil, "cannot start preflight command"
  end
  process = result
  return {
    dispose = function()
      if disposed then
        return
      end
      disposed = true
      chunks = {}
      if not completed then
        process:kill(9)
      end
    end,
  }
end

return M
