local M = {}

local MAX_WIDTH = 78

---@param value unknown
---@return string
local function format_value(value)
  if type(value) == "string" then
    return string.format("%q", value)
  end
  if type(value) == "table" then
    return "{}"
  end
  return tostring(value)
end

---@param node louiselm.schema.Field
---@return string
local function type_description(node)
  if node.type == "array-of" then
    if node.items == nil then
      error("normalized array schema is missing items")
    end
    return "array of " .. type_description(node.items)
  end
  if node.type == "map-of" then
    if node.items == nil then
      error("normalized map schema is missing items")
    end
    return "string-keyed map of " .. type_description(node.items)
  end
  if node.type == "one-of" then
    if node.options == nil then
      error("normalized union schema is missing options")
    end
    local options = {}
    for _, option in ipairs(node.options) do
      options[#options + 1] = type_description(option)
    end
    return "one of: " .. table.concat(options, ", ")
  end
  return node.type
end

---@param path string
---@return string
local function tag_for(path)
  local tag = path:gsub("[^%w]+", "-"):lower()
  return "louiselm-config-" .. tag
end

---@param title string
---@param tag string
---@return string
local function heading(title, tag)
  local padding = math.max(1, 62 - #title)
  return title .. string.rep(" ", padding) .. "*" .. tag .. "*"
end

---@param lines string[]
---@param text string
---@param first_prefix string
---@param continuation_prefix string
local function append_wrapped(lines, text, first_prefix, continuation_prefix)
  local words = {}
  for word in text:gmatch("%S+") do
    words[#words + 1] = word
  end
  if #words == 0 then
    lines[#lines + 1] = first_prefix
    return
  end

  local current = first_prefix
  for _, word in ipairs(words) do
    local separator = current == first_prefix and "" or " "
    if #current + #separator + #word <= MAX_WIDTH then
      current = current .. separator .. word
    else
      lines[#lines + 1] = current
      current = continuation_prefix .. word
    end
  end
  lines[#lines + 1] = current
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

---@type fun(lines: string[], field_name: string, node: louiselm.schema.Field, path: string)
local emit_field

---@param lines string[]
---@param node louiselm.schema.Field
---@param path string
local function emit_nested_fields(lines, node, path)
  if node.type == "table" then
    if node.fields == nil then
      error("normalized table schema is missing fields")
    end
    for _, child_name in ipairs(sorted_field_names(node.fields)) do
      local child = node.fields[child_name]
      if child == nil then
        error("normalized table schema contains a missing field")
      end
      emit_field(lines, child_name, child, path .. "." .. child_name)
    end
  elseif node.type == "array-of" then
    if node.items == nil then
      error("normalized array schema is missing items")
    end
    emit_nested_fields(lines, node.items, path .. "[]")
  elseif node.type == "map-of" then
    if node.items == nil then
      error("normalized map schema is missing items")
    end
    emit_nested_fields(lines, node.items, path .. ".<name>")
  elseif node.type == "one-of" then
    if node.options == nil then
      error("normalized union schema is missing options")
    end
    for _, option in ipairs(node.options) do
      emit_nested_fields(lines, option, path)
    end
  end
end

---@param lines string[]
---@param field_name string
---@param node louiselm.schema.Field
---@param path string
emit_field = function(lines, field_name, node, path)
  lines[#lines + 1] = ""
  lines[#lines + 1] = "------------------------------------------------------------------------------"
  lines[#lines + 1] = heading(path, tag_for(path))
  lines[#lines + 1] = "    Type: " .. type_description(node)
  if node.default ~= nil then
    lines[#lines + 1] = "    Default: " .. format_value(node.default)
  end
  if node.description ~= nil then
    local description = node.description:gsub("[\r\n]+", " ")
    append_wrapped(lines, description, "    Description: ", "    ")
  end
  emit_nested_fields(lines, node, path)
end

---@param lines string[]
---@param schema louiselm.schema.Schema
---@param tag string
local function emit_configuration(lines, schema, tag)
  if schema.fields == nil then
    error("normalized table schema is missing fields")
  end
  lines[#lines + 1] = "=============================================================================="
  lines[#lines + 1] = heading("Configuration", tag)
  for _, field_name in ipairs(sorted_field_names(schema.fields)) do
    local field = schema.fields[field_name]
    if field == nil then
      error("normalized table schema contains a missing field")
    end
    emit_field(lines, field_name, field, field_name)
  end
end

---Generate the configuration section of a Neovim help document.
---@param schema louiselm.schema.Schema Normalized schema returned by `Schema.define`.
---@return string vimdoc A deterministic configuration reference section.
function M.generate_configuration(schema)
  local lines = {}
  emit_configuration(lines, schema, "louiselm-configuration")
  return table.concat(lines, "\n") .. "\n"
end

---Generate a Neovim help document from a normalized schema.
---@param schema louiselm.schema.Schema Normalized schema returned by `Schema.define`.
---@return string vimdoc A deterministic `louiselm.txt` help document.
function M.generate(schema)
  local lines = {
    "*louiselm.txt*  louiselm.nvim configuration reference",
    "",
    "==============================================================================",
    heading("Contents", "louiselm-contents"),
    "",
    "  |louiselm|          louiselm.nvim configuration reference",
    "  Configuration options are documented below.",
    "",
  }

  emit_configuration(lines, schema, "louiselm")
  lines[#lines + 1] = ""
  return table.concat(lines, "\n") .. "\n"
end

return M
