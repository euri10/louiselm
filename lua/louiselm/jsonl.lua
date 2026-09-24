---Bounded, schema-independent summaries of JSON records. No UI or file I/O.
---@diagnostic disable-next-line: undefined-global -- Neovim's native JSON codec.
local nvim = vim
local M = {}

local function text(value)
  local encoded = nvim.json.encode(value)
  local escaped = encoded:sub(2, -2)
  if nvim.fn.strchars(escaped) > 80 then
    return nvim.fn.strcharpart(escaped, 0, 80) .. "…"
  end
  return escaped
end

local function scalar(value)
  if value == nvim.NIL then
    return "null"
  elseif type(value) == "string" then
    return text(value)
  elseif type(value) == "table" then
    return nvim.islist(value) and ("[" .. #value .. " items]") or "{…}"
  end
  return tostring(value)
end

---Summarize at most eight fields/items from one record of at most 16 KiB.
---Strings are escaped to one display line; nested values remain compact.
---@param line string Original JSON line, never modified.
---@return string? summary Nil means the view should retain the original line.
---@return string? error_message Invalid or oversized input; never includes input bytes.
function M.summary(line)
  if type(line) ~= "string" or #line > 16384 then
    return nil, "JSONL record must be a string of at most 16 KiB"
  end
  local ok, value = pcall(nvim.json.decode, line)
  if not ok then
    return nil, "invalid JSONL record"
  end
  if type(value) ~= "table" then
    return scalar(value)
  end
  local parts = {}
  if nvim.islist(value) then
    for index = 1, math.min(#value, 8) do
      parts[#parts + 1] = scalar(value[index])
    end
    if #value > 8 then
      parts[#parts + 1] = "…"
    end
    return "[" .. table.concat(parts, " · ") .. "]"
  end
  local keys = nvim.tbl_keys(value)
  table.sort(keys)
  for index = 1, math.min(#keys, 8) do
    local key = keys[index]
    parts[#parts + 1] = text(key) .. "=" .. scalar(value[key])
  end
  if #keys > 8 then
    parts[#parts + 1] = "…"
  end
  return #keys == 0 and "{}" or table.concat(parts, " · ")
end

return M
