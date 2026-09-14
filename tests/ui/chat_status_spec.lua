local MiniTest = require("mini.test")
local Status = require("louiselm.ui.chat.status")

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local T = MiniTest.new_set()

local function snapshot(id, agent)
  return { id = id, name = id, agent = agent, status = "ready", current_turn = 0, config_options = {} }
end

T["autonomous processing is active even after the prompt completed"] = function()
  local state = snapshot("session-1", "copilot")
  state.status = "running"
  local bar = Status.session_winbar(state)
  MiniTest.expect.equality(bar:find("LouiselmStatusActive", 1, true) ~= nil, true)
  MiniTest.expect.equality(nvim.api.nvim_eval_statusline(bar, { maxwidth = 200 }).str, "Model responding · copilot")
end

T["compact options count hidden controls and restore after resize"] = function()
  local state = snapshot("session-1", "codex")
  state.name = "Review"
  state.config_options = {
    { id = "m", name = "Model", category = "model", type = "select", current_value = "GPT-6" },
    { id = "e", name = "Effort", category = "thought_level", type = "select", current_value = "high" },
    { id = "fast", name = "Fast", type = "boolean", current_value = false },
  }
  local function rendered(width)
    local bar = Status.session_winbar(state, nil, width)
    return nvim.api.nvim_eval_statusline(bar, { use_winbar = true, maxwidth = 200 }).str
  end
  MiniTest.expect.equality(rendered(200), "Your turn · Review · codex · GPT-6 e=high +1")
  MiniTest.expect.equality(rendered(37), "Your turn · Review · codex · GPT-6 +2")
  MiniTest.expect.equality(rendered(35), "Your turn · Review · codex · opts")
  state.source = "loaded"
  MiniTest.expect.equality(rendered(200), "Your turn · Review · codex · GPT-6 e=high +1")
end

T["options and limits targets never collide with background Sessions"] = function()
  local state = snapshot("current", "codex")
  state.config_options = { { id = "fast", name = "Fast", type = "boolean", current_value = false } }
  local backgrounds = {}
  for index = 1, 100 do
    backgrounds[index] = { state = snapshot("background-" .. index, "codex"), unread_turn = false }
  end
  local base, agent, visible = Status.session_winbar(state, { text = "limits", group = "Normal" }, 200)
  local _, targets = Status.layout_winbar(base, agent, backgrounds, 10000, visible and state.id or nil)
  MiniTest.expect.equality(targets[98], { session = "current" })
  MiniTest.expect.equality(targets[99], { agent = "codex" })
  MiniTest.expect.equality(targets[100], "background-98")
  MiniTest.expect.equality(targets[102], "background-100")
end

T["whole-line budget preserves urgent attention before optional details"] = function()
  local state = snapshot("current", "codex")
  state.name = "Review"
  state.config_options = {
    { id = "model", name = "Model", category = "model", type = "select", current_value = "界 100% long-model" },
  }
  local urgent = snapshot("urgent", "claude")
  urgent.status = "waiting_permission"
  local backgrounds = { { state = urgent, unread_turn = false } }
  local width = 30
  local base, agent, visible = Status.session_winbar(state, nil, width - Status.background_width(backgrounds))
  local used = nvim.api.nvim_eval_statusline(base, { maxwidth = 200 }).width
  local bar, targets = Status.layout_winbar(base, agent, backgrounds, width - used, visible and state.id or nil)
  local rendered = nvim.api.nvim_eval_statusline(bar, { use_winbar = true, maxwidth = width }).str
  MiniTest.expect.equality(rendered:find("Your turn", 1, true) ~= nil, true)
  MiniTest.expect.equality(rendered:find("! urgent", 1, true) ~= nil, true)
  MiniTest.expect.equality(targets[98], { session = "current" })
  local wide = Status.session_winbar(state, nil, 200)
  MiniTest.expect.equality(
    nvim.api.nvim_eval_statusline(wide, { maxwidth = 200 }).str,
    "Your turn · Review · codex · 界 100% long-model"
  )
