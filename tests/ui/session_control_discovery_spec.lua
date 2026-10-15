local MiniTest = require("mini.test")
local Control = require("louiselm.ui.session_control")
local T = MiniTest.new_set()
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim

T["discovery lists configured names without initializing a Session or UI"] = function()
  local plugin = require("louiselm")
  local Command = require("louiselm.ui.chat.command")
  MiniTest.finally(function()
    Command.register()
    Command.configure(nil)
  end)
  assert(plugin.setup({ agents = { offline = { provider = "test", command = "must-not-run" } } }))
  local buffers, windows = nvim.api.nvim_list_bufs(), nvim.api.nvim_list_wins()
  local found = assert(plugin.session_control({ action = "discover" }))
  MiniTest.expect.equality(found.agents, { "offline" })
  MiniTest.expect.equality(found.sessions, {})
  MiniTest.expect.equality(found.availability, "unavailable")
  MiniTest.expect.equality(nvim.api.nvim_list_bufs(), buffers)
  MiniTest.expect.equality(nvim.api.nvim_list_wins(), windows)
  MiniTest.expect.equality(require("louiselm.session").exit_verdict(), {})
end

T["discovery reads fresh advertisements from an explicitly selected Session"] = function()
  local state = {
    id = "known",
    agent = "mock",
    status = "ready",
    acp_session_id = "acp-known",
    config_options = {
      {
        id = "model",
        name = "Model",
        type = "select",
        category = "model",
        current_value = "small",
        options = { { value = "small", name = "Small" }, { value = "large", name = "Large" } },
      },
      {
        id = "effort",
        name = "Effort",
        type = "select",
        current_value = "low",
        options = { { value = "low", name = "Low" } },
      },
    },
  }
  local api = {
    list_sessions = function()
      return { "known" }
    end,
    get_session = function(_, id)
      if id == "known" then
        return {
          inspect = function()
            return nvim.deepcopy(state)
          end,
        }
      end
    end,
  }
  local request = { action = "discover", agent = "mock", session_id = "known" }
  local first = assert(Control.discover(api, { mock = {} }, request))
  MiniTest.expect.equality(first.options, state.config_options)
  first.options[1].current_value = "caller edit"
  state.config_options[1].current_value = "large"
  state.config_options[2].options[2] = { value = "max", name = "Max" }
  request.option_id = "effort"
  local changed = assert(Control.discover(api, { mock = {} }, request))
  MiniTest.expect.equality(changed.options, { state.config_options[2] })
  MiniTest.expect.equality(changed.sessions[1].options.model, "large")
  request.session_id = "missing"
  local value, err = Control.discover(api, { mock = {} }, request)
  MiniTest.expect.equality(value, nil)
  assert(assert(err):find("Session", 1, true))
  request.session_id, request.agent = "known", "other"
  value, err = Control.discover(api, { mock = {}, other = {} }, request)
  MiniTest.expect.equality(value, nil)
  assert(assert(err):find("Agent", 1, true))
end

T["discovery validates selectors and explicitly reports unavailable advertisements"] = function()
  for _, request in ipairs({
    { action = "discover", prompt = "not permitted" },
    { action = "discover", session_id = "s" },
    { action = "discover", agent = "mock", option_id = "model" },
    { action = "discover", agent = false },
  }) do
    MiniTest.expect.equality(type(Control.validate(request)), "string")
  end
  local found = assert(Control.discover(nil, { mock = {} }, { action = "discover", agent = "mock" }))
  MiniTest.expect.equality(found.availability, "unavailable")
  MiniTest.expect.equality(found.options, nil)
end

return T
