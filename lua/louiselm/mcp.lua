local Schema = require("louiselm.schema")

---@class louiselm.mcp.Server
---@field type "stdio"|"http" Transport passed to the Agent.
---@field command? string Absolute executable path for stdio.
---@field args? string[] Stdio arguments, empty by default.
---@field env? table<string, string> Stdio environment, empty by default.
---@field url? string HTTP(S) MCP endpoint for Streamable HTTP.
---@field headers? table<string, string> HTTP headers, empty by default.

---@class louiselm.mcp.Config
---@field servers table<string, louiselm.mcp.Server> Shared named server catalog.
---@field default_servers string[] Ordered selection inherited by Agents without an override.

---@class louiselm.mcp.WireServer
---@field name string Configured server name.
---@field type? "http" ACP v1 omits the type discriminator for stdio.
---@field command? string Absolute stdio executable path.
---@field args? string[] Stdio arguments.
---@field env? {name: string, value: string}[] Stdio environment.
---@field url? string Streamable HTTP endpoint.
---@field headers? {name: string, value: string}[] HTTP headers.

---@class louiselm.mcp.Summary
---@field name string Configured server name, safe to display.
---@field transport "stdio"|"http" Transport, without command, URL or credentials.

local M = {}

local function valid_name(value)
  return value:find("%S") ~= nil and value:find("%c") == nil, "must be a nonblank name without control characters"
end

local function no_nul(value)
  return value:find("\0", 1, true) == nil, "must not contain NUL characters"
end

local function unique_names(value)
  local seen = {}
  for _, name in ipairs(value) do
    if type(name) == "string" then
      if seen[name] then
        return false, "server names must be unique"
      end
      seen[name] = true
    end
  end
  return true
end

local selection = {
  type = "array-of",
  optional = true,
  items = { type = "string", validator = valid_name },
  validator = unique_names,
  description = "Ordered MCP server names replacing global defaults; omission inherits and an empty list disables LouiseLM-supplied MCP servers.",
}

---Shared Agent selection field for setup and headless configuration.
---@type louiselm.schema.Schema
M.selection_schema = assert(Schema.define({ mcp_servers = selection }))

---Shared closed MCP schema; transport defaults are applied only after validation.
---@type louiselm.schema.Schema
M.schema = assert(Schema.define({
  mcp = {
    type = "table",
    default = {},
    description = "MCP servers supplied to ACP Agents; the Agent owns server connections and tools.",
    fields = {
      servers = {
        type = "map-of",
        default = {},
        description = "Shared named server catalog; registering a server does not enable it.",
        validator = function(value)
          for name in pairs(value) do
            if type(name) == "string" and not valid_name(name) then
              return false, "server names must be nonblank and contain no control characters"
            end
          end
          return true
        end,
        items = {
          type = "table",
          validator = function(value)
            if value.type == "stdio" then
              return value.command ~= nil and value.url == nil and value.headers == nil,
                "stdio requires command and does not accept url or headers"
            elseif value.type == "http" then
              return value.url ~= nil and value.command == nil and value.args == nil and value.env == nil,
                "http requires url and does not accept command, args or env"
            end
            return true
          end,
          fields = {
            type = {
              type = "string",
              description = "Required transport: stdio or http (Streamable HTTP).",
              validator = function(value)
                return value == "stdio" or value == "http", "must be stdio or http"
              end,
            },
            command = {
              type = "string",
              optional = true,
              description = "Required for stdio: absolute executable path, as required by ACP v1.",
              validator = function(value)
                local absolute = value:sub(1, 1) == "/" or value:match("^%a:[/\\]") ~= nil or value:sub(1, 2) == "\\\\"
                return absolute and value:find("%c") == nil,
                  "must be an absolute executable path without control characters"
              end,
            },
            args = {
              type = "array-of",
              optional = true,
              items = { type = "string", validator = no_nul },
              description = "Stdio arguments; omission uses an empty list.",
            },
            env = {
              type = "map-of",
              optional = true,
              items = { type = "string", validator = no_nul },
              description = "Stdio environment variables; omission uses an empty map. Values are never displayed in MCP inspection.",
              validator = function(value)
                for name in pairs(value) do
                  if type(name) == "string" and name:find("[=%c]") ~= nil then
                    return false, "environment names must not contain equals signs or control characters"
                  end
                end
                return true
              end,
            },
            url = {
              type = "string",
              optional = true,
              description = "Required for http: absolute HTTP(S) Streamable HTTP endpoint.",
              validator = function(value)
                return value:match("^https?://[^/%s?#]+") ~= nil and value:find("[%s%c]") == nil,
                  "must be an absolute HTTP(S) URL without whitespace or control characters"
              end,
            },
            headers = {
              type = "map-of",
              optional = true,
              description = "HTTP headers; omission uses an empty map. Values are never displayed in MCP inspection.",
              items = {
                type = "string",
                validator = function(value)
                  return value:find("%c") == nil, "HTTP header values must not contain control characters"
                end,
              },
              validator = function(value)
                local seen = {}
                for name in pairs(value) do
                  if type(name) == "string" then
                    if name:match("^[!#$%%&'*+.^_`|~%w-]+$") == nil or seen[name:lower()] then
                      return false, "HTTP header names must be valid tokens and unique ignoring case"
                    end
                    seen[name:lower()] = true
                  end
                end
                return true
              end,
            },
          },
        },
      },
      default_servers = {
        type = "array-of",
        default = {},
        items = selection.items,
        validator = unique_names,
        description = "Ordered server names inherited by ordinary Sessions unless their Agent overrides the selection; empty by default.",
      },
    },
  },
}))