end

T["renders header highlights and escaped winbars without changing snapshots"] = function()
  local state = snapshot("session-1", "codex")
  state.name = "界\n100%"
  state.acp_session_id = "acp-1"
  state.config_options = {
    {
      id = "model",
      name = "Model",
      type = "select",
      current_value = "astra",
      options = { { value = "astra", name = "GPT-6" } },
    },
    { id = "fast", name = "Fast", type = "boolean", current_value = false },
  }
  state.context = { used = 25, size = 100, percentage = 25, stale = true }
  state.cost = { amount = 0.5, currency = "USD" }
  local before = nvim.deepcopy(state)
  local lines, highlights = Status.session_header(state)
  MiniTest.expect.equality(lines, {
    "# codex/acp-1 · 界 100% · session-1",
    "Session: status=ready · display=Your turn",
    "ACP options: Model=GPT-6 · Fast=false",
    "Telemetry: context=25/100 (25% stale) · cost=0.5 USD",
  })
  local values = {}
  for _, highlight in ipairs(highlights) do
    values[#values + 1] = {
      lines[highlight.line + 1]:sub(highlight.start_col + 1, highlight.end_col),
      highlight.group,
    }
  end
  MiniTest.expect.equality(values, {
    { "display=Your turn", "LouiselmStatusReady" },
    { "Model=GPT-6", "LouiselmAcpValue" },
    { "Fast=false", "LouiselmAcpValue" },
    { "context=25/100", "LouiselmAcpValue" },
    { "25% stale", "LouiselmDerivedValue" },
    { "cost=0.5 USD", "LouiselmAcpValue" },
  })
  local base, agent = Status.session_winbar(state)
  MiniTest.expect.equality(agent, nil)
  MiniTest.expect.equality(
    nvim.api.nvim_eval_statusline(base, { use_winbar = true, maxwidth = 200 }).str,
    "Your turn · 界 100% · codex · opts · ctx 25% stale · cost=0.5 USD"
  )
  MiniTest.expect.equality(state, before)
end

T["returns independent width-specific click targets and keeps urgent entries pinned"] = function()
  local current = snapshot("current", "codex")
  local backgrounds = {
    { state = snapshot("界界", "codex"), unread_turn = false },
    { state = snapshot("unseen", "claude"), unread_turn = true },
    { state = snapshot("urgent", "claude"), unread_turn = false },
  }
  backgrounds[3].state.status = "waiting_permission"
  local before = nvim.deepcopy(backgrounds)
  local limits = { text = "limits 20%/5h", group = "LouiselmStatusWarning" }
  local base, agent = Status.session_winbar(current, limits)
  local wide, wide_targets = Status.layout_winbar(base, agent, backgrounds, 100)
  local narrow, narrow_targets = Status.layout_winbar(base, agent, backgrounds, 0)
  MiniTest.expect.equality(wide_targets, { "界界", "unseen", "urgent", [99] = { agent = "codex" } })
  MiniTest.expect.equality(narrow_targets, { false, false, "urgent", [99] = { agent = "codex" } })
  MiniTest.expect.equality(wide:find("● 界界", 1, true) ~= nil, true)
  MiniTest.expect.equality(narrow:find("%#LouiselmStatusReady#+1●", 1, true) ~= nil, true)
  MiniTest.expect.equality(narrow:find("%#LouiselmStatusWarning#+1●", 1, true) ~= nil, true)
  MiniTest.expect.equality(
    narrow:find("%3@v:lua.__louiselm_winbar_click@%#LouiselmStatusWarning#! urgent", 1, true) ~= nil,
    true
  )
  MiniTest.expect.equality(narrow:find("limits 20%%/5h", 1, true) ~= nil, true)
  local plain, empty_targets = Status.layout_winbar(base, nil, {}, 0)
  MiniTest.expect.equality(plain, base)
  MiniTest.expect.equality(empty_targets, {})
  MiniTest.expect.equality(backgrounds, before)
  MiniTest.expect.equality(limits, { text = "limits 20%/5h", group = "LouiselmStatusWarning" })
