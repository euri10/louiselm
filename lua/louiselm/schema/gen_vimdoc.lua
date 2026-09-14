local Text = require("louiselm.docs.vimdoc_text")

local M = {}

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

---@alias louiselm.schema.VimdocLink fun(text: string): string

---@type fun(lines: string[], field_name: string, node: louiselm.schema.Field, path: string, link: louiselm.schema.VimdocLink)
local emit_field

---@param lines string[]
---@param node louiselm.schema.Field
---@param path string
---@param link louiselm.schema.VimdocLink
local function emit_nested_fields(lines, node, path, link)
  if node.type == "table" then
    if node.fields == nil then
      error("normalized table schema is missing fields")
    end
    for _, child_name in ipairs(sorted_field_names(node.fields)) do
      local child = node.fields[child_name]
      if child == nil then
        error("normalized table schema contains a missing field")
      end
      emit_field(lines, child_name, child, path .. "." .. child_name, link)
    end
  elseif node.type == "array-of" then
    if node.items == nil then
      error("normalized array schema is missing items")
    end
    emit_nested_fields(lines, node.items, path .. "[]", link)
  elseif node.type == "map-of" then
    if node.items == nil then
      error("normalized map schema is missing items")
    end
    emit_nested_fields(lines, node.items, path .. ".<name>", link)
  elseif node.type == "one-of" then
    if node.options == nil then
      error("normalized union schema is missing options")
    end
    for _, option in ipairs(node.options) do
      emit_nested_fields(lines, option, path, link)
    end
  end
end

---@param lines string[]
---@param field_name string
---@param node louiselm.schema.Field
---@param path string
---@param link louiselm.schema.VimdocLink
emit_field = function(lines, field_name, node, path, link)
  lines[#lines + 1] = ""
  lines[#lines + 1] = Text.SUBSECTION_RULE
  Text.append_heading(lines, path, tag_for(path))
  lines[#lines + 1] = "    Type: " .. type_description(node)
  if node.default ~= nil then
    lines[#lines + 1] = "    Default: " .. format_value(node.default)
  end
  if node.description ~= nil then
    local description = node.description:gsub("[\r\n]+", " ")
    Text.append_wrapped(lines, link(description), "    Description: ", "    ")
  end
  emit_nested_fields(lines, node, path, link)
end

---@param text string
---@return string
local function unlinked(text)
  return text
end

---Append every configuration field, without the section heading that owns them.
---@param lines string[] Accumulator appended in place.
---@param schema louiselm.schema.Schema Normalized schema returned by `Schema.define`.
---@param link? louiselm.schema.VimdocLink Rewrites descriptions into cross-references.
function M.append_configuration_fields(lines, schema, link)
  if schema.fields == nil then
    error("normalized table schema is missing fields")
  end
  for _, field_name in ipairs(sorted_field_names(schema.fields)) do
    local field = schema.fields[field_name]
    if field == nil then
      error("normalized table schema contains a missing field")
    end
    emit_field(lines, field_name, field, field_name, link or unlinked)
  end
end

---Generate a Neovim help document from a normalized schema.
---@param schema louiselm.schema.Schema Normalized schema returned by `Schema.define`.
---@return string vimdoc A deterministic `louiselm.txt` help document.
function M.generate(schema)
  local lines = { "*louiselm.txt*  louiselm.nvim configuration reference", "", Text.SECTION_RULE }
  Text.append_heading(lines, "Contents", "louiselm-contents")
  lines[#lines + 1] = ""
  lines[#lines + 1] = "  |louiselm|          louiselm.nvim configuration reference"
  lines[#lines + 1] = "  Configuration options are documented below."
  lines[#lines + 1] = ""
  lines[#lines + 1] = Text.SECTION_RULE
  Text.append_heading(lines, "Configuration", "louiselm")
  M.append_configuration_fields(lines, schema)
  lines[#lines + 1] = ""
  return table.concat(lines, "\n") .. "\n"
end

return M
