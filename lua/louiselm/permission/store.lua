local PrivateFile = require("louiselm.private_file")

---@alias louiselm.permission.Lifetime "session"|"always"

---@class louiselm.permission.Context
---@field session_id string Local session identifier.
---@field agent string Configured agent name.
---@field adapter louiselm.permission.Adapter Exact adapter process identity.
---@field workspace string Session working directory.

---@class louiselm.permission.Adapter
---@field command string
---@field args string[]

---@class louiselm.permission.Rule
---@field id string Stable scope identifier.
---@field decision "allow"|"deny"
---@field lifetime louiselm.permission.Lifetime
---@field session_id? string Present only for in-memory session rules.
---@field agent string
---@field adapter louiselm.permission.Adapter
---@field workspace string
---@field kind "file_edit"|"command"
---@field path? string Exact normalized file path.
---@field command? string[] Exact argv prefix.

---@class louiselm.permission.Store
---@field path string Persistent JSON file.
---@field session_rules table<string, louiselm.permission.Rule>
---@field evaluate fun(self: louiselm.permission.Store, context: louiselm.permission.Context, request: louiselm.permission.Request): louiselm.permission.Decision?, string?
---@field remember fun(self: louiselm.permission.Store, context: louiselm.permission.Context, request: louiselm.permission.Request, decision: unknown, lifetime: unknown): louiselm.permission.Rule?, string?
---@field list fun(self: louiselm.permission.Store): louiselm.permission.Rule[]?, string?
---@field revoke fun(self: louiselm.permission.Store, id: unknown): boolean, string?
---@field clear_session fun(self: louiselm.permission.Store, session_id: unknown): boolean

local M = {}
local Store = {}
Store.__index = Store

---@return table
local function nvim()
  ---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
  return vim
end

---@param value unknown
---@param allow_empty? boolean
---@return boolean
local function is_dense_string_array(value, allow_empty)
  if type(value) ~= "table" then
    return false
  end
  local length = 0
  while value[length + 1] ~= nil do
    if type(value[length + 1]) ~= "string" or (not allow_empty and value[length + 1] == "") then
      return false
    end
    length = length + 1
  end
  for key in pairs(value) do
    if type(key) ~= "number" or key < 1 or key % 1 ~= 0 or key > length then
      return false
    end
  end
  return true
end

---@param values string[]
---@return string[]
local function copy_strings(values)
  local result = {}
  for index, value in ipairs(values) do
    result[index] = value
  end
  return result
end

---@param value louiselm.permission.Rule
---@return louiselm.permission.Rule
local function copy_rule(value)
  return {
    id = value.id,
    decision = value.decision,
    lifetime = value.lifetime,
    session_id = value.session_id,
    agent = value.agent,
    adapter = { command = value.adapter.command, args = copy_strings(value.adapter.args) },
    workspace = value.workspace,
    kind = value.kind,
    path = value.path,
    command = value.command and copy_strings(value.command) or nil,
  }
end

---@param value string
---@return string
local function canonical_path(value)
  local editor = nvim()
  local normalized = editor.fs.normalize(value)
  if editor.fs.abspath(normalized) ~= normalized then
    normalized = editor.fs.abspath(normalized)
  end
  return editor.fs.normalize(editor.uv.fs_realpath(normalized) or normalized)
end

---@param value string
---@param workspace string
---@return string
local function operation_path(value, workspace)
  local editor = nvim()
  local normalized = editor.fs.normalize(value)
  if editor.fs.abspath(normalized) ~= normalized then
    normalized = editor.fs.joinpath(workspace, normalized)
  end
  return canonical_path(normalized)
end

---@param context unknown
---@return louiselm.permission.Context? normalized
---@return string? error_message
local function normalize_context(context)
  if type(context) ~= "table" then
    return nil, "permission context must be a table"
  end
  if type(context.session_id) ~= "string" or context.session_id == "" then
    return nil, "permission context session_id must be a non-empty string"
  end
  if type(context.agent) ~= "string" or context.agent == "" then
    return nil, "permission context agent must be a non-empty string"
  end
  if type(context.workspace) ~= "string" or context.workspace == "" then
    return nil, "permission context workspace must be a non-empty string"
  end
  if
    type(context.adapter) ~= "table"
    or type(context.adapter.command) ~= "string"
    or context.adapter.command == ""
    or not is_dense_string_array(context.adapter.args, true)
  then
    return nil, "permission context adapter must contain a command and dense args"
  end
  return {
    session_id = context.session_id,
    agent = context.agent,
    adapter = { command = context.adapter.command, args = copy_strings(context.adapter.args) },
    workspace = canonical_path(context.workspace),
  }
