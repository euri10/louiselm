local MiniTest = require("mini.test")
local Decisions = require("louiselm.ui.chat.decisions")

---@diagnostic disable-next-line: undefined-global -- Neovim injects its runtime API.
local nvim = vim

local T = MiniTest.new_set()

local function harness()
  local original_select = nvim.ui.select
  local picks, responses, errors, resolved = {}, {}, {}, {}
  local owner = Decisions.new({
    is_live = function()
      return true
    end,
    report_error = function(_, message)
      errors[#errors + 1] = message
    end,
    resolved = function(_, id)
      resolved[#resolved + 1] = id
    end,
  })
  MiniTest.finally(function()
    owner:dispose()
    nvim.ui.select = original_select
  end)
  nvim.ui.select = function(items, options, callback)
    picks[#picks + 1] = { items = items, options = options, callback = callback }
  end
  local function request(session, id)
    owner:request(session, {
      request_id = id,
      operation = { kind = "command", command = { "git", "status" } },
      options = {
        { optionId = "allow", kind = "allow_once" },
        { optionId = "deny", kind = "reject_once" },
      },
    }, function(result)
      responses[#responses + 1] = { id = id, result = result }
      return true
    end)
  end
  return owner, request, picks, responses, errors, resolved
end

T["late callbacks cannot answer twice or release another Session's picker"] = function()
  local owner, request, picks, responses, _, resolved = harness()
  -- The owner treats Sessions as opaque identities; host callbacks own view lookup.
  local first, second = {}, {}
  request(first, "cancelled")
  request(second, "survivor")
  owner:cancel(first, { "cancelled" })
  MiniTest.expect.equality(owner:is_active(), false)
  MiniTest.expect.equality(#picks, 1)
  MiniTest.expect.equality(responses, {})

  picks[1].callback(picks[1].items[2])
  MiniTest.expect.equality(#picks, 2)
  MiniTest.expect.equality(owner:is_active(), true)
  request(first, "next")
  picks[1].callback(picks[1].items[2])
  MiniTest.expect.equality(#picks, 2)
  MiniTest.expect.equality(responses, {})

  picks[2].callback(picks[2].items[2])
  MiniTest.expect.equality(#picks, 3)
  picks[2].callback(nil)
  MiniTest.expect.equality(owner:is_active(), true)
  picks[3].callback(nil)
  MiniTest.expect.equality(owner:is_active(), false)
  MiniTest.expect.equality(responses, {
    { id = "survivor", result = { outcome = { outcome = "selected", optionId = "allow" } } },
    { id = "next", result = { outcome = { outcome = "cancelled" } } },
  })
  MiniTest.expect.equality(resolved, { "survivor", "next" })
end

T["picker exceptions cancel once and let later requests open"] = function()
  local owner, request, picks, responses, errors = harness()
  local select = nvim.ui.select
  nvim.ui.select = function()
    error("selector failed")
  end
  request({}, "broken")
  MiniTest.expect.equality(owner:is_active(), false)
  MiniTest.expect.equality(#errors, 1)
  MiniTest.expect.equality(errors[1]:find("permission picker failed:", 1, true) ~= nil, true)
  MiniTest.expect.equality(responses, { { id = "broken", result = { outcome = { outcome = "cancelled" } } } })
  nvim.ui.select = select
  request({}, "next")
  MiniTest.expect.equality(#picks, 1)
  picks[1].callback(picks[1].items[2])
  MiniTest.expect.equality(#responses, 2)
end

T["disposal cancels queued requests and late choices at most once"] = function()
  local owner, request, picks, responses = harness()
  request({}, "active")
  request({}, "queued")
  owner:dispose()
  owner:dispose()
  MiniTest.expect.equality(responses, { { id = "queued", result = { outcome = { outcome = "cancelled" } } } })
  picks[1].callback(picks[1].items[2])
  picks[1].callback(picks[1].items[2])
  request({}, "late")
  MiniTest.expect.equality(responses, {
    { id = "queued", result = { outcome = { outcome = "cancelled" } } },
    { id = "active", result = { outcome = { outcome = "cancelled" } } },
    { id = "late", result = { outcome = { outcome = "cancelled" } } },
  })
  MiniTest.expect.equality(#picks, 1)
end

T["puts rejection options first so permission pickers fail closed"] = function()
  local owner = harness()
  local session = {} -- Only identity is consumed; no Session methods are called.
  local labels
  local option_ids
  local prompt
  local response
  nvim.ui.select = function(options, select_options, callback)
    prompt = select_options.prompt
    labels = {}
    option_ids = {}
    for _, option in ipairs(options) do
      labels[#labels + 1] = select_options.format_item(option)
      option_ids[#option_ids + 1] = option.optionId
    end
    callback(options[1], 1)
  end
  owner:request(session, {
    operation = { kind = "command", command = { "git", "commit" } },
    options = {
      { optionId = "allow_once", name = "Allow Once", kind = "allow_once" },
      { optionId = "allow_always", name = "Allow for Session", kind = "allow_always" },
      { optionId = "allow_prefix", name = "Allow Commands Starting With git", kind = "allow_always" },
      { optionId = "reject_once", name = "Reject", kind = "reject_once" },
    },
  }, function(result)
    response = result
    return true
  end)

  MiniTest.expect.equality(labels, { "Reject", "Allow Once", "Allow for Session", "Allow Commands Starting With git" })
  MiniTest.expect.equality(option_ids, { "reject_once", "allow_once", "allow_always", "allow_prefix" })
  MiniTest.expect.equality(prompt, 'louiselm permission (command): ["git","commit"] ')
  MiniTest.expect.equality(response, { outcome = { outcome = "selected", optionId = "reject_once" } })
end

return T