local function copy(value)
  if type(value) ~= "table" then
    return value
  end
  local owned = {}
  for key, child in pairs(value) do
    owned[key] = copy(child)
  end
  return owned
end

---Validate catalog and selection references, then return an owned normalized configuration.
---Returns nil and every error without including server argument, environment or header values.
---@param raw_mcp unknown Omission means an empty catalog and default selection.
---@param definitions? unknown Structurally validated Agent definitions whose optional selections refer to this catalog.
---@return louiselm.mcp.Config? config
---@return louiselm.agent.ConfigError[] errors
function M.normalize(raw_mcp, definitions)
  local errors = {}
  for _, err in ipairs(Schema.validate(M.schema, { mcp = raw_mcp })) do
    errors[#errors + 1] = {
      path = err.path,
      type = err.type == "validation_failed" and "invalid_value" or err.type,
      message = err.message
        or (err.type == "unknown_key" and "unknown MCP configuration key" or "invalid MCP configuration field"),
      expected = err.expected,
      got = err.got,
    }
  end
  local raw = type(raw_mcp) == "table" and raw_mcp or {}
  local servers = type(raw.servers) == "table" and raw.servers or {}
  local function check_references(names, path)
    if type(names) ~= "table" then
      return
    end
    for index, name in ipairs(names) do
      if type(name) == "string" and valid_name(name) and servers[name] == nil then
        errors[#errors + 1] = {
          path = path .. "[" .. index .. "]",
          type = "invalid_value",
          message = "server name is not registered in mcp.servers",
        }
      end
    end
  end
  check_references(raw.default_servers, "mcp.default_servers")
  if type(definitions) == "table" then
    for name, definition in pairs(definitions) do
      if type(name) == "string" and type(definition) == "table" then
        check_references(definition.mcp_servers, "agents." .. name .. ".mcp_servers")
      end
    end
  end
  table.sort(errors, function(left, right)
    return left.path < right.path
  end)
  if #errors > 0 then
    return nil, errors
  end
  local config = { servers = copy(servers), default_servers = copy(raw.default_servers or {}) }
  for _, server in pairs(config.servers) do
    if server.type == "stdio" then
      server.args, server.env = server.args or {}, server.env or {}
    else
      server.headers = server.headers or {}
    end
  end
  return config, errors
end

local function wire_entries(values)
  local entries = {}
  for name, value in pairs(values) do
    entries[#entries + 1] = { name = name, value = value }
  end
  table.sort(entries, function(left, right)
    return left.name < right.name
  end)
  return entries
end

---Resolve a validated selection into detached ACP v1 wire descriptions.
---The caller must validate configuration and Agent selections before resolution.
---@param config louiselm.mcp.Config Normalized catalog and defaults.
---@param names? string[] Omission inherits defaults; an empty selection disables MCP.
---@return louiselm.mcp.WireServer[] servers Owned descriptions in selection order.
function M.resolve(config, names)
  local servers = {}
  for _, name in ipairs(names or config.default_servers) do
    local server = config.servers[name]
    if server.type == "stdio" then
      servers[#servers + 1] =
        { name = name, command = server.command, args = copy(server.args), env = wire_entries(server.env) }
    else
      servers[#servers + 1] = { name = name, type = "http", url = server.url, headers = wire_entries(server.headers) }
    end
  end
  return servers
end

return M
