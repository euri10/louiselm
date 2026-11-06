local MiniTest = require("mini.test")
local Decisions = require("louiselm.ui.chat.decisions")

---@diagnostic disable-next-line: undefined-global -- Neovim injects its runtime API.
local nvim = vim
local T = MiniTest.new_set()

local function key(buffer, lhs)
  for _, mapping in ipairs(nvim.api.nvim_buf_get_keymap(buffer, "n")) do
    if mapping.lhs == lhs then
      return mapping.callback
    end
  end
  error("missing decision key " .. lhs)
end

local function harness()
  local origin, window = nvim.api.nvim_get_current_buf(), nvim.api.nvim_get_current_win()
  local hosts, responses, errors, resolved = {}, {}, {}, {}
  local owner = Decisions.new({
    is_live = function(session)
      return hosts[session] ~= nil and nvim.api.nvim_buf_is_valid(hosts[session])
    end,
    is_visible = function(session)
      return hosts[session] == nvim.api.nvim_get_current_buf()
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
    if nvim.api.nvim_win_is_valid(window) then
      nvim.api.nvim_set_current_win(window)
      if nvim.api.nvim_buf_is_valid(origin) then
        nvim.api.nvim_win_set_buf(window, origin)
      end
    end
    for _, buffer in pairs(hosts) do
      if nvim.api.nvim_buf_is_valid(buffer) then
        nvim.api.nvim_buf_delete(buffer, { force = true })
      end
    end
  end)
  local function session(id, name)
    local value = {
      inspect = function()
        return { id = id, name = name or id, agent = "codex" }
      end,
    }
    hosts[value] = nvim.api.nvim_create_buf(false, true)
    return value
  end
  local function show(value)
    local suspension = owner:suspend()
    nvim.api.nvim_set_current_win(window)
    nvim.api.nvim_win_set_buf(window, hosts[value])
    owner:present_next(suspension)
  end
  local function request(value, id, options, extra)
    local data = {
      request_id = id,
      operation = { kind = "command", command = { "git", "status" } },
      options = options or {
        { optionId = "allow", kind = "allow_once", name = "Allow" },
        { optionId = "deny", kind = "reject_once", name = "Reject" },
      },
    }
    for field, entry in pairs(extra or {}) do
      data[field] = entry
    end
    owner:request(value, data, function(result)
      responses[#responses + 1] = { id = id, result = result }
      return true
    end)
  end
  return owner, session, show, request, responses, errors, resolved, hosts, window
end

local function choose(index)
  local buffer = nvim.api.nvim_get_current_buf()
  nvim.api.nvim_win_set_cursor(0, { index, 0 })
  key(buffer, "<CR>")()
end

T["interaction and Diff response failures are reported once by their decision owner"] = function()
  for _, kind in ipairs({ "interaction", "diff" }) do
    local owner, session, show, _, _, errors = harness()
    local notify = nvim.notify
    MiniTest.finally(function()
      rawset(nvim, "notify", notify)
    end)
    rawset(nvim, "notify", function(message)
      errors[#errors + 1] = message
    end)
    local first = session("failed-response")
    show(first)
    local function failed_response()
      return false, "private response payload"
    end
    if kind == "interaction" then
      owner:request_interaction(first, {
        request_id = "failed",
        session_id = "failed-response",
        tool_call_id = "failed-tool",
        kind = "plan_review",
      }, failed_response)
      choose(3)
    else
      local path = nvim.fn.tempname()
      nvim.fn.writefile({ "before" }, path)
      MiniTest.finally(function()
        nvim.fn.delete(path)
      end)
      owner:request(first, {
        request_id = "failed",
        operation = { kind = "file_edit", path = path },
        toolCall = { rawInput = { content = "after\n" } },
        options = { "allow", "deny" },
      }, failed_response)
      key(nvim.api.nvim_get_current_buf(), "a")()
    end
    MiniTest.expect.equality(errors, { "decision response failed" })
    MiniTest.expect.equality(owner:is_active(), false)
  end
end

T["background requests wait without blocking the visible Session"] = function()
  local owner, session, show, request, responses, _, _, hosts = harness()
  local first, second = session("session-1"), session("session-2")
  show(second)
  local windows = #nvim.api.nvim_list_wins()
  request(first, "hidden")
  MiniTest.expect.equality(#nvim.api.nvim_list_wins(), windows)
  MiniTest.expect.equality(nvim.api.nvim_get_current_buf(), hosts[second])
  MiniTest.expect.equality(owner:is_active(), false)
  MiniTest.expect.equality(responses, {})

  request(second, "visible")
  MiniTest.expect.equality(owner:active_session(), second)
  choose(1)
  MiniTest.expect.equality(
    responses,
    { { id = "visible", result = { outcome = { outcome = "selected", optionId = "deny" } } } }
  )
  MiniTest.expect.equality(owner:is_active(), false)
  MiniTest.expect.equality(nvim.api.nvim_get_current_buf(), hosts[second])
  show(first)
  MiniTest.expect.equality(owner:active_session(), first)
  choose(2)
  MiniTest.expect.equality(
    responses[2],
    { id = "hidden", result = { outcome = { outcome = "selected", optionId = "allow" } } }
  )
end

T["switching hides without settling and invalidates old presentation callbacks"] = function()
  local owner, session, show, request, responses, _, resolved = harness()
  local first, second = session("session-1"), session("session-2")
  show(first)
  request(first, "pending")
  local buffer, window = nvim.api.nvim_get_current_buf(), nvim.api.nvim_get_current_win()
  nvim.api.nvim_win_set_cursor(window, { 2, 0 })
  local stale = key(buffer, "<CR>")
  show(second)
  MiniTest.expect.equality(nvim.api.nvim_win_is_valid(window), false)
  MiniTest.expect.equality(nvim.api.nvim_buf_is_valid(buffer), true)
  MiniTest.expect.equality(owner:active_session(), nil)
  stale()
  MiniTest.expect.equality(responses, {})
  show(first)
  MiniTest.expect.equality(nvim.api.nvim_get_current_buf(), buffer)
  MiniTest.expect.equality(nvim.api.nvim_win_get_cursor(0)[1], 2)
  stale()
  MiniTest.expect.equality(responses, {})
  key(buffer, "<CR>")()
  stale()
  MiniTest.expect.equality(
    responses,
    { { id = "pending", result = { outcome = { outcome = "selected", optionId = "allow" } } } }
  )
  MiniTest.expect.equality(resolved, { "pending" })
end

T["ordinary window navigation suspends and restores the owning Session"] = function()
  local owner, session, show, request, responses, _, _, hosts, window = harness()
  local first, second = session("session-1"), session("session-2")
  show(first)
  request(first, "pending")
  local picker_window = nvim.api.nvim_get_current_win()
  nvim.api.nvim_set_current_win(window)
  nvim.api.nvim_win_set_buf(window, hosts[second])
  assert(nvim.wait(1000, function()
    return not nvim.api.nvim_win_is_valid(picker_window)
  end))
  MiniTest.expect.equality(responses, {})
  MiniTest.expect.equality(owner:active_session(), nil)
  nvim.api.nvim_win_set_buf(window, hosts[first])
  assert(nvim.wait(1000, function()
    return owner:active_session() == first and nvim.api.nvim_get_current_win() ~= window
  end))
  choose(1)
  MiniTest.expect.equality(#responses, 1)
end

T["details retain requester identity and pending state through suspension"] = function()
  local owner, session, show, request, responses = harness()
  local first, second = session("session-7", "Build QA"), session("session-2")
  show(first)
  request(first, "details", nil, { toolCall = { title = "Run build" } })
  MiniTest.expect.equality(nvim.api.nvim_win_get_config(0).title[1][1]:find("[codex session-7] Build QA", 1, true), 1)
  choose(3)
  local details = nvim.api.nvim_get_current_buf()
  MiniTest.expect.equality(
    nvim.api.nvim_win_get_config(0).title[1][1],
    "[codex session-7] Build QA · Request details · q/Esc: back to choices"
  )
  show(second)
  MiniTest.expect.equality(responses, {})
  show(first)
  MiniTest.expect.equality(nvim.api.nvim_get_current_buf(), details)
  key(details, "q")()
  choose(1)
  MiniTest.expect.equality(#responses, 1)
  MiniTest.expect.equality(owner:is_active(), false)
end

T["Session cancellation preserves queued order and ignores retired mappings"] = function()
  local owner, session, show, request, responses = harness()
  local first = session("session-1")
  show(first)
  request(first, "cancelled")
  request(first, "survivor")
  local stale = key(nvim.api.nvim_get_current_buf(), "<CR>")
  owner:cancel(first, { "cancelled" })
  stale()
  MiniTest.expect.equality(responses, {})
  MiniTest.expect.equality(owner:active_session(), first)
  choose(2)
  MiniTest.expect.equality(
    responses,
    { { id = "survivor", result = { outcome = { outcome = "selected", optionId = "allow" } } } }
  )
end

T["retirement and disposal cancel visible and hidden requests exactly once"] = function()
  local owner, session, show, request, responses = harness()
  local first, second = session("session-1"), session("session-2")
  show(first)
  request(first, "active")
  request(first, "queued")
  request(second, "other")
  local stale = key(nvim.api.nvim_get_current_buf(), "<CR>")
  owner:retire(first)
  stale()
  MiniTest.expect.equality(#responses, 2)
  show(second)
  owner:dispose()
  owner:dispose()
  request(second, "late")
  MiniTest.expect.equality(#responses, 4)
  for _, response in ipairs(responses) do
    MiniTest.expect.equality(response.result, { outcome = { outcome = "cancelled" } })
  end
end

T["rejection stays first and same-kind choices retain their exact identifiers"] = function()
  local _, session, show, request, responses = harness()
  local first = session("session-1")
  show(first)
  request(first, "options", {
    { optionId = "allow", kind = "allow_once", name = "Allow Once" },
    { optionId = "session", kind = "allow_always", name = "Allow for Session" },
    { optionId = "prefix", kind = "allow_always", name = "Allow Commands Starting With git" },
    { optionId = "deny", kind = "reject_once", name = "Reject" },
  })
  MiniTest.expect.equality(nvim.api.nvim_buf_get_lines(0, 0, -1, false), {
    "Reject",
    "Allow Once",
    "Allow for Session",
    "Allow Commands Starting With git",
  })
  choose(4)
  MiniTest.expect.equality(responses[1].result, { outcome = { outcome = "selected", optionId = "prefix" } })
end

T["invalid requests cancel without UI and failed responders are reported"] = function()
  local owner, session, show, request, responses, errors = harness()
  local first = session("session-1")
  show(first)
  request(first, "empty", {})
  MiniTest.expect.equality(responses[1].result, { outcome = { outcome = "cancelled" } })
  owner:request(first, nil)
  MiniTest.expect.equality(errors[1], "permission request has no response callback")
  owner:request(first, { options = { { optionId = "deny", kind = "reject_once" } } }, function()
    return false, "transport closed"
  end)
  choose(1)
  MiniTest.expect.equality(errors[#errors], "decision response failed")
  MiniTest.expect.equality(owner:is_active(), false)
end

T["fast cancellation revokes visible and queued requests before scheduled cleanup"] = function()
  for _, presented in ipairs({ true, false }) do
    local owner, session, show, request, responses, errors, _, hosts = harness()
    local first = session("cancel-in-fast-event")
    show(first)
    if presented then
      request(first, "cancelled")
    end
    local opened = 0
    local observer = nvim.api.nvim_create_autocmd("WinNew", {
      callback = function()
        opened = opened + 1
      end,
    })
    local timer = assert(nvim.uv.new_timer())
    MiniTest.finally(function()
      nvim.api.nvim_del_autocmd(observer)
      if not timer:is_closing() then
        timer:close()
      end
    end)
    local completed, call_ok, call_error, fast = false, nil, nil, false
    timer:start(0, 0, function()
      fast = nvim.in_fast_event()
      call_ok, call_error = pcall(function()
        if not presented then
          request(first, "cancelled")
        end
        owner:cancel(first, { "cancelled" })
      end)
      completed = true
    end)
    assert(nvim.wait(1000, function()
      return completed and nvim.api.nvim_get_current_buf() == hosts[first]
    end, 1))
    MiniTest.expect.equality({ fast, call_ok, call_error }, { true, true })
    MiniTest.expect.equality(owner:is_active(), false)
    MiniTest.expect.equality({ responses, errors, opened }, { {}, {}, 0 })
    owner:dispose()
  end
end

return T