end

---@param parts string[]
---@param value string
local function add_identity_part(parts, value)
  parts[#parts + 1] = tostring(#value) .. ":" .. value
end

---@param rule louiselm.permission.Rule
---@return string
local function rule_id(rule)
  local parts = {}
  add_identity_part(parts, rule.lifetime)
  if rule.session_id ~= nil then
    add_identity_part(parts, rule.session_id)
  end
  add_identity_part(parts, rule.agent)
  add_identity_part(parts, rule.adapter.command)
  for _, argument in ipairs(rule.adapter.args) do
    add_identity_part(parts, argument)
  end
  add_identity_part(parts, rule.workspace)
  add_identity_part(parts, rule.kind)
  if rule.path ~= nil then
    add_identity_part(parts, rule.path)
  else
    for _, argument in ipairs(rule.command or {}) do
      add_identity_part(parts, argument)
    end
  end
  return "rule-" .. nvim().fn.sha256(table.concat(parts, "|"))
end

---@param value table
---@param allowed table<string, boolean>
---@return boolean
local function has_only_keys(value, allowed)
  for key in pairs(value) do
    if not allowed[key] then
      return false
    end
  end
  return true
end

---@param value unknown
---@return louiselm.permission.Rule? rule
local function validate_persistent_rule(value)
  if type(value) ~= "table" then
    return nil
  end
  if
    not has_only_keys(value, {
      id = true,
      decision = true,
      lifetime = true,
      agent = true,
      adapter = true,
      workspace = true,
      kind = true,
      path = true,
      command = true,
    })
  then
    return nil
  end
  if
    type(value.id) ~= "string"
    or (value.decision ~= "allow" and value.decision ~= "deny")
    or value.lifetime ~= "always"
    or type(value.agent) ~= "string"
    or value.agent == ""
    or type(value.workspace) ~= "string"
    or value.workspace == ""
    or type(value.adapter) ~= "table"
    or not has_only_keys(value.adapter, { command = true, args = true })
    or type(value.adapter.command) ~= "string"
    or value.adapter.command == ""
    or not is_dense_string_array(value.adapter.args, true)
  then
    return nil
  end
  local rule = {
    id = value.id,
    decision = value.decision,
    lifetime = "always",
    agent = value.agent,
    adapter = { command = value.adapter.command, args = copy_strings(value.adapter.args) },
    workspace = value.workspace,
    kind = value.kind,
    path = value.path,
    command = value.command and copy_strings(value.command) or nil,
  }
  if rule.kind == "file_edit" then
    if type(rule.path) ~= "string" or rule.path == "" or rule.command ~= nil then
      return nil
    end
  elseif rule.kind == "command" then
    if rule.path ~= nil or not is_dense_string_array(rule.command) then
      return nil
    end
  else
    return nil
  end
  if rule.id ~= rule_id(rule) then
    return nil
  end
  return rule
end

---@param path string
---@return louiselm.permission.Rule[]? rules
---@return string? error_message
local function read_rules(path)
  local editor = nvim()
  local stat = editor.uv.fs_stat(path)
  if stat == nil then
    return {}, nil
  end
  if stat.type ~= "file" then
    return nil, "permission state path is not a regular file"
  end
  local file, open_error = editor.uv.fs_open(path, "r", 384)
  if file == nil then
    return nil, "could not read permission state: " .. tostring(open_error)
  end
  local content, read_error = editor.uv.fs_read(file, stat.size, 0)
  local closed, close_error = editor.uv.fs_close(file)
  if content == nil then
    return nil, "could not read permission state: " .. tostring(read_error)
  end
  if not closed then
    return nil, "could not close permission state: " .. tostring(close_error)
  end
  local decoded_ok, decoded = pcall(editor.json.decode, content)
  if not decoded_ok then
    return nil, "permission state is not valid JSON"
  end
  if
    type(decoded) ~= "table"
    or not has_only_keys(decoded, { version = true, rules = true })
    or decoded.version ~= 1
    or type(decoded.rules) ~= "table"
  then
    return nil, "permission state has invalid schema"
  end
  local rules = {}
  for index, value in ipairs(decoded.rules) do
    local rule = validate_persistent_rule(value)
    if rule == nil then
      return nil, "permission state has invalid schema"
    end
    rules[index] = rule
  end
  for key in pairs(decoded.rules) do
    if type(key) ~= "number" or key < 1 or key % 1 ~= 0 or key > #rules then
      return nil, "permission state has invalid schema"
    end
  end
  return rules, nil
end

---@param path string
---@param rules louiselm.permission.Rule[]
---@return boolean written
---@return string? error_message
local function write_rules(path, rules)
  local editor = nvim()
  local directory = editor.fs.dirname(path)
  if editor.fn.mkdir(directory, "p", 448) == 0 and editor.fn.isdirectory(directory) ~= 1 then
    return false, "could not create permission state directory"
  end
  table.sort(rules, function(left, right)
    return left.id < right.id
  end)
  local encoded_ok, content = pcall(editor.json.encode, { version = 1, rules = rules })
  if not encoded_ok then
    return false, "could not encode permission state"
  end
  return PrivateFile.write(editor.uv, path, content, "permission state", "replace")
end

---@param left louiselm.permission.Adapter
---@param right louiselm.permission.Adapter
---@return boolean
local function same_adapter(left, right)
  if left.command ~= right.command or #left.args ~= #right.args then
    return false
  end
  for index, argument in ipairs(left.args) do
    if right.args[index] ~= argument then
      return false
    end
  end
  return true
end

---@param command string[]
---@param prefix string[]
---@return boolean
local function command_has_prefix(command, prefix)
  if #command < #prefix then
    return false
  end
  for index, argument in ipairs(prefix) do
    if command[index] ~= argument then
      return false
    end
  end
  return true
end

---@param rule louiselm.permission.Rule
---@param context louiselm.permission.Context
---@param request louiselm.permission.Request
---@return boolean
local function matches(rule, context, request)
  if
    rule.agent ~= context.agent
    or rule.workspace ~= context.workspace
    or not same_adapter(rule.adapter, context.adapter)
    or (rule.session_id ~= nil and rule.session_id ~= context.session_id)
    or rule.kind ~= request.kind
  then
    return false
  end
  if rule.kind == "file_edit" then
    return type(request.path) == "string" and operation_path(request.path, context.workspace) == rule.path
  end
  return is_dense_string_array(request.command) and command_has_prefix(request.command, rule.command)
end

---@param context louiselm.permission.Context
---@param request louiselm.permission.Request
---@param decision "allow"|"deny"
---@param lifetime louiselm.permission.Lifetime
---@return louiselm.permission.Rule? rule
---@return string? error_message
local function make_rule(context, request, decision, lifetime)
  local rule = {
    id = "",
    decision = decision,
    lifetime = lifetime,
    session_id = lifetime == "session" and context.session_id or nil,
    agent = context.agent,
    adapter = { command = context.adapter.command, args = copy_strings(context.adapter.args) },
    workspace = context.workspace,
    kind = request.kind,
  }
  if request.kind == "file_edit" and type(request.path) == "string" and request.path ~= "" then
    rule.path = operation_path(request.path, context.workspace)
  elseif request.kind == "command" and is_dense_string_array(request.command) then
    rule.command = copy_strings(request.command)
  else
    return nil, "only valid file_edit and command requests can be remembered"
  end
  rule.id = rule_id(rule)
  return rule, nil
end

---Create an explicit permission store.
---@param path? string JSON path. Defaults below stdpath("state").
---@return louiselm.permission.Store? store
---@return string? error_message
function M.new(path)
  if path ~= nil and (type(path) ~= "string" or path == "") then
    return nil, "permission state path must be a non-empty string"
  end
  local editor = nvim()
  local resolved = path or editor.fs.joinpath(editor.fn.stdpath("state"), "louiselm", "permissions.json")
  return setmetatable({ path = canonical_path(resolved), session_rules = {} }, Store), nil
end

---Evaluate remembered rules without broadening their adapter or workspace scope.
---@param self louiselm.permission.Store
---@param context louiselm.permission.Context
---@param request louiselm.permission.Request
---@return louiselm.permission.Decision? decision
---@return string? error_message
function Store:evaluate(context, request)
  local normalized, context_error = normalize_context(context)
  if normalized == nil then
    return nil, context_error
  end
  for _, rule in pairs(self.session_rules) do
    if matches(rule, normalized, request) then
      return rule.decision, nil
    end
  end
  local rules, read_error = read_rules(self.path)
  if rules == nil then
    return nil, read_error
  end
  for _, rule in ipairs(rules) do
    if matches(rule, normalized, request) then
      return rule.decision, nil
    end
  end
  return "ask", nil
end

---Remember an exact operation scope for this session or future sessions.
---@param self louiselm.permission.Store
---@param context louiselm.permission.Context
---@param request louiselm.permission.Request
---@param decision unknown
---@param lifetime unknown
---@return louiselm.permission.Rule? rule
---@return string? error_message
function Store:remember(context, request, decision, lifetime)
  if decision ~= "allow" and decision ~= "deny" then
    return nil, "remembered permission decision must be allow or deny"
  end
  if lifetime ~= "session" and lifetime ~= "always" then
    return nil, "remembered permission lifetime must be session or always"
  end
  local normalized, context_error = normalize_context(context)
  if normalized == nil then
    return nil, context_error
  end
  local rule, rule_error = make_rule(normalized, request, decision, lifetime)
  if rule == nil then
    return nil, rule_error
  end
  if lifetime == "session" then
    self.session_rules[rule.id] = rule
    return copy_rule(rule), nil
  end
  local rules, read_error = read_rules(self.path)
  if rules == nil then
    return nil, read_error
  end
  local replaced = false
  for index, existing in ipairs(rules) do
    if existing.id == rule.id then
      rules[index] = rule
      replaced = true
      break
    end
  end
  if not replaced then
    rules[#rules + 1] = rule
  end
  local written, write_error = write_rules(self.path, rules)
  if not written then
    return nil, write_error
  end
  return copy_rule(rule), nil
end

---List persistent and live session rules as detached values.
---@param self louiselm.permission.Store
---@return louiselm.permission.Rule[]? rules
---@return string? error_message
function Store:list()
  local persistent, read_error = read_rules(self.path)
  if persistent == nil then
    return nil, read_error
  end
  local rules = {}
  for _, rule in ipairs(persistent) do
    rules[#rules + 1] = copy_rule(rule)
  end
  for _, rule in pairs(self.session_rules) do
    rules[#rules + 1] = copy_rule(rule)
  end
  table.sort(rules, function(left, right)
    return left.id < right.id
  end)
  return rules, nil
end

---Revoke one rule by stable identifier.
---@param self louiselm.permission.Store
---@param id unknown
---@return boolean revoked
---@return string? error_message
function Store:revoke(id)
  if type(id) ~= "string" or id == "" then
    return false, "permission rule id must be a non-empty string"
  end
  if self.session_rules[id] ~= nil then
    self.session_rules[id] = nil
    return true, nil
  end
  local rules, read_error = read_rules(self.path)
  if rules == nil then
    return false, read_error
  end
  local kept = {}
  local revoked = false
  for _, rule in ipairs(rules) do
    if rule.id == id then
      revoked = true
    else
      kept[#kept + 1] = rule
    end
  end
  if not revoked then
    return false, nil
  end
  local written, write_error = write_rules(self.path, kept)
  if not written then
    return false, write_error
  end
  return true, nil
end

---Forget all memory-only rules for a disposed session.
---@param self louiselm.permission.Store
---@param session_id unknown
---@return boolean cleared
function Store:clear_session(session_id)
  if type(session_id) ~= "string" or session_id == "" then
    return false
  end
  for id, rule in pairs(self.session_rules) do
    if rule.session_id == session_id then
      self.session_rules[id] = nil
    end
  end
  return true
end

return M
