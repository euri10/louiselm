---@class louiselm.schema.ValidationError
---@field type "unknown_key"|"wrong_type"|"missing_required"|"validation_failed"
---@field path string
---@field key? string
---@field suggestion? string
---@field expected? string
---@field got? string
---@field example? louiselm.schema.Value
---@field message? string

local M = {}

local SUGGESTION_DISTANCE = 3

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

---@param key unknown
---@return string
local function key_name(key)
  if type(key) == "string" then
    return key
  end
  return tostring(key)
end

---@param key unknown
---@return string
local function key_sort_value(key)
  return type(key) .. ":" .. key_name(key)
end

---@param value table
---@return unknown[]
local function sorted_keys(value)
  local keys = {}
  for key in pairs(value) do
    keys[#keys + 1] = key
  end
  table.sort(keys, function(left, right)
    return key_sort_value(left) < key_sort_value(right)
  end)
  return keys
end

---@param value table
---@return boolean
local function is_dense_array(value)
  local length = 0
  while value[length + 1] ~= nil do
    length = length + 1
  end

  for key in pairs(value) do
    if type(key) ~= "number" or key < 1 or key % 1 ~= 0 or key > length then
      return false
    end
  end

  return true
end

---@param left string
---@param right string
---@return integer
local function string_distance(left, right)
  local previous = {}
  for column = 0, #right do
    previous[column] = column
  end

  for row = 1, #left do
    local current = { [0] = row }
    for column = 1, #right do
      local substitution_cost = left:sub(row, row) == right:sub(column, column) and 0 or 1
      current[column] =
        math.min(current[column - 1] + 1, previous[column] + 1, previous[column - 1] + substitution_cost)
    end
    previous = current
  end

  return previous[#right]
end

---@param key string
---@param fields table<string, louiselm.schema.Field>
---@return string?
local function suggestion_for(key, fields)
  local suggestion
  local best_distance = SUGGESTION_DISTANCE + 1
  local field_names = {}
  for field_name in pairs(fields) do
    field_names[#field_names + 1] = field_name
  end
  table.sort(field_names)

  for _, field_name in ipairs(field_names) do
    local distance = string_distance(key, field_name)
    if distance <= SUGGESTION_DISTANCE and distance < best_distance then
      suggestion = field_name
      best_distance = distance
    end
  end

  return suggestion
end

---@param node louiselm.schema.Field
---@return string
local function expected_type(node)
  if node.type == "array-of" then
    return "array"
  end
  if node.type == "map-of" then
    return "string-keyed table"
  end
  if node.type ~= "one-of" then
    return node.type
  end

  local types = {}
  for _, option in ipairs(node.options) do
    local option_type = expected_type(option)
    local already_present = false
    for _, existing_type in ipairs(types) do
      if existing_type == option_type then
        already_present = true
        break
      end
    end
    if not already_present then
      types[#types + 1] = option_type
    end
  end
  return table.concat(types, " or ")
end

---@param node louiselm.schema.Field
---@return louiselm.schema.Value
local function example_for(node)
  if node.default ~= nil then
    return node.default
  end
  if node.type == "string" then
    return ""
  end
  if node.type == "number" then
    return 0
  end
  if node.type == "boolean" then
    return true
  end
  if node.type == "table" or node.type == "array-of" or node.type == "map-of" then
    return {}
  end
  return example_for(node.options[1])
end

---@param node louiselm.schema.Field
---@param value unknown
---@return boolean
local function matches_type(node, value)
  if node.type == "one-of" then
    for _, option in ipairs(node.options) do
      if matches_type(option, value) then
        return true
      end
    end
    return false
  end
  if node.type == "array-of" then
    return type(value) == "table" and is_dense_array(value)
  end
  if node.type == "map-of" then
    return type(value) == "table"
  end
  return type(value) == node.type
end

---@param errors louiselm.schema.ValidationError[]
---@param node louiselm.schema.Field
---@param value unknown
---@param path string
local function validate_custom(errors, node, value, path)
  if node.validator == nil then
    return
  end

  local call_ok, valid, message = pcall(node.validator, value)
  if not call_ok then
    errors[#errors + 1] = {
      type = "validation_failed",
      path = path,
      message = "custom validator raised an error",
    }
    return
  end
  if valid == false then
    errors[#errors + 1] = {
      type = "validation_failed",
      path = path,
      message = type(message) == "string" and message or "custom validator rejected the value",
    }
    return
  end
  if valid ~= true then
    errors[#errors + 1] = {
      type = "validation_failed",
      path = path,
      message = "custom validator must return a boolean",
    }
  end
end

---@type fun(errors: louiselm.schema.ValidationError[], node: louiselm.schema.Field, value: unknown, path: string)
local validate_node

---@param errors louiselm.schema.ValidationError[]
---@param node louiselm.schema.Field
---@param value table
---@param path string
local function validate_table(errors, node, value, path)
  local fields = node.fields
  if fields == nil then
    error("normalized table schema is missing fields")
  end
  local keys = sorted_keys(value)
  for _, key in ipairs(keys) do
    local field_name = key_name(key)
    if type(key) ~= "string" or fields[field_name] == nil then
      errors[#errors + 1] = {
        type = "unknown_key",
        path = child_path(path, field_name),
        key = field_name,
        suggestion = type(key) == "string" and suggestion_for(key, fields) or nil,
      }
    end
  end

  local field_names = {}
  for field_name in pairs(fields) do
    field_names[#field_names + 1] = field_name
  end
  table.sort(field_names)
  for _, field_name in ipairs(field_names) do
    local field = fields[field_name]
    if field == nil then
      error("normalized table schema contains a missing field")
    end
    local field_value = value[field_name]
    local field_path = child_path(path, field_name)
    if field_value == nil then
      if field.default == nil then
        errors[#errors + 1] = {
          type = "missing_required",
          path = field_path,
          key = field_name,
        }
      end
    else
      validate_node(errors, field, field_value, field_path)
    end
  end
end

---@param errors louiselm.schema.ValidationError[]
---@param node louiselm.schema.Field
---@param value table
---@param path string
local function validate_array(errors, node, value, path)
  if not is_dense_array(value) then
    errors[#errors + 1] = {
      type = "wrong_type",
      path = path,
      expected = "array",
      got = "table",
      example = {},
    }
    return
  end

  for index = 1, #value do
    validate_node(errors, node.items, value[index], item_path(path, index))
  end
end

---@param errors louiselm.schema.ValidationError[]
---@param node louiselm.schema.Field
---@param value table
---@param path string
local function validate_map(errors, node, value, path)
  for _, key in ipairs(sorted_keys(value)) do
    local field_path = child_path(path, key_name(key))
    if type(key) ~= "string" or key == "" then
      errors[#errors + 1] = {
        type = "validation_failed",
        path = field_path,
        message = "map keys must be non-empty strings",
      }
    else
      validate_node(errors, node.items, value[key], field_path)
    end
  end
end

---@param errors louiselm.schema.ValidationError[]
---@param node louiselm.schema.Field
---@param value unknown
---@param path string
validate_node = function(errors, node, value, path)
  if node.type == "one-of" then
    local selected_option
    for _, option in ipairs(node.options) do
      if matches_type(option, value) then
        selected_option = option
        break
      end
    end
    if selected_option == nil then
      errors[#errors + 1] = {
        type = "wrong_type",
        path = path,
        expected = expected_type(node),
        got = type(value),
        example = example_for(node),
      }
      return
    end
    validate_node(errors, selected_option, value, path)
    validate_custom(errors, node, value, path)
    return
  end

  if not matches_type(node, value) then
    errors[#errors + 1] = {
      type = "wrong_type",
      path = path,
      expected = expected_type(node),
      got = type(value),
      example = example_for(node),
    }
    return
  end

  if node.type == "table" then
    validate_table(errors, node, value, path)
  elseif node.type == "array-of" then
    validate_array(errors, node, value, path)
  elseif node.type == "map-of" then
    validate_map(errors, node, value, path)
  end
  validate_custom(errors, node, value, path)
end

---Validate a configuration value against a normalized schema.
---@param schema louiselm.schema.Schema Normalized schema returned by `Schema.define`.
---@param value unknown User configuration to validate.
---@return louiselm.schema.ValidationError[] errors Every validation error, in stable traversal order.
function M.validate(schema, value)
  local errors = {}
  validate_node(errors, schema, value, "")
  return errors
end

return M
