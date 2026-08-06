---@class louiselm.schema.Report
---@field ok boolean Whether the configuration has no errors.
---@field count integer Number of reported errors.
---@field errors louiselm.schema.ValidationError[] Errors with human-readable messages.
---@field lines string[] One formatted line per error.
---@field text string All formatted lines joined by newlines.

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

---@param validation_error louiselm.schema.ValidationError
---@return string
local function error_message(validation_error)
  local path = validation_error.path == "" and "<root>" or validation_error.path
  if validation_error.type == "unknown_key" then
    if validation_error.suggestion ~= nil then
      return string.format("%s: unknown key; did you mean '%s'?", path, validation_error.suggestion)
    end
    return path .. ": unknown key"
  end
  if validation_error.type == "wrong_type" then
    return string.format(
      "%s: wrong type (expected %s, got %s; example: %s)",
      path,
      validation_error.expected,
      validation_error.got,
      format_value(validation_error.example)
    )
  end
  if validation_error.type == "missing_required" then
    return path .. ": missing required key"
  end
  if validation_error.type == "validation_failed" then
    if validation_error.message ~= nil then
      return string.format("%s: validation failed (%s)", path, validation_error.message)
    end
    return path .. ": validation failed"
  end
  return path .. ": validation error"
end

---@param validation_error louiselm.schema.ValidationError
---@return louiselm.schema.ValidationError formatted_error
local function format_error(validation_error)
  local formatted_error = {
    type = validation_error.type,
    path = validation_error.path,
    message = error_message(validation_error),
  }
  if validation_error.key ~= nil then
    formatted_error.key = validation_error.key
  end
  if validation_error.suggestion ~= nil then
    formatted_error.suggestion = validation_error.suggestion
  end
  if validation_error.expected ~= nil then
    formatted_error.expected = validation_error.expected
  end
  if validation_error.got ~= nil then
    formatted_error.got = validation_error.got
  end
  if validation_error.example ~= nil then
    formatted_error.example = validation_error.example
  end
  return formatted_error
end

---Format validation errors for startup messages and health checks.
---@param validation_errors louiselm.schema.ValidationError[] Errors returned by the schema validator.
---@return louiselm.schema.Report report Structured errors and human-readable output.
function M.format(validation_errors)
  local errors = {}
  local lines = {}
  for _, validation_error in ipairs(validation_errors) do
    local formatted_error = format_error(validation_error)
    errors[#errors + 1] = formatted_error
    lines[#lines + 1] = formatted_error.message
  end

  return {
    ok = #errors == 0,
    count = #errors,
    errors = errors,
    lines = lines,
    text = table.concat(lines, "\n"),
  }
end

return M
