local Schema = require("louiselm.schema")

---@class louiselm.agent.ProviderRoute
---@field provider string Service supplying access or quota, independent of Model manufacturer.
---@field options table<string, string|boolean> Exact advertised option IDs and typed values; all must match.

---@alias louiselm.agent.Provider string|louiselm.agent.ProviderRoute[]

local M = {}

local function nonblank(value)
  return value:find("%S") ~= nil, "Provider must name the service supplying access or quota"
end

---Shared required Provider field for setup and headless Agent validation.
---@type louiselm.schema.Schema
M.schema = assert(Schema.define({
  provider = {
    type = "one-of",
    description = "Required access/quota service: a fixed name or nonempty routes matching exact advertised option IDs and typed values. Exactly one route must match before each prompt; Model display names never determine Provider.",
    options = {
      { type = "string", validator = nonblank },
      {
        type = "array-of",
        validator = function(value)
          return #value > 0, "Provider routes must not be empty"
        end,
        items = {
          type = "table",
          fields = {
            provider = { type = "string", validator = nonblank },
            options = {
              type = "map-of",
              items = { type = "one-of", options = { { type = "string" }, { type = "boolean" } } },
              validator = function(value)
                return next(value) ~= nil and value[""] == nil, "Provider route needs nonempty option IDs and values"
              end,
            },
          },
        },
      },
    },
  },
}))

---Validate Provider configuration and return an owned copy; never coerce values.
---@param value unknown
---@return louiselm.agent.Provider? provider
---@return louiselm.schema.ValidationError[] errors
function M.normalize(value)
  local errors = Schema.validate(M.schema, { provider = value })
  if #errors > 0 then
    return nil, errors
  end
  if type(value) == "string" then
    return value, errors
  end
  local routes = {}
  for index, route in ipairs(value) do
    local options = {}
    for id, option in pairs(route.options) do
      options[id] = option
    end
    routes[index] = { provider = route.provider, options = options }
  end
  return routes, errors
end

---Resolve a validated Provider against a complete active or candidate option tuple.
---For a picker candidate, pass its value with every other active option unchanged.
---Reads caller tables only; zero or multiple matching routes fail closed.
---@param provider louiselm.agent.Provider? Normalized Agent Provider configuration.
---@param values table<string, string|boolean> Advertised option IDs and typed values.
---@return string? resolved Access/quota service.
---@return string? error_message Actionable attribution failure.
function M.resolve(provider, values)
  if type(provider) == "string" then
    return provider
  end
  if provider == nil then
    return nil, "Provider is missing; configure the Agent's provider before prompting"
  end
  local resolved
  for _, route in ipairs(provider) do
    local matches = true
    for id, value in pairs(route.options) do
      if values[id] ~= value then
        matches = false
        break
      end
    end
    if matches then
      if resolved ~= nil then
        return nil, "Provider is ambiguous; configure exactly one matching provider route before prompting"
      end
      resolved = route.provider
    end
  end
  if resolved == nil then
    return nil, "Provider is unresolved; configure a provider route for the active option values before prompting"
  end
  return resolved
end

return M
