local MiniTest = require("mini.test")
local Session = require("louiselm.session")
local TranscriptExport = require("louiselm.session.transcript_export")

local T = MiniTest.new_set()

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim
local project_root = nvim.fn.getcwd()

---@param overrides? table
---@return table
local function mock_definition(overrides)
  overrides = overrides or {}
  local env = {}
  if overrides.mode ~= nil then
    env.LOUISELM_MOCK_MODE = overrides.mode
  end
  if overrides.crash_on ~= nil then
    env.LOUISELM_MOCK_CRASH_ON = overrides.crash_on
  end
  if overrides.replay_user_message ~= nil then
    env.LOUISELM_MOCK_REPLAY_USER = overrides.replay_user_message
  end
  if overrides.replay_reasoning ~= nil then
    env.LOUISELM_MOCK_REPLAY_REASONING = overrides.replay_reasoning
  end
  return {
    command = nvim.v.progpath,
    args = {
      "--headless",
      "--noplugin",
      "-u",
      project_root .. "/tests/mock/init.lua",
      "-c",
      "lua require('louiselm.dev.mock_agent').run()",
    },
    env = env,
  }
end

local function read_file(path)
  return table.concat(nvim.fn.readfile(path), "\n")
end

T["export"] = MiniTest.new_set()

T["export"]["requires a non-empty output path"] = function()
  -- Path validation must short-circuit before the api is ever touched, so an api
  -- stub missing `load_session` is fine here.
  local unused_api = {} --[[@as louiselm.session.SessionLoader]]
  local written_path, export_error = TranscriptExport.export(unused_api, "agent", "session-id", "")

  MiniTest.expect.equality(written_path, nil)
  MiniTest.expect.equality(export_error, "output path must be a non-empty string")
end

T["export"]["reports the registry's error for an empty ACP session id without starting an agent"] = function()
  local api = assert(Session.new({ mock = mock_definition() }))

  local written_path, export_error = TranscriptExport.export(api, "mock", "", nvim.fn.tempname() .. ".md")

  MiniTest.expect.equality(written_path, nil)
  MiniTest.expect.equality(export_error, "ACP session id must be a non-empty string")
  MiniTest.expect.equality(api:list_sessions(), {})
  api:dispose()
end

T["export"]["times out and disposes the session when the agent never becomes ready"] = function()
  local disposed = false
  local fake_session = {
    inspect = function()
      return { id = "session-1", agent = "mock", status = "starting" }
    end,
    on = function(_, callback)
      return function() end
    end,
    dispose = function()
      disposed = true
      return true
    end,
  }
  local fake_api = {
    load_session = function(_, agent_name, acp_session_id, options, _ready_callback)
      MiniTest.expect.equality(agent_name, "mock")
      MiniTest.expect.equality(acp_session_id, "never-ready")
      MiniTest.expect.equality(type(options.on_event), "function")
      -- `ready_callback` is intentionally never invoked: this models an agent that
      -- hangs during session/load.
      return fake_session
    end,
  }

  local written_path, export_error =
    TranscriptExport.export(fake_api, "mock", "never-ready", nvim.fn.tempname() .. ".md", 50)

  MiniTest.expect.equality(written_path, nil)
  MiniTest.expect.equality(export_error, "timed out waiting for agent 'mock' to load session never-ready")
  MiniTest.expect.equality(disposed, true)
end

T["export"]["reports a real agent crash during session/load as the export error"] = function()
  local api = assert(Session.new({ mock = mock_definition({ crash_on = "session/load" }) }))

  local written_path, export_error = TranscriptExport.export(api, "mock", "any-session", nvim.fn.tempname() .. ".md")

  MiniTest.expect.equality(written_path, nil)
  MiniTest.expect.equality(export_error, "agent process exited with code 23")
  api:dispose()
end

