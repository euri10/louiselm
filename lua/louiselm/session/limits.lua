---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

---@alias louiselm.session.LimitsStatus "loading"|"fresh"|"stale"|"unavailable"|"unsupported"|"empty"|"unlimited"|"not_observed"

---@class louiselm.session.LimitWindow
---@field used_percent number Consumed capacity from zero through one hundred.
---@field duration_mins integer Exact quota-window duration in minutes.
---@field resets_at integer Unix timestamp in seconds for the next reset.

---@class louiselm.session.LimitCredits
---@field balance? number Remaining workspace credit balance.
---@field unlimited? boolean Whether workspace credits are explicitly unlimited.

---@class louiselm.session.LimitBucket
---@field id string Stable metered bucket identifier.
---@field label? string Optional user-facing bucket label.
---@field windows louiselm.session.LimitWindow[] Quota windows for this bucket.
---@field reached_type? string Provider classification for a reached limit.
---@field plan_type? string Plan associated with this bucket.
---@field credits? louiselm.session.LimitCredits Optional workspace credit state.

---@class louiselm.session.LimitResetCredit
---@field id? string Opaque reset-credit identifier.
---@field expires_at? integer Unix expiry timestamp in seconds.
---@field title? string User-facing credit title.
---@field description? string User-facing credit description.

---@class louiselm.session.LimitResetCredits
---@field available_count integer Authoritative available reset-credit count.
---@field credits? louiselm.session.LimitResetCredit[] Optional detail rows.

---@class louiselm.session.LimitsSnapshot
---@field default_bucket_id? string Bucket corresponding to the adapter default view.
---@field buckets louiselm.session.LimitBucket[] Complete normalized bucket list.
---@field unlimited? boolean Whether account limits are explicitly unlimited.
---@field reset_credits? louiselm.session.LimitResetCredits Optional earned reset credits.

---@class louiselm.session.LimitsState
---@field agent string Configured Agent name.
---@field status louiselm.session.LimitsStatus Current observation state.
---@field snapshot? louiselm.session.LimitsSnapshot Last good complete snapshot.
---@field updated_at? integer Local Unix timestamp when the last good snapshot arrived.
---@field error? string Sanitized reason current data is unavailable or stale.

---@class louiselm.session.LimitsCapability
---@field read_method string ACP custom request returning a complete snapshot.
---@field updated_method string ACP custom notification carrying a complete snapshot.

local M = {}

M.META_KEY = "io.github.euri10.louiselm"

---@param value unknown
---@return boolean
local function is_absent(value)
  return value == nil or value == nvim.NIL
end

---@param value unknown
---@return boolean
local function is_dense_array(value)
  if type(value) ~= "table" then
    return false
  end
  local length = #value
  for key in pairs(value) do
    if type(key) ~= "number" or key % 1 ~= 0 or key < 1 or key > length then
      return false
    end
  end
  return true
end

---@param value unknown
---@return string? result
---@return boolean valid
local function optional_string(value)
  if is_absent(value) then
    return nil, true
  end
  if type(value) ~= "string" or value == "" then
    return nil, false
  end
  return value, true
end

---@param value unknown
---@return louiselm.session.LimitsCapability? capability
function M.capability(value)
  if type(value) ~= "table" or type(value._meta) ~= "table" then
    return nil
  end
  local namespace = value._meta[M.META_KEY]
  local capability = type(namespace) == "table" and namespace.accountLimits or nil
  if type(capability) ~= "table" or capability.version ~= 1 then
    return nil
  end
  local read_method = capability.readMethod
  local updated_method = capability.updatedMethod
  if
    type(read_method) ~= "string"
    or read_method:sub(1, 1) ~= "_"
    or type(updated_method) ~= "string"
    or updated_method:sub(1, 1) ~= "_"
  then
    return nil
  end
  return { read_method = read_method, updated_method = updated_method }
end

---@param value unknown
---@return louiselm.session.LimitWindow? window
local function window(value)
  if type(value) ~= "table" then
    return nil
  end
  local used = value.usedPercent
  local duration = value.windowDurationMins
  local resets_at = value.resetsAt
  if
    type(used) ~= "number"
    or used < 0
    or used > 100
    or type(duration) ~= "number"
    or duration % 1 ~= 0
    or duration <= 0
    or type(resets_at) ~= "number"
    or resets_at % 1 ~= 0
    or resets_at <= 0
  then
    return nil
  end
  return { used_percent = used, duration_mins = duration, resets_at = resets_at }
end

