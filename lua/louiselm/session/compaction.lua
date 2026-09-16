---@diagnostic disable-next-line: undefined-global -- Neovim provides JSON null and ownership copies.
local nvim = vim
local M = {}

---@class louiselm.session.Compaction
---@field id string Opaque Agent-owned ID, unique within the Session.
---@field status string ACP lifecycle status; unknown values remain opaque.
---@field summary? table[] Retained content blocks; opaque non-text blocks are preserved.
---@field error? string Agent-reported failure description.
---@field meta? table Compaction metadata, replaced by concrete patches.

local terminal = { completed = true, failed = true, cancelled = true }

---@param value unknown
---@return boolean
local function block(value)
  return type(value) == "table"
    and type(value.type) == "string"
    and value.type ~= ""
    and (value.type ~= "text" or type(value.text) == "string")
end

---@param value unknown
---@return boolean
local function blocks(value)
  if type(value) ~= "table" or not nvim.islist(value) then
    return false
  end
  for _, item in ipairs(value) do
    if not block(item) then
      return false
    end
  end
  return true
end

---Validate and apply one experimental ACP compaction update without mutating inputs.
---Unknown content types are preserved opaquely; only text is consumed for display.
---@param previous? louiselm.session.Compaction Prior entity with the same ID.
---@param update table Raw ACP compaction_update or compaction_summary_chunk.
---@return louiselm.session.Compaction? entity Owned replacement snapshot, nil on invalid input/order.
---@return string? error_message Generic diagnostic without payload content.
function M.apply(previous, update)
  local invalid = "malformed ACP compaction update or lifecycle"
  if type(update.compactionId) ~= "string" or update.compactionId == "" then
    return nil, invalid
  end
  if previous ~= nil and previous.id ~= update.compactionId then
    return nil, invalid
  end
  if update._meta ~= nil and update._meta ~= nvim.NIL then
    if type(update._meta) ~= "table" or nvim.islist(update._meta) then
      return nil, invalid
    end
  end
  if update.sessionUpdate == "compaction_summary_chunk" then
    if previous == nil or previous.status ~= "in_progress" or not block(update.content) then
      return nil, invalid
    end
    local result = nvim.deepcopy(previous)
    result.summary = result.summary or {}
    result.summary[#result.summary + 1] = nvim.deepcopy(update.content)
    return result
  end
  if update.sessionUpdate ~= "compaction_update" or type(update.status) ~= "string" or update.status == "" then
    return nil, invalid
  end
  if previous ~= nil and terminal[previous.status] and update.status ~= previous.status then
    return nil, invalid
  end
  if update.summary ~= nil and update.summary ~= nvim.NIL then
    if not blocks(update.summary) or (#update.summary > 0 and update.status ~= "completed") then
      return nil, invalid
    end
  end
  if update.error ~= nil and update.error ~= nvim.NIL then
    if type(update.error) ~= "string" or update.status ~= "failed" then
      return nil, invalid
    end
  end
  local result = previous and nvim.deepcopy(previous) or { id = update.compactionId }
  result.status = update.status
  for wire, field in pairs({ summary = "summary", error = "error", _meta = "meta" }) do
    local value = update[wire]
    if value == nvim.NIL then
      result[field] = nil
    elseif value ~= nil then
      result[field] = nvim.deepcopy(value)
    end
  end
  return result
end

---Return a text-only summary, or nil when empty or not faithfully representable as text.
---@param entity louiselm.session.Compaction
---@return string? text
function M.text(entity)
  local parts = {}
  for _, content in ipairs(entity.summary or {}) do
    if content.type ~= "text" then
      return nil
    end
    parts[#parts + 1] = content.text
  end
  local text = table.concat(parts)
  return text:match("%S") and text or nil
end

return M
