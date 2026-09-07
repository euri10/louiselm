local Schema = require("louiselm.schema")

---@class louiselm.agent.ProviderRoute
---@field provider string Service supplying access or quota, independent of Model manufacturer.
---@field options table<string, string|boolean> Exact advertised option IDs and typed values; all must match.

---@class louiselm.agent.ProviderPrefixes
---@field option string Advertised string-valued option ID; not a display name.
---@field prefixes table<string, string> Nonempty literal prefixes mapped to services; exactly one must match.

---@alias louiselm.agent.Provider string|louiselm.agent.ProviderRoute[]|louiselm.agent.ProviderPrefixes

local M = {}

local function nonblank(value)
  return value:find("%S") ~= nil, "Provider must name the service supplying access or quota"
end

---Shared required Provider field for setup and headless Agent validation.
---@type louiselm.schema.Schema
M.schema = assert(Schema.define({
  provider = {
    type = "one-of",
    description = "Required access/quota service: a fixed name, exact typed option routes, or { option, prefixes } mapping literal prefixes of an advertised string option to services. Exactly one route or prefix must match before each prompt; display names never determine Provider.",
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
      {
        type = "table",
        fields = {
          option = {
            type = "string",
            description = "Advertised string-valued option ID to match, not its display name.",
            validator = function(value)
              return value:find("%S") ~= nil, "Provider option must name an advertised string-valued option"
            end,
          },
          prefixes = {
            type = "map-of",
            description = "Nonempty literal, case-sensitive prefixes mapped to access/quota services. No regex or longest-match precedence; multiple matches are ambiguous even for the same service.",
            items = { type = "string", validator = nonblank },
            validator = function(value)
              return next(value) ~= nil, "Provider prefixes must not be empty"
            end,
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
  if value.option ~= nil then
    local prefixes = {}
    for prefix, service in pairs(value.prefixes) do
      prefixes[prefix] = service
    end
    return { option = value.option, prefixes = prefixes }, errors
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
---Reads caller tables only; zero or multiple matching routes/prefixes fail closed.
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
  if provider.option ~= nil then
    local value = values[provider.option]
    if type(value) ~= "string" then
      return nil, "Provider is unresolved; configure provider.option to name an active string-valued option"
    end
    for prefix, service in pairs(provider.prefixes) do
      if value:sub(1, #prefix) == prefix then
        if resolved ~= nil then
          return nil, "Provider is ambiguous; configure exactly one matching provider prefix before prompting"
        end
        resolved = service
      end
    end
  else
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
  end
  if resolved == nil then
    return nil,
      "Provider is unresolved; configure a provider route or prefix for the active option values before prompting"
  end
  return resolved
end

return M