---@param value unknown
---@return louiselm.session.LimitCredits? credits
---@return boolean valid
local function credits(value)
  if is_absent(value) then
    return nil, true
  end
  if type(value) ~= "table" then
    return nil, false
  end
  local result = {}
  if not is_absent(value.balance) then
    if type(value.balance) ~= "number" or value.balance < 0 then
      return nil, false
    end
    result.balance = value.balance
  end
  if not is_absent(value.unlimited) then
    if type(value.unlimited) ~= "boolean" then
      return nil, false
    end
    result.unlimited = value.unlimited
  end
  return result, true
end

---@param value unknown
---@return louiselm.session.LimitResetCredits? result
---@return boolean valid
local function reset_credits(value)
  if is_absent(value) then
    return nil, true
  end
  if type(value) ~= "table" then
    return nil, false
  end
  local count = value.availableCount
  if type(count) ~= "number" or count % 1 ~= 0 or count < 0 then
    return nil, false
  end
  local result = { available_count = count }
  if not is_absent(value.credits) then
    if not is_dense_array(value.credits) then
      return nil, false
    end
    result.credits = {}
    for _, item in ipairs(value.credits) do
      if type(item) ~= "table" then
        return nil, false
      end
      local id, id_valid = optional_string(item.id)
      local title, title_valid = optional_string(item.title)
      local description, description_valid = optional_string(item.description)
      local expires_at = item.expiresAt
      if
        not id_valid
        or not title_valid
        or not description_valid
        or (not is_absent(expires_at) and (type(expires_at) ~= "number" or expires_at % 1 ~= 0 or expires_at <= 0))
      then
        return nil, false
      end
      result.credits[#result.credits + 1] = {
        id = id,
        expires_at = not is_absent(expires_at) and expires_at or nil,
        title = title,
        description = description,
      }
    end
  end
  return result, true
end

---Validate and normalize one complete account-limits extension snapshot.
---@param value unknown ACP extension payload.
---@return louiselm.session.LimitsSnapshot? snapshot
---@return string? error_message Why the payload was rejected.
function M.snapshot(value)
  if type(value) ~= "table" or not is_dense_array(value.buckets) then
    return nil, "account limits snapshot must contain a dense buckets array"
  end
  if not is_absent(value.unlimited) and type(value.unlimited) ~= "boolean" then
    return nil, "account limits unlimited must be a boolean"
  end
  local result = { buckets = {} }
  if not is_absent(value.unlimited) then
    result.unlimited = value.unlimited
  end
  local ids = {}
  for _, raw_bucket in ipairs(value.buckets) do
    if type(raw_bucket) ~= "table" or type(raw_bucket.id) ~= "string" or raw_bucket.id == "" then
      return nil, "account limit bucket id must be a non-empty string"
    end
    if ids[raw_bucket.id] or not is_dense_array(raw_bucket.windows) then
      return nil, "account limit bucket ids must be unique and windows must be dense"
    end
    ids[raw_bucket.id] = true
    local label, label_valid = optional_string(raw_bucket.label)
    local reached_type, reached_valid = optional_string(raw_bucket.reachedType)
    local plan_type, plan_valid = optional_string(raw_bucket.planType)
    local normalized_credits, credits_valid = credits(raw_bucket.credits)
    if not label_valid or not reached_valid or not plan_valid or not credits_valid then
      return nil, "account limit bucket metadata is malformed"
    end
    local normalized_bucket = {
      id = raw_bucket.id,
      label = label,
      windows = {},
      reached_type = reached_type,
      plan_type = plan_type,
      credits = normalized_credits,
    }
    for _, raw_window in ipairs(raw_bucket.windows) do
      local normalized_window = window(raw_window)
      if normalized_window == nil then
        return nil, "account limit window is malformed"
      end
      normalized_bucket.windows[#normalized_bucket.windows + 1] = normalized_window
    end
    result.buckets[#result.buckets + 1] = normalized_bucket
  end
  local default_bucket_id, default_valid = optional_string(value.defaultBucketId)
  if not default_valid or (default_bucket_id ~= nil and not ids[default_bucket_id]) then
    return nil, "account limits default bucket is malformed"
  end
  if #result.buckets > 0 and default_bucket_id == nil then
    return nil, "account limits default bucket is missing"
  end
  result.default_bucket_id = default_bucket_id
  local normalized_reset_credits, reset_valid = reset_credits(value.resetCredits)
  if not reset_valid then
    return nil, "account limit reset credits are malformed"
  end
  result.reset_credits = normalized_reset_credits
  return result
end

return M