end

T["measures Unicode entries in display columns at the exact overflow boundary"] = function()
  local backgrounds = { { state = snapshot("界界", "codex"), unread_turn = false } }
  local _, fits = Status.layout_winbar("", nil, backgrounds, 6)
  local _, overflow = Status.layout_winbar("", nil, backgrounds, 5)
  MiniTest.expect.equality(fits, { "界界" })
  MiniTest.expect.equality(overflow, { false })
end

T["aligns corresponding options advertised with different Agent ids"] = function()
  local claude = snapshot("session-1", "claude")
  local codex = snapshot("session-2", "codex")
  -- Observed configOptions ids/names/categories, 2026-09-09; display values
  -- simplified below. This fixture describes no event ordering.
  -- ~/.local/state/acp-llm-adapter/proxy/sessions/<ACP-id>/log.jsonl
  -- claude/3282c3df-a569-4d46-9b35-358157805c69
  -- codex/01a083f4-469e-7351-97fb-205f6aa39a6f
  claude.config_options = {
    { id = "effort", name = "Effort", category = "thought_level", type = "select", current_value = "Xhigh" },
    { id = "fast", name = "Fast mode", category = "model_config", type = "select", current_value = "Off" },
  }
  codex.config_options = {
    {
      id = "collaboration_mode",
      name = "Collaboration mode",
      category = "collaboration_mode",
      type = "select",
      current_value = "Default",
    },
    {
      id = "reasoning_effort",
      name = "Reasoning effort",
      category = "thought_level",
      type = "select",
      current_value = "Xhigh",
    },
    { id = "fast-mode", name = "Fast mode", category = "model_config", type = "select", current_value = "Off" },
    -- Synthetic guard: model_config is a group, not one interchangeable control.
    { id = "temperature", name = "Temperature", category = "model_config", type = "select", current_value = "Low" },
  }
  local original_options = nvim.deepcopy({ claude.config_options, codex.config_options })
  local rows = Status.session_labels({ claude, codex })
  local function cells(row)
    return nvim.tbl_map(nvim.trim, nvim.split(row, " · ", { plain = true }))
  end
  MiniTest.expect.equality(cells(rows[1]), {
    "session-1",
    "ready",
    "claude",
    "Effort=Xhigh",
    "Fast mode=Off",
    "",
    "",
  })
  MiniTest.expect.equality(cells(rows[2]), {
    "session-2",
    "ready",
    "codex",
    "Reasoning effort=Xhigh",
    "Fast mode=Off",
    "Collaboration mode=Default",
    "Temperature=Low",
  })
  local first_fast = assert(rows[1]:find("Fast mode=Off", 1, true))
  local second_fast = assert(rows[2]:find("Fast mode=Off", 1, true))
  MiniTest.expect.equality(
    nvim.fn.strdisplaywidth(rows[1]:sub(1, first_fast - 1)),
    nvim.fn.strdisplaywidth(rows[2]:sub(1, second_fast - 1))
  )
  MiniTest.expect.equality({ claude.config_options, codex.config_options }, original_options)

  -- Synthetic ambiguity: two thought-level controls must not overwrite one another.
  codex.config_options[#codex.config_options + 1] = {
    id = "budget",
    name = "Thinking budget",
    category = "thought_level",
    type = "select",
    current_value = "Large",
  }
  rows = Status.session_labels({ claude, codex })
  MiniTest.expect.equality(rows[2]:find("Reasoning effort=Xhigh", 1, true) ~= nil, true)
  MiniTest.expect.equality(rows[2]:find("Thinking budget=Large", 1, true) ~= nil, true)
  MiniTest.expect.equality(rows[1]:find("Effort=Xhigh", 1, true) ~= nil, true)
end

return T
