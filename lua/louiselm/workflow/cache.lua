---Memoization for workflow Validation.
---
---Validation is a pure function of the workflow name, the stage blocks that claim it, and the set
---of discovered skill names. Caching it is therefore safe in the strong sense: a hit and a miss
---cannot disagree.
---
---The manifest key set is part of the digest on purpose. Transition targets resolve against
---discovered skills, so adding an unrelated skill genuinely changes what Validation would decide
---about a target that previously did not resolve. Invalidating then is correct behaviour, not
---over-eager invalidation, and it belongs in the contract rather than being discovered later as a
---stale-cache bug.

local M = {}
local bit = require("bit")

local FNV_OFFSET = 2166136261
local FNV_PRIME = 16777619
local MASK = 4294967295

---@param seed integer
---@param text string
---@return integer
local function fold(seed, text)
  local hash = seed
  for index = 1, #text do
    hash = bit.bxor(hash, text:byte(index))
    hash = bit.band(hash * FNV_PRIME, MASK)
  end
  return hash
end

local serialize

---Serialize deterministically. Lua table iteration order is unspecified, so keys are sorted; an
---unsorted digest would report spurious changes and quietly disable the cache.
---@param value unknown
---@param out string[]
serialize = function(value, out)
  local kind = type(value)
  if kind ~= "table" then
    out[#out + 1] = kind .. ":" .. tostring(value)
    return
  end

  local keys = {}
  for key in pairs(value) do
    keys[#keys + 1] = key
  end
  table.sort(keys, function(left, right)
    return type(left) .. ":" .. tostring(left) < type(right) .. ":" .. tostring(right)
  end)

  out[#out + 1] = "{"
  for _, key in ipairs(keys) do
    out[#out + 1] = tostring(key) .. "="
    serialize(value[key], out)
    out[#out + 1] = ","
  end
  out[#out + 1] = "}"
end

---@param workflow string
---@param document unknown
---@param manifest table<string, table>
---@return string
function M.digest(workflow, document, manifest)
  local names = {}
  for name in pairs(manifest) do
    names[#names + 1] = tostring(name)
  end
  table.sort(names)

  -- Every discovered name, so an addition invalidates. Only the participating blocks' contents,
  -- so editing an unrelated skill's body does not.
  local document_parts = {}
  serialize(document, document_parts)
  local hash = fold(FNV_OFFSET, workflow .. "\0" .. table.concat(document_parts) .. "\0" .. table.concat(names, "\0"))
  for _, name in ipairs(names) do
    local block = manifest[name]
    if type(block) == "table" and block.workflow == workflow then
      local out = {}
      serialize(block, out)
      hash = fold(hash, name .. "\0" .. table.concat(out))
    end
  end
  return string.format("%08x", hash)
end

---@param value table
---@return table
local function copy(value)
  local cloned = {}
  for key, item in pairs(value) do
    cloned[key] = type(item) == "table" and copy(item) or item
  end
  return cloned
end

M.copy = copy

---@class louiselm.workflow.Cache
---@field entries table<string, louiselm.workflow.Result>
---@field hits integer
---@field misses integer
local Cache = {}
Cache.__index = Cache

---@return louiselm.workflow.Cache
function M.new()
  return setmetatable({ entries = {}, hits = 0, misses = 0 }, Cache)
end

---Return the memoized result, computing it only on a miss. Callers receive a copy, so a caller
---that annotates a result cannot poison later hits.
---@param workflow string
---@param document unknown
---@param manifest table<string, table>
---@param compute fun(workflow: string, manifest: table<string, table>): louiselm.workflow.Result
---@return louiselm.workflow.Result
function Cache:resolve(workflow, document, manifest, compute)
  local key = M.digest(workflow, document, manifest)
  local entry = self.entries[key]
  if entry == nil then
    self.misses = self.misses + 1
    entry = compute(workflow, manifest)
    self.entries[key] = entry
  else
    self.hits = self.hits + 1
  end
  return copy(entry)
end

return M
