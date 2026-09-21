---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local root, port, result_path, enable_path = arg[1], arg[2], arg[3], arg[4]

nvim.opt.runtimepath:prepend(root)

local Permission = require("louiselm.permission")
local Session = require("louiselm.session")

local result = {
  schema = "louiselm.codex-guard-acp-driver/1",
  ready = false,
  prompt_completed = false,
  errors = {},
}

local function finish()
  nvim.fn.writefile({ nvim.json.encode(result) }, result_path)
end

local config = {
  model = "gpt-5.2",
  model_provider = "guard-fixture",
  model_providers = {
    ["guard-fixture"] = {
      name = "guard-fixture",
      base_url = "http://127.0.0.1:" .. port .. "/v1",
      wire_api = "responses",
      requires_openai_auth = false,
      request_max_retries = 0,
      stream_max_retries = 0,
    },
  },
}

local api, errors = Session.new({
  codex = {
    provider = "fixture",
    command = "/var/tmp/integration-fixture/acp-proxy",
    args = {
      "--log-root",
      "/var/tmp/integration/proxy",
      "--",
      "/var/tmp/integration-fixture/node",
      "/var/tmp/integration-fixture/codex-acp.js",
    },
    env = {
      CODEX_CONFIG = nvim.json.encode(config),
      CODEX_PATH = "/var/tmp/ow3ok-codex",
      DEFAULT_AUTH_REQUEST = nvim.json.encode({
        methodId = "gateway",
        _meta = {
          gateway = {
            baseUrl = "http://127.0.0.1:" .. port .. "/v1",
            headers = {},
            providerName = "guard-fixture",
          },
        },
      }),
      HOME = "/var/tmp/integration/home",
      INITIAL_AGENT_MODE = "agent",
      MODEL_PROVIDER = "guard-fixture",
      PATH = "/var/tmp/integration-fixture:/usr/bin:/bin",
      USER = "fixture",
      XDG_STATE_HOME = "/var/tmp/integration/state",
    },
  },
}, nil, { usage_directory = "/var/tmp/integration/usage" })

if api == nil then
  for _, item in ipairs(errors) do
    result.errors[#result.errors + 1] = item.path .. ": " .. item.message
  end
  finish()
  return
end

local ready = false
local session
local policy = assert(Permission.policy("auto-approve-scoped", {
  commands = { { "python3", "/var/tmp/integration_tool.py" } },
  paths = { "/var/tmp/integration/workspace" },
}))

session = api:create_session("codex", {
  cwd = "/var/tmp/integration/workspace",
  permission_policy = policy,
  start_timeout_ms = 30000,
}, function(created, err)
  if created == nil then
    result.errors[#result.errors + 1] = err or "session initialization failed"
  else
    ready = true
    result.ready = true
  end
end)

if session == nil then
  result.errors[#result.errors + 1] = "session start failed"
  api:dispose()
  finish()
  return
end

session:on(function(event)
  if event.type == "permission_requested" then
    result.permission_requested = true
  elseif event.type == "tool_call_started" then
    result.tool_started = true
  elseif event.type == "tool_call_finished" then
    result.tool_finished = true
  elseif event.type == "error" then
    result.errors[#result.errors + 1] = tostring(event.data and event.data.message or "session error")
  end
end)

if not nvim.wait(30000, function()
  return ready or #result.errors > 0
end, 10) or not ready then
  result.errors[#result.errors + 1] = "session did not become ready"
else
  nvim.fn.writefile({ "ready" }, "/var/tmp/integration/driver-ready")
  if not nvim.wait(15000, function()
    return nvim.uv.fs_stat(enable_path) ~= nil
  end, 10) then
    result.errors[#result.errors + 1] = "guard enablement timed out"
  else
    local completed = false
    local _, prompt_error = session:prompt("Run the offline fixture tool exactly once.", function(_, err)
      completed = true
      result.prompt_completed = err == nil
      if err ~= nil then
        result.errors[#result.errors + 1] = err
      end
    end)
    if prompt_error ~= nil then
      result.errors[#result.errors + 1] = prompt_error
    elseif not nvim.wait(45000, function()
      return completed
    end, 10) then
      result.errors[#result.errors + 1] = "prompt did not complete"
    end
  end
end

local disposed, dispose_error = api:dispose()
result.disposed = disposed
if dispose_error ~= nil then
  result.errors[#result.errors + 1] = dispose_error
end
finish()