T["export"]["loads a session, captures its replayed history, and exports the full transcript"] = function()
  local api = assert(Session.new({ mock = mock_definition({ replay_user_message = "what did we decide last time" }) }))
  local path = nvim.fn.tempname() .. ".md"

  local written_path, export_error = TranscriptExport.export(api, "mock", "prior-acp-session", path)

  MiniTest.expect.equality(export_error, nil)
  MiniTest.expect.equality(written_path, path)
  MiniTest.expect.equality(api:list_sessions(), {})

  local content = read_file(path)
  MiniTest.expect.equality(content:find("agent: mock", 1, true) ~= nil, true)
  MiniTest.expect.equality(content:find("acp session: prior-acp-session", 1, true) ~= nil, true)
  MiniTest.expect.equality(content:find("## User", 1, true) ~= nil, true)
  MiniTest.expect.equality(content:find("what did we decide last time", 1, true) ~= nil, true)

  nvim.fn.delete(path)
  api:dispose()
end

T["export"]["exports replayed reasoning once, between the user prompt and the answer"] = function()
  local api = assert(Session.new({
    mock = mock_definition({
      replay_user_message = "what did we decide last time",
      replay_reasoning = "**planned** a careful answer",
    }),
  }))
  local path = nvim.fn.tempname() .. ".md"

  local written_path, export_error = TranscriptExport.export(api, "mock", "prior-acp-session", path)

  MiniTest.expect.equality(export_error, nil)
  MiniTest.expect.equality(written_path, path)

  local content = read_file(path)
  local occurrences = select(2, content:gsub("%*%*planned%*%* a careful answer", ""))
  MiniTest.expect.equality(occurrences, 1)
  MiniTest.expect.equality(select(2, content:gsub("## Reasoning", "")), 1)
  local user_at = assert(content:find("## User", 1, true))
  local reasoning_at = assert(content:find("## Reasoning", 1, true))
  MiniTest.expect.equality(user_at < reasoning_at, true)

  nvim.fn.delete(path)
  api:dispose()
end

T["run"] = MiniTest.new_set()

---@return table hooks
---@return table calls
local function fake_io()
  local calls = { out = {}, err = {}, quit = 0 }
  local hooks = {
    write_out = function(text)
      calls.out[#calls.out + 1] = text
    end,
    write_err = function(text)
      calls.err[#calls.err + 1] = text
    end,
    quit = function()
      calls.quit = calls.quit + 1
    end,
  }
  return hooks, calls
end

T["run"]["reports invalid agent configuration to stderr and force-quits"] = function()
  local hooks, calls = fake_io()
  -- Missing `command`: exercises rejection of a malformed definitions table.
  ---@diagnostic disable-next-line: missing-fields
  local invalid_definitions = { mock = {} }

  TranscriptExport.run(invalid_definitions, "mock", "any-session", nvim.fn.tempname() .. ".md", hooks)

  MiniTest.expect.equality(calls.out, {})
  MiniTest.expect.equality(#calls.err, 1)
  MiniTest.expect.equality(calls.err[1]:find("louiselm: ", 1, true), 1)
  MiniTest.expect.equality(calls.err[1]:find("invalid agent configuration", 1, true) ~= nil, true)
  MiniTest.expect.equality(calls.quit, 1)
end

T["run"]["reports an export error to stderr and force-quits without starting an agent"] = function()
  local hooks, calls = fake_io()

  TranscriptExport.run({ mock = mock_definition() }, "mock", "", nvim.fn.tempname() .. ".md", hooks)

  MiniTest.expect.equality(calls.out, {})
  MiniTest.expect.equality(calls.err, { "louiselm: ACP session id must be a non-empty string\n" })
  MiniTest.expect.equality(calls.quit, 1)
end

T["run"]["writes the exported path to stdout and never force-quits on success"] = function()
  local hooks, calls = fake_io()
  local path = nvim.fn.tempname() .. ".md"

  TranscriptExport.run({ mock = mock_definition() }, "mock", "prior-acp-session", path, hooks)

  MiniTest.expect.equality(calls.err, {})
  MiniTest.expect.equality(calls.quit, 0)
  MiniTest.expect.equality(calls.out, { path .. "\n" })
  MiniTest.expect.equality(nvim.fn.filereadable(path), 1)

  nvim.fn.delete(path)
end

return T
