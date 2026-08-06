---@class louiselm.schema.Deprecation
---@field key string Deprecated configuration key.
---@field message string Explanation shown to the user.
---@field migration string Replacement key or migration guidance.

---@class louiselm.schema.DeprecationWarning
---@field path string Present deprecated configuration path.
---@field message string Human-readable warning.

local M = {}

---@param key unknown
---@param metadata unknown
---@return louiselm.schema.Deprecation? deprecation
---@return string? error_message
function M.create(key, metadata)
  if type(key) ~= "string" or key == "" then
    return nil, "deprecated key must be a non-empty string"
  end
  if type(metadata) ~= "table" then
    return nil, string.format("deprecated key '%s' metadata must be a table", key)
  end
  if type(metadata.message) ~= "string" or metadata.message == "" then
    return nil, string.format("deprecated key '%s' requires a message", key)
  end
  if type(metadata.migration) ~= "string" or metadata.migration == "" then
    return nil, string.format("deprecated key '%s' requires a migration", key)
  end

  return {
    key = key,
    message = metadata.message,
    migration = metadata.migration,
  }
end

---@param value unknown
---@param path string
---@return louiselm.schema.Deprecation? deprecation
---@return string? error_message
function M.normalize(value, path)
  if type(value) ~= "table" then
    return nil, string.format("field '%s' deprecation must be created with Schema.deprecated", path)
  end
  return M.create(value.key, value)
end

---@param path string
---@param key string
---@return string
local function child_path(path, key)
  if path == "" then
    return key
  end
  return path .. "." .. key
end

---@param path string
---@param index integer
---@return string
local function item_path(path, index)
  return path .. "[" .. index .. "]"
end

---@param node louiselm.schema.Field
---@param value unknown
---@return boolean
local function matches_type(node, value)
  if node.type == "one-of" then
    if node.options == nil then
      return false
    end
    for _, option in ipairs(node.options) do
      if matches_type(option, value) then
        return true
      end
    end
    return false
  end
  return type(value) == (node.type == "array-of" and "table" or node.type)
end

---@type fun(warnings: louiselm.schema.DeprecationWarning[], node: louiselm.schema.Field, value: unknown, path: string)
local walk

---@param warnings louiselm.schema.DeprecationWarning[]
---@param node louiselm.schema.Field
---@param value table
---@param path string
local function walk_table(warnings, node, value, path)
  if node.fields == nil then
    return
  end
  local field_names = {}
  for field_name in pairs(node.fields) do
    field_names[#field_names + 1] = field_name
  end
  table.sort(field_names)
  for _, field_name in ipairs(field_names) do
    local field = node.fields[field_name]
    if field ~= nil then
      walk(warnings, field, value[field_name], child_path(path, field_name))
    end
  end
end

---@param warnings louiselm.schema.DeprecationWarning[]
---@param node louiselm.schema.Field
---@param value table
---@param path string
local function walk_array(warnings, node, value, path)
  if node.items == nil then
    return
  end
  for index = 1, #value do
    walk(warnings, node.items, value[index], item_path(path, index))
  end
end

---@param warnings louiselm.schema.DeprecationWarning[]
---@param node louiselm.schema.Field
---@param value unknown
---@param path string
walk = function(warnings, node, value, path)
  if value == nil then
    return
  end
  if node.deprecated ~= nil then
    warnings[#warnings + 1] = {
      path = path,
      message = string.format(
        "deprecated key '%s': %s; migrate to '%s'",
        path,
        node.deprecated.message,
        node.deprecated.migration
      ),
    }
  end

  if node.type == "table" and type(value) == "table" then
    walk_table(warnings, node, value, path)
  elseif node.type == "array-of" and type(value) == "table" then
    walk_array(warnings, node, value, path)
  elseif node.type == "one-of" and node.options ~= nil then
    for _, option in ipairs(node.options) do
      if matches_type(option, value) then
        walk(warnings, option, value, path)
        break
      end
    end
  end
end

---Find deprecation warnings for present keys in a configuration value.
---@param schema louiselm.schema.Schema Normalized schema returned by `Schema.define`.
---@param value unknown User configuration to inspect.
---@return louiselm.schema.DeprecationWarning[] warnings Warnings in stable schema order.
function M.find(schema, value)
  local warnings = {}
  walk(warnings, schema, value, "")
  return warnings
end

return M
