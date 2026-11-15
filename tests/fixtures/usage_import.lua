-- Sanitized structural sequence from proxy/sessions/
-- 62b4d897-b815-47ec-865a-764225cf98c5/log.jsonl, 2026-08-31,
-- lines 1-58, recovered from a verified 2026-09-10 local backup. Unrelated
-- updates and all content are omitted; option labels and token values are
-- synthetic. The connection records below are synthetic: the captured
-- 699e9a8a-e8cb-46fa-80cd-0c4d0fd22398 connection file no longer exists.
local session = "62b4d897-b815-47ec-865a-764225cf98c5"

local function options(mode, model, effort)
  local result = {
    {
      id = "mode",
      name = "Mode",
      type = "select",
      category = "mode",
      currentValue = mode,
      options = { { value = "default", name = "Default" }, { value = "bypassPermissions", name = "Bypass" } },
    },
    {
      id = "model",
      name = "Model",
      type = "select",
      category = "model",
      currentValue = model,
      options = { { value = "default", name = "Default" }, { value = "haiku", name = "Haiku" } },
    },
  }
  if effort then
    result[#result + 1] = {
      id = "effort",
      name = "Effort",
      type = "select",
      category = "thought_level",
      currentValue = "default",
      options = { { value = "default", name = "Default" } },
    }
  end
  return result
end

local function record(timestamp, direction, payload)
  return { timestamp = timestamp, direction = direction, kind = "frame", session_id = session, payload = payload }
end

local function request(timestamp, id, method, params)
  params = params or {}
  params.sessionId = session
  return record(timestamp, "client_to_agent", { jsonrpc = "2.0", id = id, method = method, params = params })
end

local function response(timestamp, id, result)
  return record(timestamp, "agent_to_client", { jsonrpc = "2.0", id = id, result = result })
end

local function update(timestamp, data)
  return record(timestamp, "agent_to_client", {
    jsonrpc = "2.0",
    method = "session/update",
    params = { sessionId = session, update = data },
  })
end

local first = {
  response("2026-08-31T17:00:49.484Z", 2, { sessionId = session, configOptions = options("default", "default", true) }),
  request(
    "2026-08-31T17:00:56.876Z",
    3,
    "session/set_config_option",
    { configId = "mode", value = "bypassPermissions" }
  ),
  response("2026-08-31T17:00:56.883Z", 3, { configOptions = options("bypassPermissions", "default", true) }),
  request("2026-08-31T17:00:59.602Z", 4, "session/set_config_option", { configId = "model", value = "haiku" }),
  response("2026-08-31T17:00:59.614Z", 4, { configOptions = options("bypassPermissions", "haiku", false) }),
  request("2026-08-31T17:01:03.713Z", 5, "session/prompt", { prompt = { { text = "PRIVATE PROMPT" } } }),
  update("2026-08-31T17:01:04.541Z", { sessionUpdate = "usage_update", used = 10, size = 100 }),
  update(
    "2026-08-31T17:01:08.205Z",
    { sessionUpdate = "usage_update", used = 20, size = 100, cost = { amount = 0.01, currency = "USD" } }
  ),
  response(
    "2026-08-31T17:01:08.205Z",
    5,
    { stopReason = "end_turn", usage = { inputTokens = 10, outputTokens = 3, totalTokens = 13 } }
  ),
  {
    timestamp = "2026-08-31T17:01:30.526Z",
    direction = "internal",
    kind = "exit",
    session_id = session,
    payload = "PRIVATE STDERR",
  },
}

local second = {
  request("2026-08-31T17:02:01.765Z", 2, "session/load"),
  update("2026-08-31T17:02:03.324Z", { sessionUpdate = "user_message_chunk", content = { text = "PRIVATE REPLAY" } }),
  response("2026-08-31T17:02:03.325Z", 2, { sessionId = session, configOptions = options("default", "haiku", false) }),
  request("2026-08-31T17:02:26.030Z", 3, "session/prompt", { prompt = { { text = "PRIVATE PROMPT" } } }),
  update(
    "2026-08-31T17:02:29.704Z",
    { sessionUpdate = "usage_update", used = 20, size = 100, cost = { amount = 0.02, currency = "USD" } }
  ),
  response(
    "2026-08-31T17:02:29.704Z",
    3,
    { stopReason = "end_turn", usage = { inputTokens = 11, outputTokens = 4, totalTokens = 15 } }
  ),
  {
    timestamp = "2026-08-31T17:05:34.579Z",
    direction = "internal",
    kind = "exit",
    session_id = session,
    payload = "PRIVATE STDERR",
  },
}

local connection = {
  {
    timestamp = "2026-08-31T17:00:49.000Z",
    direction = "client_to_agent",
    kind = "frame",
    payload = { jsonrpc = "2.0", id = 2, method = "session/new", params = { cwd = "PRIVATE CWD" } },
  },
  {
    timestamp = "2026-08-31T17:00:49.484Z",
    direction = "internal",
    kind = "session-bound",
    session_id = session,
    payload = { sessionId = session },
  },
}
local resumed_connection = {
  {
    timestamp = "2026-08-31T17:02:01.765Z",
    direction = "internal",
    kind = "session-bound",
    session_id = session,
    payload = { sessionId = session },
  },
}

return {
  session = session,
  first = first,
  second = second,
  connection = connection,
  resumed_connection = resumed_connection,
}
