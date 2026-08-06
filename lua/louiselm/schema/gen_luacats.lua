local M = {}

---@param value string
---@return string
local function pascal_case(value)
  local result = {}
  local capitalize = true
  for index = 1, #value do
    local character = value:sub(index, index)
    if character:match("[%w]") ~= nil then
      result[#result + 1] = capitalize and character:upper() or character
      capitalize = false
    else
      capitalize = true
    end
  end
  if #result == 0 then
    return "Field"
  end
  return table.concat(result)
end

---@param parent_name string
---@param field_name string
---@return string
local function child_class_name(parent_name, field_name)
  return parent_name .. pascal_case(field_name)
end

---@param names string[]
---@param value string
---@return boolean
local function contains(names, value)
  for _, name in ipairs(names) do
    if name == value then
      return true
    end
  end
  return false
end

---@param fields table<string, louiselm.schema.Field>
---@return string[]
local function sorted_field_names(fields)
  local names = {}
  for field_name in pairs(fields) do
    names[#names + 1] = field_name
  end
  table.sort(names)
  return names
end

---@class louiselm.schema.LuacatsContext
---@field classes { name: string, node: louiselm.schema.Field }[]
---@field registered table<louiselm.schema.Field, boolean>

---@type fun(context: louiselm.schema.LuacatsContext, node: louiselm.schema.Field, name: string): string
local type_for

---@param context louiselm.schema.LuacatsContext
---@param node louiselm.schema.Field
---@param name string
local function register_class(context, node, name)
  if context.registered[node] then
    return
  end
  context.registered[node] = true
  context.classes[#context.classes + 1] = { name = name, node = node }

  if node.fields == nil then
    error("normalized table schema is missing fields")
  end
  for _, field_name in ipairs(sorted_field_names(node.fields)) do
    local field = node.fields[field_name]
    if field == nil then
      error("normalized table schema contains a missing field")
    end
    type_for(context, field, child_class_name(name, field_name))
  end
end

---@param context louiselm.schema.LuacatsContext
---@param node louiselm.schema.Field
---@param name string
---@return string
type_for = function(context, node, name)
  if node.type == "table" then
    register_class(context, node, name)
    return name
  end
  if node.type == "array-of" then
    if node.items == nil then
      error("normalized array schema is missing items")
    end
    local item_type = type_for(context, node.items, name .. "Item")
    if item_type:find("|", 1, true) ~= nil then
      item_type = "(" .. item_type .. ")"
    end
    return item_type .. "[]"
  end
  if node.type == "one-of" then
    if node.options == nil then
      error("normalized union schema is missing options")
    end
    local option_types = {}
    for index, option in ipairs(node.options) do
      local option_type = type_for(context, option, name .. "Option" .. index)
      if not contains(option_types, option_type) then
        option_types[#option_types + 1] = option_type
      end
    end
    return table.concat(option_types, "|")
  end
  return node.type
end

---@param description string
---@return string
local function format_description(description)
  local formatted = description:gsub("[\r\n]+", " ")
  return formatted
end

---@param context louiselm.schema.LuacatsContext
---@param class { name: string, node: louiselm.schema.Field }
---@param lines string[]
local function emit_class(context, class, lines)
  if class.node.fields == nil then
    error("normalized table schema is missing fields")
  end
  lines[#lines + 1] = "---@class " .. class.name
  for _, field_name in ipairs(sorted_field_names(class.node.fields)) do
    local field = class.node.fields[field_name]
    if field == nil then
      error("normalized table schema contains a missing field")
    end
    local optional = field.default ~= nil and "?" or ""
    local field_type = type_for(context, field, child_class_name(class.name, field_name))
    local line = string.format("---@field %s%s %s", field_name, optional, field_type)
    if field.description ~= nil then
      line = line .. " " .. format_description(field.description)
    end
    lines[#lines + 1] = line
  end
  lines[#lines + 1] = ""
end

---Generate LuaCATS annotations from a normalized schema.
---@param schema louiselm.schema.Schema Normalized schema returned by `Schema.define`.
---@return string annotations A deterministic LuaCATS source document.
function M.generate(schema)
  local context = {
    classes = {},
    registered = {},
  }
  type_for(context, schema, "louiselm.Config")

  local lines = {}
  for _, class in ipairs(context.classes) do
    emit_class(context, class, lines)
  end
  return table.concat(lines, "\n")
end

return M
