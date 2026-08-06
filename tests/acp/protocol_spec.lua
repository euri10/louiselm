local MiniTest = require("mini.test")
local Protocol = require("louiselm.acp.protocol")

local T = MiniTest.new_set()

T["request"] = MiniTest.new_set()

T["request"]["encodes and decodes a request"] = function()
  local request = assert(Protocol.request(7, "initialize", { protocolVersion = 1 }))
  local encoded = assert(Protocol.encode(request))
  local decoded = assert(Protocol.decode(encoded))

  MiniTest.expect.equality(decoded, request)
end

T["request"]["rejects invalid ids and methods"] = function()
  ---@diagnostic disable-next-line: param-type-mismatch -- Deliberately exercise invalid external input.
  local request, request_error = Protocol.request(true, "initialize", {})
  MiniTest.expect.equality(request, nil)
  MiniTest.expect.equality(request_error, "request id must be a string or number")

  local notification, notification_error = Protocol.notification("", {})
  MiniTest.expect.equality(notification, nil)
  MiniTest.expect.equality(notification_error, "method must be a non-empty string")
end

T["response"] = MiniTest.new_set()

T["decode"] = MiniTest.new_set()

T["response"]["accepts a result or error but not both"] = function()
  local response = Protocol.response(3, { ok = true })
  MiniTest.expect.equality(Protocol.validate(response), true)

  local invalid = { jsonrpc = "2.0", id = 3, result = {}, error = { code = -1, message = "bad" } }
  local valid, validation_error = Protocol.validate(invalid)
  MiniTest.expect.equality(valid, nil)
  MiniTest.expect.equality(validation_error, "response must contain exactly one of result or error")
end

T["decode"]["reports malformed JSON and invalid messages"] = function()
  local decoded, decode_error = Protocol.decode("{")
  MiniTest.expect.equality(decoded, nil)
  MiniTest.expect.equality(type(decode_error), "string")

  local invalid, validation_error = Protocol.decode('{"jsonrpc":"1.0","method":"ping"}')
  MiniTest.expect.equality(invalid, nil)
  MiniTest.expect.equality(validation_error, 'jsonrpc must be "2.0"')
end

return T
