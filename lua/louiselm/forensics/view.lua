local Record = require("louiselm.forensics.record")

local M = {}

---Render an identifier as prose without inventing a second vocabulary for it.
---@param identifier string Canonical snake_case record key.
---@return string words
local function words(identifier)
  return (identifier:gsub("_", " "))
end

---@param map table<string, unknown>
---@return string[] keys
local function sorted_keys(map)
  local keys = {}
  for key in pairs(map) do
    keys[#keys + 1] = key
  end
  table.sort(keys)
  return keys
end

---@param value unknown
---@return string rendered
local function scalar(value)
  if type(value) == "string" then
    return value
  end
  return tostring(value)
end

---Render one bounded observation value. Observations never hold raw source
---contents (louiselm-ce6.24 AC6), so every shape here is already safe to show.
---@param value unknown
---@return string rendered
local function observation(value)
  if type(value) ~= "table" then
    return scalar(value)
  end
  if #value > 0 then
    local items = {}
    for index, item in ipairs(value) do
      items[index] = scalar(item)
    end
    return table.concat(items, ", ")
  end
  local keys = sorted_keys(value)
  if #keys == 0 then
    return "none"
  end
  local pairs_out = {}
  for index, key in ipairs(keys) do
    pairs_out[index] = key .. "=" .. scalar(value[key])
  end
  return table.concat(pairs_out, ", ")
end

---@param source louiselm.forensics.EvidenceSource
---@return string mutability
local function mutability(source)
  if source.mutable == true then
    return "changes after observation"
  end
  return "mutability not recorded"
end

---@param lines string[]
---@param source louiselm.forensics.EvidenceSource
---@param index integer
local function describe_source(lines, source, index)
  lines[#lines + 1] = string.format("%d. %s — %s, %s", index, source.kind, source.state, mutability(source))
  local properties = Record.source_properties(source.kind)
  if #properties > 0 then
    local named = {}
    for position, property in ipairs(properties) do
      named[position] = words(property)
    end
    lines[#lines + 1] = "   keeps " .. table.concat(named, ", ")
  end
  if source.path ~= nil then
    lines[#lines + 1] = "   read it at: " .. source.path
  end
  if source.reason ~= nil then
    lines[#lines + 1] = "   why: " .. source.reason
  end
end

---Render a human-facing projection of one Forensics inspection.
---
---The canonical record stays the JSON file on disk; this is a read-only view of
---the same values and is never persisted beside it. It names pointers to
---evidence, never the evidence itself, so nothing sensitive is shown that the
---record did not already hold.
---@param inspection louiselm.forensics.Inspection Inspection from `store:inspect()`.
---@return string[] lines Buffer lines, in display order.
function M.lines(inspection)
  if type(inspection) ~= "table" or type(inspection.subject) ~= "table" then
    error("forensics view requires an inspection table", 2)
  end
  local subject = inspection.subject
  local lines = {
    "# Session Forensics " .. tostring(inspection.id),
    "",
    "Subject Session:    " .. subject.agent .. "/" .. subject.acp_session_id,
    "Diagnosing Session: " .. (inspection.diagnosing_session or "none recorded"),
    "Observed at:        " .. os.date("!%Y-%m-%dT%H:%M:%SZ", inspection.observed_at),
    "Schema version:     " .. tostring(inspection.schema_version),
    "",
    "## Evidence sources",
    "",
  }
  local sources = inspection.evidence_sources or {}
  if #sources == 0 then
    lines[#lines + 1] = "no evidence sources were recorded"
  end
  for index, source in ipairs(sources) do
    describe_source(lines, source, index)
  end

  -- Recorded state and current state are different claims: a source present at
  -- observation may be gone now, and saying so is the point of the inspection.
  lines[#lines + 1] = ""
  lines[#lines + 1] = "## Readable now"
  lines[#lines + 1] = ""
  local availability = inspection.evidence_availability or {}
  local properties = sorted_keys(availability)
  if #properties == 0 then
    lines[#lines + 1] = "no evidence property could be checked"
  end
  for _, property in ipairs(properties) do
    lines[#lines + 1] = property .. ": " .. availability[property]
  end

  lines[#lines + 1] = ""
  lines[#lines + 1] = "## Observations"
  lines[#lines + 1] = ""
  local observations = inspection.observations or {}
  local keys = sorted_keys(observations)
  if #keys == 0 then
    lines[#lines + 1] = "no observations were recorded"
  end
  for _, key in ipairs(keys) do
    lines[#lines + 1] = key .. ": " .. observation(observations[key])
  end
  return lines
end

return M
