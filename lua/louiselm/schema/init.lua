---@alias louiselm.schema.Type "string"|"number"|"boolean"|"table"|"array-of"|"map-of"|"one-of"
---@alias louiselm.schema.Value string|number|boolean|table
---@alias louiselm.schema.Validator fun(value: louiselm.schema.Value): boolean, string?

---@class louiselm.schema.Field
---@field type louiselm.schema.Type
---@field default? louiselm.schema.Value
---@field description? string
---@field validator? louiselm.schema.Validator
---@field deprecated? louiselm.schema.Deprecation
---@field fields? table<string, louiselm.schema.Field>
---@field items? louiselm.schema.Field
---@field options? louiselm.schema.Field[]

---@class louiselm.schema.Schema: louiselm.schema.Field
---@field type "table"
---@field fields table<string, louiselm.schema.Field>

local M = {}
local Deprecate = require("louiselm.schema.deprecate")

local supported_types = {
  ["string"] = true,
  ["number"] = true,
  ["boolean"] = true,
  ["table"] = true,
  ["array-of"] = true,
  ["map-of"] = true,
  ["one-of"] = true,
}

local normalize_node

---@param value table
---@param path string
---@return integer length
---@return string? error_message
local function sequence_length(value, path)
  local length = 0
  while value[length + 1] ~= nil do
    length = length + 1
  end

  for key in pairs(value) do
    if type(key) ~= "number" or key < 1 or key % 1 ~= 0 or key > length then
      return 0, string.format("field '%s' must be a dense array", path)
    end
  end

  return length
end

---@param value unknown
---@param path string
---@return louiselm.schema.Field? node
---@return string? error_message
local function normalize_type(value, path)
  if type(value) == "string" then
    if
      not supported_types[value]
      or value == "array-of"
      or value == "map-of"
      or value == "one-of"
      or value == "table"
    then
      return nil, string.format("field '%s' has unsupported type '%s'", path, value)
    end
    return { type = value }
  end

  if type(value) ~= "table" then
    return nil, string.format("field '%s' must describe a type", path)
  end

  return normalize_node(value, path)
end

---@param spec table<string, table>
---@param parent_path string
---@return table<string, louiselm.schema.Field>? fields
---@return string? error_message
local function normalize_fields(spec, parent_path)
  local fields = {}
  for key, description in pairs(spec) do
    if type(key) ~= "string" then
      return nil, string.format("schema key at '%s' must be a string", parent_path)
    end

    local path = parent_path == "" and key or parent_path .. "." .. key
    local field, err = normalize_node(description, path)
    if field == nil then
      return nil, err
    end
    fields[key] = field
  end
  return fields
end

---@param description table
---@param path string
---@return louiselm.schema.Field? node
---@return string? error_message
normalize_node = function(description, path)
  if type(description) ~= "table" then
    return nil, string.format("field '%s' must be a table", path)
  end

  local type_name = description.type
  if type(type_name) ~= "string" then
    return nil, string.format("field '%s' is missing a string type", path)
  end
  if not supported_types[type_name] then
    return nil, string.format("field '%s': unsupported type '%s'", path, type_name)
  end

  local node = { type = type_name }
  if description.default ~= nil then
    node.default = description.default
  end
  if description.description ~= nil then
    if type(description.description) ~= "string" then
      return nil, string.format("field '%s' description must be a string", path)
    end
    node.description = description.description
  end
  if description.validator ~= nil then
    if type(description.validator) ~= "function" then
      return nil, string.format("field '%s' validator must be a function", path)
    end
    node.validator = description.validator
  end
  if description.deprecated ~= nil then
    local deprecation, deprecation_err = Deprecate.normalize(description.deprecated, path)
    if deprecation == nil then
      return nil, deprecation_err
    end
    node.deprecated = deprecation
  end

  if type_name == "table" then
    if type(description.fields) ~= "table" then
      return nil, string.format("field '%s' requires a fields table", path)
    end
    local fields, err = normalize_fields(description.fields, path)
    if fields == nil then
      return nil, err
    end
    node.fields = fields
  elseif type_name == "array-of" or type_name == "map-of" then
    if description.items == nil then
      return nil, string.format("field '%s' requires an items type", path)
    end
    local items, err = normalize_type(description.items, path .. ".items")
    if items == nil then
      return nil, err
    end
    node.items = items
  elseif type_name == "one-of" then
    local options = description.options or description.types
    if type(options) ~= "table" then
      return nil, string.format("field '%s' requires an options array", path)
    end
    local length, length_err = sequence_length(options, path .. ".options")
    if length_err ~= nil then
      return nil, length_err
    end
    if length == 0 then
      return nil, string.format("field '%s' requires at least one option", path)
    end

    node.options = {}
    for index = 1, length do
      local option, err = normalize_type(options[index], path .. ".options[" .. index .. "]")
      if option == nil then
        return nil, err
      end
      node.options[index] = option
    end
  end

  return node
end

---Describe a configuration schema and return its normalized representation.
---@param spec table<string, table> Top-level field descriptions.
---@return louiselm.schema.Schema? schema The normalized table schema.
---@return string? error_message An actionable DSL error, if `spec` is invalid.
function M.define(spec)
  if type(spec) ~= "table" then
    return nil, "schema spec must be a table"
  end

  local fields, err = normalize_fields(spec, "")
  if fields == nil then
    return nil, err
  end
  return { type = "table", fields = fields }
end

M.validate = require("louiselm.schema.validate").validate
M.report = require("louiselm.schema.report").format
M.deprecated = Deprecate.create
M.generate_luacats = require("louiselm.schema.gen_luacats").generate
M.generate_vimdoc = require("louiselm.schema.gen_vimdoc").generate

return M
