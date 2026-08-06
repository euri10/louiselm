---@class louiselm.acp.JsonRpcError
---@field code integer JSON-RPC error code.
---@field message string JSON-RPC error message.
---@field data? unknown Additional error data.

---@class louiselm.acp.JsonRpcRequest
---@field jsonrpc "2.0"
---@field id string|number
---@field method string
---@field params? unknown

---@class louiselm.acp.JsonRpcNotification
---@field jsonrpc "2.0"
---@field method string
---@field params? unknown

---@class louiselm.acp.JsonRpcResponse
---@field jsonrpc "2.0"
---@field id string|number|nil
---@field result? unknown
---@field error? louiselm.acp.JsonRpcError

---@alias louiselm.acp.JsonRpcMessage louiselm.acp.JsonRpcRequest|louiselm.acp.JsonRpcNotification|louiselm.acp.JsonRpcResponse

local M = {}

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

---@param value unknown
---@return boolean
local function is_request_id(value)
  return type(value) == "string" or type(value) == "number"
end

---@param value unknown
---@return boolean
local function is_null(value)
  return value == nvim.NIL
end

---@param value unknown
---@return boolean
local function is_integer(value)
  return type(value) == "number" and value % 1 == 0
end

---@param message unknown
---@return true|nil valid
---@return string? error_message
function M.validate(message)
  if type(message) ~= "table" then
    return nil, "message must be an object"
  end
  if message.jsonrpc ~= "2.0" then
    return nil, 'jsonrpc must be "2.0"'
  end

  local has_method = message.method ~= nil
  local has_result = message.result ~= nil
  local has_error = message.error ~= nil
  if has_method then
    if type(message.method) ~= "string" or message.method == "" then
      return nil, "method must be a non-empty string"
    end
    if has_result or has_error then
      return nil, "request or notification cannot contain result or error"
    end
    if message.id ~= nil and not is_request_id(message.id) then
      return nil, "request id must be a string or number"
    end
    if message.id == nil then
      if message.params ~= nil and type(message.params) ~= "table" then
        return nil, "params must be an object or array"
      end
      return true
    end
    if message.params ~= nil and type(message.params) ~= "table" then
      return nil, "params must be an object or array"
    end
    return true
  end

  if message.id ~= nil and not is_null(message.id) and not is_request_id(message.id) then
    return nil, "response id must be a string or number"
  end
  if has_result == has_error then
    return nil, "response must contain exactly one of result or error"
  end
  if has_error then
    if type(message.error) ~= "table" then
      return nil, "error must be an object"
    end
    if not is_integer(message.error.code) then
      return nil, "error code must be an integer"
    end
    if type(message.error.message) ~= "string" or message.error.message == "" then
      return nil, "error message must be a non-empty string"
    end
  end
  return true
end

---Build a JSON-RPC request.
---@param id string|number Request identifier.
---@param method string Method name.
---@param params? unknown Method parameters.
---@return louiselm.acp.JsonRpcRequest? request
---@return string? error_message
function M.request(id, method, params)
  if not is_request_id(id) then
    return nil, "request id must be a string or number"
  end
  if type(method) ~= "string" or method == "" then
    return nil, "method must be a non-empty string"
  end
  local request = { jsonrpc = "2.0", id = id, method = method, params = params }
  return request
end

---Build a JSON-RPC notification.
---@param method string Method name.
---@param params? unknown Method parameters.
---@return louiselm.acp.JsonRpcNotification? notification
---@return string? error_message
function M.notification(method, params)
  if type(method) ~= "string" or method == "" then
    return nil, "method must be a non-empty string"
  end
  return { jsonrpc = "2.0", method = method, params = params }
end

---Build a successful JSON-RPC response.
---@param id string|number|nil Request identifier.
---@param result unknown Result value.
---@return louiselm.acp.JsonRpcResponse response
function M.response(id, result)
  if result == nil then
    result = nvim.NIL
  end
  return { jsonrpc = "2.0", id = id, result = result }
end

---Build a JSON-RPC error response.
---@param id string|number|nil Request identifier.
---@param code integer Error code.
---@param message string Error message.
---@param data? unknown Additional error data.
---@return louiselm.acp.JsonRpcResponse response
function M.error_response(id, code, message, data)
  local rpc_error = { code = code, message = message, data = data }
  return { jsonrpc = "2.0", id = id, error = rpc_error }
end

---Encode one JSON-RPC message as a single ACP stdio line without its newline.
---@param message louiselm.acp.JsonRpcMessage Message to encode.
---@return string? encoded
---@return string? error_message
function M.encode(message)
  local valid, validation_error = M.validate(message)
  if not valid then
    return nil, validation_error
  end

  local call_ok, encoded_or_error = pcall(nvim.json.encode, message)
  if not call_ok then
    return nil, tostring(encoded_or_error)
  end
  return encoded_or_error
end

---Decode and validate one ACP stdio line.
---@param line string JSON-RPC line without its trailing newline.
---@return louiselm.acp.JsonRpcMessage? message
---@return string? error_message
function M.decode(line)
  if type(line) ~= "string" then
    return nil, "message line must be a string"
  end
  if line:find("[\r\n]", 1) ~= nil then
    return nil, "message line must not contain a newline"
  end

  local call_ok, message_or_error = pcall(nvim.json.decode, line)
  if not call_ok then
    return nil, "invalid JSON"
  end
  local valid, validation_error = M.validate(message_or_error)
  if not valid then
    return nil, validation_error
  end
  return message_or_error
end

return M
