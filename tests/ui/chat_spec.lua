local MiniTest = require("mini.test")
local Chat = require("louiselm.ui.chat")

local T = MiniTest.new_set()

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local function fake_session(id, agent)
  local listeners = {}
  local session = {
    state = { id = id, agent = agent, status = "ready", current_turn = 0, config_options = {} },
    prompts = {},
    prompt_error = nil,
    config_changes = {},
    disposed = false,
  }

  function session:on(callback)
    listeners[#listeners + 1] = callback
    return function()
      for index, listener in ipairs(listeners) do
        if listener == callback then
          table.remove(listeners, index)
          return
        end
      end
    end
  end

  function session:inspect()
    return self.state
  end

  function session:prompt(prompt)
    if self.prompt_error ~= nil then
      return nil, self.prompt_error
    end
    self.prompts[#self.prompts + 1] = prompt
    return #self.prompts
  end

  function session:set_config_option(option_id, value, callback)
    self.config_changes[#self.config_changes + 1] = { id = option_id, value = value }
    if callback ~= nil then
      callback(self.state.config_options)
    end
    return #self.config_changes
  end

  function session:cancel()
    self.state.status = "cancelling"
    return true
  end

  function session:dispose()
    self.disposed = true
    self.state.status = "disposed"
    return true
  end

  function session:emit(event)
    for _, listener in ipairs(listeners) do
      listener(event)
    end
  end

  return session
end

local function fake_api()
  return {
    registry = {},
    create_session = function()
      return nil, "not implemented in this test"
    end,
    get_session = function()
      return nil
    end,
    list_sessions = function()
      return {}
    end,
    dispose = function()
      return true
    end,
  }
end

local function buffer_lines(buffer)
  return nvim.api.nvim_buf_get_lines(buffer, 0, -1, false)
end

---@param line integer One-based line number in the current window.
---@return integer first
---@return integer last
local function fold_range(line)
  return nvim.fn.foldclosed(line), nvim.fn.foldclosedend(line)
end

local function chat_lines(identity, session, body, options, telemetry)
  local lines = {
    "# " .. identity,
    "Session: " .. session,
    "ACP options:" .. (options and " " .. options or ""),
    "Telemetry:" .. (telemetry and " " .. telemetry or ""),
  }
  nvim.list_extend(lines, body)
  return lines
end

local function virtual_text(buffer)
  local marks = nvim.api.nvim_buf_get_extmarks(buffer, -1, 0, -1, { details = true })
  local text = {}
  for _, mark in ipairs(marks) do
    for _, chunk in ipairs(mark[4].virt_text or {}) do
      text[#text + 1] = chunk[1]
    end
  end
  return text
end

local function header_highlights(buffer)
  local namespace = nvim.api.nvim_get_namespaces()["louiselm.chat.header"]
  local lines = buffer_lines(buffer)
  local highlights = {}
  for _, mark in ipairs(nvim.api.nvim_buf_get_extmarks(buffer, namespace, 0, -1, { details = true })) do
    local details = mark[4]
    if details.hl_group ~= nil then
      highlights[#highlights + 1] = {
        text = lines[mark[2] + 1]:sub(mark[3] + 1, details.end_col),
        group = details.hl_group,
      }
    end
  end
  return highlights
end

local original_schedule = nvim.schedule
local original_select = nvim.ui.select

T["chat"] = MiniTest.new_set({
  hooks = {
    post_case = function()
      -- A failing expectation skips a test's own restore, and MiniTest itself needs these.
      nvim.schedule = original_schedule
      nvim.ui.select = original_select
      nvim.cmd.normal({ args = { "<Esc>" }, bang = true })
      for _, buffer in ipairs(nvim.api.nvim_list_bufs()) do
        if nvim.api.nvim_buf_is_valid(buffer) and nvim.api.nvim_buf_get_name(buffer):match("^louiselm://") then
          nvim.api.nvim_buf_delete(buffer, { force = true })
        end
      end
    end,
  },
})

T["chat"]["focuses the prompt"] = function()
  local first = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))

  MiniTest.expect.equality(nvim.api.nvim_win_get_cursor(0), { 6, 1 })

  chat:dispose()
end

T["chat"]["lists and confirms revocation of remembered permission rules"] = function()
  local api = fake_api()
  local revoked
  api.list_permissions = function()
    return {
      {
        id = "rule-1",
        decision = "allow",
        lifetime = "always",
        agent = "codex",
        adapter = { command = "codex-acp", args = {} },
        workspace = "/workspace",
        kind = "command",
        command = { "git", "status" },
      },
    }
  end
  api.revoke_permission = function(_, id)
    revoked = id
    return true
  end
  local chat = assert(Chat.new(api))
  local original_select = nvim.ui.select
  local original_notify = nvim.notify
  local prompts = {}
  local labels = {}
  local notification
  rawset(nvim, "notify", function(message, level)
    notification = { message = message, level = level }
  end)
  nvim.ui.select = function(items, options, callback)
    prompts[#prompts + 1] = options.prompt
    labels[#labels + 1] = options.format_item and options.format_item(items[1]) or items[1]
    callback(#prompts == 1 and items[1] or "Revoke")
  end

  assert(chat:manage_permissions())

  nvim.ui.select = original_select
  rawset(nvim, "notify", original_notify)
  MiniTest.expect.equality(prompts, { "louiselm remembered permissions: ", "revoke rule-1? " })
  MiniTest.expect.equality(labels[1], 'allow always · codex · /workspace · command ["git","status"]')
  MiniTest.expect.equality(revoked, "rule-1")
  MiniTest.expect.equality(
    notification,
    { message = "louiselm: revoked permission rule-1", level = nvim.log.levels.INFO }
  )
  chat:dispose()
end

T["chat"]["submits every line in a multiline prompt"] = function()
  local first = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))

  nvim.api.nvim_buf_set_lines(chat:buffer(), 5, -1, false, { "> first line", "> second line" })
  assert(chat:submit())

  MiniTest.expect.equality(first.prompts, { "first line\nsecond line" })
  MiniTest.expect.equality(
    buffer_lines(chat:buffer()),
    chat_lines("claude · session-1", "status=ready · display=Your turn", {
      "",
      "> first line",
      "> second line",
      "",
      "> ",
    })
  )
  chat:dispose()
end

T["chat"]["renders session events and forwards slash prompts"] = function()
  local first = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))

  chat:submit("/compact")
  first:emit({
    type = "chunk",
    session_id = "session-1",
    data = { content = { type = "text", text = "hello **" } },
  })
  first:emit({
    type = "chunk",
    session_id = "session-1",
    data = { content = { type = "text", text = "world**" } },
  })
  first:emit({
    type = "tool_call_started",
    session_id = "session-1",
    data = { toolCallId = "tool-1", title = "Read file" },
  })
  first:emit({
    type = "tool_call_finished",
    session_id = "session-1",
    data = { toolCallId = "tool-1", status = "completed" },
  })
  nvim.wait(100, function()
    return #buffer_lines(chat:buffer()) == 9
  end, 1)

  MiniTest.expect.equality(first.prompts, { "/compact" })
  MiniTest.expect.equality(
    buffer_lines(chat:buffer()),
    chat_lines("claude · session-1", "status=ready · display=Your turn", {
      "",
      "> /compact",
      "",
      "hello **world**",
      "[tool] tool-1: Read file (completed)",
      "> ",
    })
  )

  chat:dispose()
end

T["chat"]["shows skill status and keeps slash prompts when skills are off"] = function()
  local first = fake_session("session-1", "claude")
  first.state.skills_policy = "off"
  local chat = assert(Chat.new(fake_api(), {
    skills = {
      {
        name = "grill-me",
        description = "Stress test",
        path = "/skills/grill-me/SKILL.md",
        content = "skill",
        explicit_only = false,
      },
    },
  }))
  assert(chat:attach(first))

  assert(chat:submit("/compact"))
  local picked, pick_error = chat:pick_skill()

  MiniTest.expect.equality(first.prompts, { "/compact" })
  MiniTest.expect.equality(buffer_lines(chat:buffer())[2], "Session: status=ready · display=Your turn · skills=off")
  MiniTest.expect.equality(picked, false)
  MiniTest.expect.equality(pick_error, "skill picker is disabled for this session")
  chat:dispose()
end

T["chat"]["reports an empty picker catalog without blocking a native session"] = function()
  local first = fake_session("session-1", "codex")
  first.state.skills_policy = "native"
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))

  local picked, pick_error = chat:pick_skill()

  MiniTest.expect.equality(picked, false)
  MiniTest.expect.equality(pick_error, "no chat skills configured")
  MiniTest.expect.equality(
    buffer_lines(chat:buffer())[2],
    "Session: status=ready · display=Your turn · skills=native"
  )
  chat:dispose()
end

T["chat"]["refreshes relative skill paths from the session working directory for each picker"] = function()
  local workspace = nvim.fn.tempname()
  local skill_root = nvim.fs.joinpath(workspace, "skills")
  local function write_skill(name)
    local directory = nvim.fs.joinpath(skill_root, name)
    assert(nvim.fn.mkdir(directory, "p") == 1)
    assert(
      nvim.fn.writefile(
        { "---", "name: " .. name, "description: " .. name, "---" },
        nvim.fs.joinpath(directory, "SKILL.md")
      ) == 0
    )
  end
  write_skill("first")
  local first = fake_session("session-1", "codex")
  first.state.skills_policy = "native"
  first.state.working_dir = workspace
  local chat = assert(Chat.new(fake_api(), { skill_paths = { "skills" } }))
  assert(chat:attach(first))
  local original_select = nvim.ui.select
  local catalogs = {}
  nvim.ui.select = function(items, _, callback)
    catalogs[#catalogs + 1] = nvim.tbl_map(function(skill)
      return skill.name
    end, items)
    callback(nil)
  end

  assert(chat:pick_skill())
  write_skill("second")
  assert(chat:pick_skill())

  nvim.ui.select = original_select
  MiniTest.expect.equality(catalogs, { { "first" }, { "first", "second" } })
  chat:dispose()
  nvim.fn.delete(workspace, "rf")
end

T["chat"]["deduplicates the summary warning while picker diagnostics stay unchanged"] = function()
  local first = fake_session("session-1", "codex")
  first.state.skills_policy = "native"
  first.state.working_dir = "/workspace"
  local chat = assert(Chat.new(fake_api(), { skill_paths = { "missing-one", "missing-two" } }))
  assert(chat:attach(first))
  local original_notify = nvim.notify
  local notifications = {}
  rawset(nvim, "notify", function(message, level)
    notifications[#notifications + 1] = { message = message, level = level }
  end)

  local first_started, first_error = chat:pick_skill()
  local second_started, second_error = chat:pick_skill()

  rawset(nvim, "notify", original_notify)
  MiniTest.expect.equality(first_started, false)
  MiniTest.expect.equality(first_error, "no chat skills configured")
  MiniTest.expect.equality(second_started, false)
  MiniTest.expect.equality(second_error, "no chat skills configured")
  MiniTest.expect.equality(notifications, {
    {
      message = "louiselm: skill discovery found 2 issue(s); run :checkhealth louiselm for details",
      level = nvim.log.levels.WARN,
    },
  })
  chat:dispose()
end

---@param root string
---@param name string
---@param description string
local function write_skill_file(root, name, description)
  local directory = nvim.fs.joinpath(root, name)
  assert(nvim.fn.mkdir(directory, "p") == 1)
  assert(
    nvim.fn.writefile(
      { "---", "name: " .. name, "description: " .. description, "---" },
      nvim.fs.joinpath(directory, "SKILL.md")
    ) == 0
  )
end

---Stub `vim.ui.select` to pick the first offered item; returns a restorer.
---@return fun()
local function select_first()
  local original_select = nvim.ui.select
  nvim.ui.select = function(select_items, _, callback)
    callback(select_items[1])
  end
  return function()
    nvim.ui.select = original_select
  end
end

T["chat"]["sends a native picker's resolved dollar command merged with the task as one text block"] = function()
  local workspace = nvim.fn.tempname()
  write_skill_file(workspace, "grill-me", "Stress-test an idea")
  local first = fake_session("session-1", "codex")
  first.state.skills_policy = "native"
  first.state.commands = { { name = "$grill-me", description = "unrelated advertised text" } }
  local chat = assert(Chat.new(fake_api(), { skill_paths = { workspace } }))
  assert(chat:attach(first))
  local restore = select_first()

  assert(chat:pick_skill())
  restore()
  MiniTest.expect.equality(buffer_lines(chat:buffer())[6], "> [context: skill: grill-me] ")

  assert(chat:submit("stress-test this"))

  MiniTest.expect.equality(first.prompts, { "/$grill-me stress-test this" })
  MiniTest.expect.equality(buffer_lines(chat:buffer())[6], "> stress-test this")
  chat:dispose()
  nvim.fn.delete(workspace, "rf")
end

T["chat"]["prefers the dollar form over a colliding bare built-in and sends it alone without a task"] = function()
  local workspace = nvim.fn.tempname()
  write_skill_file(workspace, "plan", "Draft an execution plan")
  local first = fake_session("session-1", "codex")
  first.state.skills_policy = "native"
  first.state.commands = {
    { name = "$plan", description = "Draft an execution plan" },
    { name = "plan", description = "Built-in planning mode, unrelated to the skill" },
  }
  local chat = assert(Chat.new(fake_api(), { skill_paths = { workspace } }))
  assert(chat:attach(first))
  local restore = select_first()

  assert(chat:pick_skill())
  restore()
  assert(chat:submit())

  MiniTest.expect.equality(first.prompts, { "/$plan" })
  chat:dispose()
  nvim.fn.delete(workspace, "rf")
end

T["chat"]["notifies and preserves the prompt and chip when the advertised command disappears before submission"] = function()
  local workspace = nvim.fn.tempname()
  write_skill_file(workspace, "grill-me", "Stress-test an idea")
  local first = fake_session("session-1", "codex")
  first.state.skills_policy = "native"
  first.state.commands = { { name = "$grill-me", description = "x" } }
  local chat = assert(Chat.new(fake_api(), { skill_paths = { workspace } }))
  assert(chat:attach(first))
  local restore = select_first()
  assert(chat:pick_skill())
  restore()

  first.state.commands = {}
  local original_notify = nvim.notify
  local notifications = {}
  rawset(nvim, "notify", function(message, level)
    notifications[#notifications + 1] = { message = message, level = level }
  end)
  local request_id, submit_error = chat:submit("stress-test this")
  rawset(nvim, "notify", original_notify)

  MiniTest.expect.equality(request_id, nil)
  MiniTest.expect.equality(submit_error, "no advertised command matches skill 'grill-me'")
  MiniTest.expect.equality(first.prompts, {})
  MiniTest.expect.equality(
    notifications,
    { { message = "louiselm: no advertised command matches skill 'grill-me'", level = nvim.log.levels.ERROR } }
  )
  MiniTest.expect.equality(buffer_lines(chat:buffer())[6], "> [context: skill: grill-me] stress-test this")
  chat:dispose()
  nvim.fn.delete(workspace, "rf")
end

T["chat"]["re-resolves a queued native command against the latest cache only when the turn releases it"] = function()
  local workspace = nvim.fn.tempname()
  write_skill_file(workspace, "grill-me", "Stress-test an idea")
  local first = fake_session("session-1", "codex")
  first.state.skills_policy = "native"
  first.state.status = "prompting"
  first.state.commands = {}
  local chat = assert(Chat.new(fake_api(), { skill_paths = { workspace } }))
  assert(chat:attach(first))
  local restore = select_first()
  assert(chat:pick_skill())
  restore()

  assert(chat:submit("stress-test this"))
  MiniTest.expect.equality(first.prompts, {})

  first.state.commands = { { name = "$grill-me", description = "x" } }
  first.state.status = "ready"
  first:emit({ type = "turn_done", session_id = "session-1", data = {} })
  nvim.wait(100, function()
    return #first.prompts == 1
  end, 1)

  MiniTest.expect.equality(first.prompts, { "/$grill-me stress-test this" })
  chat:dispose()
  nvim.fn.delete(workspace, "rf")
end

T["chat"]["leaves a manually typed slash command untouched in a native session"] = function()
  local first = fake_session("session-1", "codex")
  first.state.skills_policy = "native"
  first.state.commands = { { name = "grill-me", description = "Stress-test an idea" } }
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))

  assert(chat:submit("/grill-me hand-typed, not picked"))

  MiniTest.expect.equality(first.prompts, { "/grill-me hand-typed, not picked" })
  chat:dispose()
end

T["chat"]["keeps a new injected catalog hidden and defers it with contexts across slash prompts"] = function()
  local created = fake_session("session-1", "claude")
  created.state.skills_policy = "inject"
  local api = fake_api()
  api.create_session = function()
    return created
  end
  local chat = assert(Chat.new(api, {
    agents = { "claude" },
    skill_catalog = "<available_skills>catalog</available_skills>",
  }))

  assert(chat:new_session("claude"))
  assert(chat:queue_context({ label = "file: init.lua", text = "Referenced file: init.lua" }))
  MiniTest.expect.equality(table.concat(buffer_lines(chat:buffer()), "\n"):find("skill%-index"), nil)
  MiniTest.expect.equality(table.concat(buffer_lines(chat:buffer()), "\n"):find("available_skills", 1, true), nil)

  assert(chat:submit("/compact"))
  MiniTest.expect.equality(created.prompts, { "/compact" })
  MiniTest.expect.equality(buffer_lines(chat:buffer())[8], "> [context: file: init.lua] ")

  assert(chat:submit("Review this"))
  MiniTest.expect.equality(created.prompts[2], {
    { type = "text", text = "<available_skills>catalog</available_skills>" },
    { type = "text", text = "Referenced file: init.lua" },
    { type = "text", text = "Review this" },
  })

  assert(chat:submit("Again"))
  MiniTest.expect.equality(created.prompts[3], "Again")
  chat:dispose()
end

T["chat"]["retains a hidden catalog and visible contexts after a local prompt failure"] = function()
  local created = fake_session("session-1", "claude")
  created.state.skills_policy = "inject"
  local api = fake_api()
  api.create_session = function()
    return created
  end
  local chat = assert(Chat.new(api, {
    agents = { "claude" },
    skill_catalog = "hidden catalog",
  }))
  assert(chat:new_session("claude"))
  assert(chat:queue_context({ label = "file", text = "file body" }))
  created.prompt_error = "write failed"

  local request_id, prompt_error = chat:submit("draft")

  MiniTest.expect.equality({ request_id, prompt_error }, { nil, "write failed" })
  MiniTest.expect.equality(buffer_lines(chat:buffer())[6], "> [context: file] draft")
  created.prompt_error = nil
  assert(chat:submit())
  MiniTest.expect.equality(created.prompts[1], {
    { type = "text", text = "hidden catalog" },
    { type = "text", text = "file body" },
    { type = "text", text = "draft" },
  })
  chat:dispose()
end

T["chat"]["does not inject a catalog into an attached existing session"] = function()
  local existing = fake_session("session-1", "claude")
  existing.state.skills_policy = "inject"
  local chat = assert(Chat.new(fake_api(), { skill_catalog = "hidden catalog" }))

  assert(chat:attach(existing))
  assert(chat:submit("hello"))

  MiniTest.expect.equality(existing.prompts, { "hello" })
  chat:dispose()
end

T["chat"]["sends the exact selected skill body once in inject mode"] = function()
  local workspace = nvim.fn.tempname()
  write_skill_file(workspace, "grill-me", "Stress-test an idea")
  local path = nvim.fs.joinpath(workspace, "grill-me", "SKILL.md")
  local first_body = table.concat({ "---", "name: grill-me", "description: Stress-test an idea", "---" }, "\n") .. "\n"
  local first = fake_session("session-1", "claude")
  first.state.skills_policy = "inject"
  local chat = assert(Chat.new(fake_api(), { skill_paths = { workspace } }))
  assert(chat:attach(first))
  local restore = select_first()

  assert(chat:pick_skill())
  restore()
  assert(nvim.fn.writefile({ "changed after selection" }, path) == 0)
  assert(chat:submit("use it"))
  assert(chat:submit("again"))

  MiniTest.expect.equality(first.prompts[1], {
    { type = "text", text = first_body },
    { type = "text", text = "use it" },
  })
  MiniTest.expect.equality(first.prompts[2], "again")
  chat:dispose()
  nvim.fn.delete(workspace, "rf")
end

T["chat"]["blocks on a selected skill read failure while preserving the prompt and selection"] = function()
  local workspace = nvim.fn.tempname()
  write_skill_file(workspace, "grill-me", "Stress-test an idea")
  local path = nvim.fs.joinpath(workspace, "grill-me", "SKILL.md")
  local first = fake_session("session-1", "claude")
  first.state.skills_policy = "inject"
  local chat = assert(Chat.new(fake_api(), { skill_paths = { workspace } }))
  assert(chat:attach(first))
  nvim.api.nvim_buf_set_lines(chat:buffer(), 5, 6, false, { "> draft" })
  local original_select = nvim.ui.select
  nvim.ui.select = function(items, _, callback)
    assert(nvim.fn.delete(path) == 0)
    callback(items[1])
  end

  assert(chat:pick_skill())
  nvim.ui.select = original_select
  local request_id, prompt_error = chat:submit()

  MiniTest.expect.equality(request_id, nil)
  MiniTest.expect.equality(prompt_error, "could not read selected skill: " .. path)
  MiniTest.expect.equality(first.prompts, {})
  MiniTest.expect.equality(buffer_lines(chat:buffer())[6], "> [context: skill: grill-me] draft")

  assert(nvim.fn.writefile({ "restored body" }, path) == 0)
  assert(chat:submit())
  MiniTest.expect.equality(first.prompts[1], {
    { type = "text", text = "restored body\n" },
    { type = "text", text = "draft" },
  })
  chat:dispose()
  nvim.fn.delete(workspace, "rf")
end

T["chat"]["never resends a native command after it was consumed by an earlier prompt"] = function()
  local workspace = nvim.fn.tempname()
  write_skill_file(workspace, "grill-me", "Stress-test an idea")
  local first = fake_session("session-1", "codex")
  first.state.skills_policy = "native"
  first.state.commands = { { name = "$grill-me", description = "x" } }
  local chat = assert(Chat.new(fake_api(), { skill_paths = { workspace } }))
  assert(chat:attach(first))
  local restore = select_first()
  assert(chat:pick_skill())
  restore()
  assert(chat:submit("stress-test this"))

  assert(chat:submit("a follow-up with no skill picked"))

  MiniTest.expect.equality(first.prompts, {
    "/$grill-me stress-test this",
    "a follow-up with no skill picked",
  })
  chat:dispose()
  nvim.fn.delete(workspace, "rf")
end

T["chat"]["keeps tool activity out of persistent session diagnostics"] = function()
  local first = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))

  first.state.status = "prompting"
  first.state.activity = "exec command\nwith another line"
  first:emit({ type = "state_changed", session_id = "session-1", data = { status = "prompting" } })
  nvim.wait(100, function()
    return buffer_lines(chat:buffer())[2] == "Session: status=prompting · display=Model responding"
  end, 1)

  MiniTest.expect.equality(nvim.list_slice(buffer_lines(chat:buffer()), 1, 4), {
    "# claude · session-1",
    "Session: status=prompting · display=Model responding",
    "ACP options:",
    "Telemetry:",
  })
  chat:dispose()
end

T["chat"]["queues one prompt in every active turn state and releases it only on turn completion"] = function()
  for _, status in ipairs({ "prompting", "waiting_permission", "cancelling" }) do
    local first = fake_session("session-" .. status, "claude")
    first.state.status = status
    local chat = assert(Chat.new(fake_api()))
    assert(chat:attach(first))

    assert(chat:submit("/compact"))
    MiniTest.expect.equality(first.prompts, {})
    local display = status == "prompting" and "Model responding"
      or status == "waiting_permission" and "Waiting for permission"
      or "Stopping"
    MiniTest.expect.equality(
      buffer_lines(chat:buffer()),
      chat_lines("claude · session-" .. status, "status=" .. status .. " · display=" .. display, {
        "",
        "> /compact",
      })
    )
    MiniTest.expect.equality(virtual_text(chat:buffer()), { "Queued for next turn" })

    first.state.status = "ready"
    first:emit({ type = "turn_done", session_id = first.state.id, data = { stopReason = "end_turn" } })
    nvim.wait(100, function()
      return #first.prompts == 1
    end, 1)

    MiniTest.expect.equality(first.prompts, { "/compact" })
    MiniTest.expect.equality(
      buffer_lines(chat:buffer()),
      chat_lines("claude · session-" .. status, "status=" .. status .. " · display=" .. display, {
        "",
        "> /compact",
        "",
        "> ",
      })
    )
    MiniTest.expect.equality(virtual_text(chat:buffer()), {})
    chat:dispose()
  end
end

T["chat"]["keeps an edited queued prompt as a draft until Enter recommits it"] = function()
  local first = fake_session("session-1", "claude")
  first.state.status = "prompting"
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  assert(chat:submit("original"))

  nvim.api.nvim_buf_set_lines(chat:buffer(), 5, 6, false, { "> revised" })
  first.state.status = "ready"
  first:emit({ type = "turn_done", session_id = "session-1", data = {} })
  nvim.wait(20)

  MiniTest.expect.equality(first.prompts, {})
  MiniTest.expect.equality(virtual_text(chat:buffer()), {})
  assert(chat:submit())
  MiniTest.expect.equality(first.prompts, { "revised" })
  chat:dispose()
end

T["chat"]["snapshots queued context and keeps it isolated with its session"] = function()
  local first = fake_session("session-1", "one")
  local second = fake_session("session-2", "two")
  first.state.status = "prompting"
  second.state.status = "prompting"
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  local item = { label = "file", text = "original context" }
  assert(chat:queue_context(item))
  assert(chat:submit("first prompt"))
  item.text = "changed context"

  assert(chat:attach(second))
  assert(chat:submit("second prompt"))
  assert(chat:switch("session-1"))
  first.state.status = "ready"
  first:emit({ type = "turn_done", session_id = "session-1", data = {} })
  nvim.wait(100, function()
    return #first.prompts == 1
  end, 1)

  MiniTest.expect.equality(first.prompts, {
    {
      { type = "text", text = "original context" },
      { type = "text", text = "first prompt" },
    },
  })
  MiniTest.expect.equality(second.prompts, {})
  MiniTest.expect.equality(virtual_text(chat:buffer("session-2")), { "Queued for next turn" })
  chat:dispose()
end

T["chat"]["preserves rejected and failed prompts outside transcript history"] = function()
  local original_notify = nvim.notify
  local notifications = {}
  rawset(nvim, "notify", function(message)
    notifications[#notifications + 1] = message
  end)

  local starting = fake_session("starting", "claude")
  starting.state.status = "starting"
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(starting))
  local request_id, start_error = chat:submit("draft")
  MiniTest.expect.equality({ request_id, start_error }, { nil, "session is not ready" })
  MiniTest.expect.equality(
    buffer_lines(chat:buffer()),
    chat_lines("claude · starting", "status=starting · display=Starting", { "", "> draft" })
  )

  starting.state.status = "ready"
  function starting:prompt()
    return nil, "write failed"
  end
  local failed_id, failed_error = chat:submit()
  rawset(nvim, "notify", original_notify)

  MiniTest.expect.equality({ failed_id, failed_error }, { nil, "write failed" })
  MiniTest.expect.equality(
    buffer_lines(chat:buffer()),
    chat_lines("claude · starting", "status=starting · display=Starting", { "", "> draft" })
  )
  MiniTest.expect.equality(notifications, { "louiselm: session is not ready", "louiselm: write failed" })
  chat:dispose()
end

T["chat"]["does not release queued work after session error or chat disposal"] = function()
  local failed = fake_session("failed", "claude")
  failed.state.status = "prompting"
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(failed))
  assert(chat:submit("keep me"))
  failed.state.status = "error"
  failed:emit({ type = "error", session_id = "failed", data = { message = "agent failed" } })
  nvim.wait(100, function()
    return virtual_text(chat:buffer())[1] == nil
  end, 1)
  MiniTest.expect.equality(failed.prompts, {})
  MiniTest.expect.equality(buffer_lines(chat:buffer())[7], "> keep me")

  local late = fake_session("late", "claude")
  late.state.status = "prompting"
  assert(chat:attach(late))
  assert(chat:submit("never send"))
  local original_schedule = nvim.schedule
  local scheduled = {}
  nvim.schedule = function(callback)
    scheduled[#scheduled + 1] = callback
  end
  late.state.status = "ready"
  late:emit({ type = "turn_done", session_id = "late", data = {} })
  chat:dispose()
  scheduled[1]()
  nvim.schedule = original_schedule

  MiniTest.expect.equality(late.prompts, {})
end

T["chat"]["warns that closing an active session discards its queued prompt"] = function()
  local first = fake_session("session-1", "claude")
  first.state.status = "prompting"
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  assert(chat:submit("discard me"))
  local original_select = nvim.ui.select
  local close_prompt
  nvim.ui.select = function(_, options, callback)
    close_prompt = options.prompt
    callback("Close")
  end

  assert(chat:close_session())
  nvim.ui.select = original_select

  MiniTest.expect.equality(close_prompt, "close active louiselm session and discard queued prompt? ")
  MiniTest.expect.equality(first.disposed, true)
  MiniTest.expect.equality(first.prompts, {})
  chat:dispose()
end

T["chat"]["keeps interleaved response and tool events chronological"] = function()
  local first = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))

  assert(chat:submit("hello"))
  first:emit({
    type = "chunk",
    session_id = "session-1",
    data = { content = { type = "text", text = "before tool" } },
  })
  first:emit({
    type = "tool_call_started",
    session_id = "session-1",
    data = { toolCallId = "tool-1", title = "Read file" },
  })
  first:emit({
    type = "tool_call_finished",
    session_id = "session-1",
    data = { toolCallId = "tool-1", status = "completed" },
  })
  first:emit({
    type = "chunk",
    session_id = "session-1",
    data = { content = { type = "text", text = "after tool" } },
  })

  nvim.wait(100, function()
    return #buffer_lines(chat:buffer()) == 10
  end, 1)

  MiniTest.expect.equality(
    buffer_lines(chat:buffer()),
    chat_lines("claude · session-1", "status=ready · display=Your turn", {
      "",
      "> hello",
      "",
      "before tool",
      "[tool] tool-1: Read file (completed)",
      "after tool",
      "> ",
    })
  )

  chat:dispose()
end

T["chat"]["keeps multiline tool titles on one buffer line"] = function()
  local first = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))

  first:emit({
    type = "tool_call_started",
    session_id = "session-1",
    data = { toolCallId = "tool-1", title = "first line\nsecond line" },
  })
  first:emit({
    type = "tool_call_finished",
    session_id = "session-1",
    data = { toolCallId = "tool-1", status = "completed" },
  })

  nvim.wait(100, function()
    return #buffer_lines(chat:buffer()) == 7
  end, 1)

  MiniTest.expect.equality(
    buffer_lines(chat:buffer()),
    chat_lines("claude · session-1", "status=ready · display=Your turn", {
      "",
      "[tool] tool-1: first line second line (completed)",
      "> ",
    })
  )

  chat:dispose()
end

T["chat"]["splits multiline error messages before inserting them"] = function()
  local first = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))

  first:emit({
    type = "error",
    session_id = "session-1",
    data = { message = "first line\nsecond line" },
  })
  nvim.wait(100, function()
    return #buffer_lines(chat:buffer()) == 8
  end, 1)

  MiniTest.expect.equality(
    buffer_lines(chat:buffer()),
    chat_lines("claude · session-1", "status=ready · display=Your turn", {
      "",
      "Error: first line",
      "second line",
      "> ",
    })
  )

  chat:dispose()
end

T["chat"]["schedules session events before touching buffers"] = function()
  local first = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  assert(chat:submit("hello"))
  local original_schedule = nvim.schedule
  local scheduled = {}
  nvim.schedule = function(callback)
    scheduled[#scheduled + 1] = callback
  end

  first:emit({
    type = "chunk",
    session_id = "session-1",
    data = { content = { type = "text", text = "scheduled" } },
  })

  MiniTest.expect.equality(#scheduled, 1)
  MiniTest.expect.equality(
    buffer_lines(chat:buffer()),
    chat_lines("claude · session-1", "status=ready · display=Your turn", { "", "> hello", "", "> " })
  )
  scheduled[1]()
  nvim.schedule = original_schedule

  MiniTest.expect.equality(
    buffer_lines(chat:buffer()),
    chat_lines("claude · session-1", "status=ready · display=Your turn", {
      "",
      "> hello",
      "",
      "scheduled",
      "> ",
    })
  )
  chat:dispose()
end

T["chat"]["renders replayed assistant chunks before a new prompt"] = function()
  local restored = fake_session("session-1", "codex")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(restored))
  local original_schedule = nvim.schedule
  local scheduled = {}
  nvim.schedule = function(callback)
    scheduled[#scheduled + 1] = callback
  end

  restored:emit({
    type = "chunk",
    session_id = "session-1",
    data = { content = { type = "text", text = "replayed" } },
  })

  scheduled[1]()
  nvim.schedule = original_schedule

  MiniTest.expect.equality(
    buffer_lines(chat:buffer()),
    chat_lines("codex · session-1", "status=ready · display=Your turn", { "", "replayed", "> " })
  )
  chat:dispose()
end

T["chat"]["discovers and resumes into a separate scheduled chat view"] = function()
  local first = fake_session("session-1", "claude")
  local restored = fake_session("session-2", "codex")
  restored.state.status = "starting"
  local api = fake_api()
  local discovery_options
  local discovery_callback
  local load_call
  function api:discover_sessions(options, callback)
    discovery_options = options
    discovery_callback = callback
    return true
  end
  function api:load_session(agent, session_id, options, ready_callback)
    load_call = { agent = agent, session_id = session_id, options = options, ready_callback = ready_callback }
    return restored
  end
  function api:list_sessions()
    return { "session-1", "session-2" }
  end

  local chat = assert(Chat.new(api))
  assert(chat:attach(first))
  local first_buffer = chat:buffer("session-1")
  local original_schedule = nvim.schedule
  local original_select = nvim.ui.select
  local scheduled = {}
  local formatted
  nvim.schedule = function(callback)
    scheduled[#scheduled + 1] = callback
  end
  nvim.ui.select = function(items, options, callback)
    formatted = options.format_item(items[1])
    callback(items[1])
  end

  assert(chat:resume_session())
  MiniTest.expect.equality(discovery_options, { cwd = nvim.fn.getcwd() })
  discovery_callback({
    {
      agent = "codex",
      session_id = "prior-acp",
      cwd = "/tmp/project",
      title = "Previous work",
      updated_at = "2026-08-10T10:00:00Z",
    },
  }, {})
  MiniTest.expect.equality(load_call, nil)
  scheduled[1]()

  MiniTest.expect.equality(
    formatted,
    "codex/prior-acp · Previous work · cwd=/tmp/project · updated=2026-08-10T10:00:00Z"
  )
  MiniTest.expect.equality(load_call.agent, "codex")
  MiniTest.expect.equality(load_call.session_id, "prior-acp")
  MiniTest.expect.equality(load_call.options, { cwd = "/tmp/project", name = "Previous work" })
  MiniTest.expect.equality(nvim.api.nvim_buf_is_valid(first_buffer), true)
  MiniTest.expect.equality(first.disposed, false)
  MiniTest.expect.equality(chat:buffer(), chat:buffer("session-2"))

  restored:emit({
    type = "chunk",
    session_id = "session-2",
    data = { content = { type = "text", text = "replayed history" } },
  })
  restored.state.status = "ready"
  restored:emit({ type = "state_changed", session_id = "session-2", data = { status = "ready" } })
  scheduled[2]()
  scheduled[3]()
  load_call.ready_callback(restored)

  MiniTest.expect.equality(
    buffer_lines(chat:buffer("session-2")),
    chat_lines("codex · session-2", "status=ready · display=Your turn", { "", "replayed history", "> " })
  )

  nvim.schedule = original_schedule
  nvim.ui.select = original_select
  chat:dispose()
end

T["chat"]["reports stable ACP identities for new and resumed sessions"] = function()
  local created = fake_session("session-1", "claude")
  created.state.acp_session_id = "created-acp"
  local loaded = fake_session("session-2", "codex")
  loaded.state.acp_session_id = "loaded-acp"
  loaded.state.source = "loaded"
  local chat = assert(Chat.new(fake_api()))

  local report_id, missing_error = chat:session_id()
  MiniTest.expect.equality(report_id, nil)
  MiniTest.expect.equality(missing_error, "no chat session is open")

  assert(chat:attach(created))
  MiniTest.expect.equality(chat:session_id(), "claude/created-acp")
  assert(chat:attach(loaded))
  MiniTest.expect.equality(chat:session_id(), "codex/loaded-acp")
  chat:dispose()
end

T["chat"]["ignores scheduled discovery after disposal and supports all workspaces"] = function()
  local api = fake_api()
  local discovery_options
  local discovery_callback
  function api:discover_sessions(options, callback)
    discovery_options = options
    discovery_callback = callback
    return true
  end
  local chat = assert(Chat.new(api))
  local original_schedule = nvim.schedule
  local original_select = nvim.ui.select
  local scheduled = {}
  local selected = false
  nvim.schedule = function(callback)
    scheduled[#scheduled + 1] = callback
  end
  nvim.ui.select = function()
    selected = true
  end

  assert(chat:resume_session(true))
  MiniTest.expect.equality(discovery_options, {})
  discovery_callback({ { agent = "codex", session_id = "prior", cwd = "/tmp/project" } }, {})
  chat:dispose()
  scheduled[1]()

  nvim.schedule = original_schedule
  nvim.ui.select = original_select
  MiniTest.expect.equality(selected, false)
end

T["chat"]["reports a selected load failure without creating a replacement"] = function()
  local api = fake_api()
  local created = 0
  function api:create_session()
    created = created + 1
    return fake_session("replacement", "codex")
  end
  function api:discover_sessions(_, callback)
    callback({ { agent = "codex", session_id = "stale", cwd = "/tmp/project" } }, {})
    return true
  end
  function api:load_session()
    return nil, "ACP session/load failed: unknown session"
  end
  local original_select = nvim.ui.select
  local original_notify = nvim.notify
  local notifications = {}
  nvim.ui.select = function(items, _, callback)
    callback(items[1])
  end
  rawset(nvim, "notify", function(message)
    notifications[#notifications + 1] = message
  end)

  local chat = assert(Chat.new(api))
  assert(chat:resume_session())
  nvim.wait(100, function()
    return #notifications > 0
  end, 1)

  nvim.ui.select = original_select
  rawset(nvim, "notify", original_notify)
  MiniTest.expect.equality(notifications, { "louiselm: ACP session/load failed: unknown session" })
  MiniTest.expect.equality(created, 0)
  MiniTest.expect.equality(chat:buffer(), nil)
  chat:dispose()
end

T["chat"]["keeps a blank boundary before the first scheduled assistant event"] = function()
  local first = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  assert(chat:submit("hello"))
  local original_schedule = nvim.schedule
  local scheduled = {}
  nvim.schedule = function(callback)
    scheduled[#scheduled + 1] = callback
  end

  first:emit({
    type = "chunk",
    session_id = "session-1",
    data = { content = { type = "text", text = "scheduled" } },
  })

  MiniTest.expect.equality(
    buffer_lines(chat:buffer()),
    chat_lines("claude · session-1", "status=ready · display=Your turn", { "", "> hello", "", "> " })
  )
  scheduled[1]()
  nvim.schedule = original_schedule

  MiniTest.expect.equality(
    buffer_lines(chat:buffer()),
    chat_lines("claude · session-1", "status=ready · display=Your turn", {
      "",
      "> hello",
      "",
      "scheduled",
      "> ",
    })
  )
  chat:dispose()
end

T["chat"]["keeps the boundary when a tool call is the first event"] = function()
  local first = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  assert(chat:submit("hello"))
  local original_schedule = nvim.schedule
  local scheduled = {}
  nvim.schedule = function(callback)
    scheduled[#scheduled + 1] = callback
  end

  first:emit({
    type = "tool_call_started",
    session_id = "session-1",
    data = { toolCallId = "tool-1", title = "Read file" },
  })
  MiniTest.expect.equality(#scheduled, 1)
  scheduled[1]()
  nvim.schedule = original_schedule

  MiniTest.expect.equality(
    buffer_lines(chat:buffer()),
    chat_lines("claude · session-1", "status=ready · display=Your turn", {
      "",
      "> hello",
      "",
      "[tool] tool-1: Read file (started)",
      "> ",
    })
  )
  chat:dispose()
end

T["chat"]["opens file permission requests in a scheduled diff review"] = function()
  local path = nvim.fn.tempname()
  nvim.fn.writefile({ "before" }, path)
  local first = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  local original_schedule = nvim.schedule
  local scheduled = {}
  nvim.schedule = function(callback)
    scheduled[#scheduled + 1] = callback
  end

  local response
  first:emit({
    type = "permission_requested",
    session_id = "session-1",
    data = {
      operation = { kind = "file_edit", path = path },
      toolCall = { rawInput = { path = path, content = "after\n" } },
      options = { { optionId = "allow-once", kind = "allow_once" }, "deny" },
    },
    respond = function(result)
      response = result
      return true
    end,
  })

  MiniTest.expect.equality(#scheduled, 1)
  MiniTest.expect.equality(chat.diff.buffer, nil)
  scheduled[1]()
  nvim.schedule = original_schedule

  MiniTest.expect.equality(nvim.api.nvim_buf_get_name(chat.diff.buffer), "louiselm-diff://" .. path)
  assert(chat.diff:accept())
  MiniTest.expect.equality(response, { outcome = { outcome = "selected", optionId = "allow-once" } })
  chat:dispose()
  nvim.fn.delete(path)
end

T["chat"]["schedules and resolves command and unknown permission requests"] = function()
  local first = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  local original_schedule = nvim.schedule
  local original_select = nvim.ui.select
  local scheduled = {}
  local selections = 0
  local prompts = {}
  local responses = {}
  nvim.schedule = function(callback)
    scheduled[#scheduled + 1] = callback
  end

  first:emit({
    type = "permission_requested",
    session_id = "session-1",
    data = {
      operation = { kind = "command", command = { "git", "status" } },
      options = { { optionId = "allow-once", kind = "allow_once" }, { optionId = "deny", kind = "deny" } },
    },
    respond = function(result)
      responses[#responses + 1] = result
      return true
    end,
  })
  first:emit({
    type = "permission_requested",
    session_id = "session-1",
    data = { operation = { kind = "unknown" }, options = { "allow", "deny" } },
    respond = function(result)
      responses[#responses + 1] = result
      return true
    end,
  })
  first:emit({
    type = "permission_requested",
    session_id = "session-1",
    data = { operation = { kind = "unknown" }, options = { "allow", "deny" } },
    respond = function(result)
      responses[#responses + 1] = result
      return true
    end,
  })

  MiniTest.expect.equality(#scheduled, 3)
  MiniTest.expect.equality(responses, {})
  nvim.ui.select = function(options, select_options, callback)
    selections = selections + 1
    prompts[#prompts + 1] = select_options.prompt
    if selections == 1 then
      callback(options[1], 1)
    elseif selections == 2 then
      callback(nil)
    else
      callback({})
    end
  end
  scheduled[1]()
  scheduled[2]()
  scheduled[3]()
  nvim.schedule = original_schedule
  nvim.ui.select = original_select

  MiniTest.expect.equality(responses, {
    { outcome = { outcome = "selected", optionId = "allow-once" } },
    { outcome = { outcome = "cancelled" } },
    { outcome = { outcome = "cancelled" } },
  })
  MiniTest.expect.equality(prompts, {
    'louiselm permission (command): ["git","status"] ',
    "louiselm permission (unknown, details unavailable): ",
    "louiselm permission (unknown, details unavailable): ",
  })
  chat:dispose()
end

T["chat"]["puts rejection options first so permission pickers fail closed"] = function()
  local first = fake_session("session-1", "codex")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  local original_schedule = nvim.schedule
  local original_select = nvim.ui.select
  local scheduled
  local labels
  local option_ids
  local prompt
  local response
  nvim.schedule = function(callback)
    scheduled = callback
  end

  first:emit({
    type = "permission_requested",
    session_id = "session-1",
    data = {
      operation = { kind = "command", command = { "git", "commit" } },
      options = {
        { optionId = "allow_once", name = "Allow Once", kind = "allow_once" },
        { optionId = "allow_always", name = "Allow for Session", kind = "allow_always" },
        { optionId = "allow_prefix", name = "Allow Commands Starting With git", kind = "allow_always" },
        { optionId = "reject_once", name = "Reject", kind = "reject_once" },
      },
    },
    respond = function(result)
      response = result
      return true
    end,
  })

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
  assert(scheduled)
  scheduled()
  nvim.schedule = original_schedule
  nvim.ui.select = original_select

  MiniTest.expect.equality(labels, { "Reject", "Allow Once", "Allow for Session", "Allow Commands Starting With git" })
  MiniTest.expect.equality(option_ids, { "reject_once", "allow_once", "allow_always", "allow_prefix" })
  MiniTest.expect.equality(prompt, 'louiselm permission (command): ["git","commit"] ')
  MiniTest.expect.equality(response, { outcome = { outcome = "selected", optionId = "reject_once" } })
  chat:dispose()
end

T["chat"]["sends the exact permission option chosen by the select provider"] = function()
  local first = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  local original_schedule = nvim.schedule
  local original_select = nvim.ui.select
  local scheduled
  local labels
  local prompt
  local response
  local command =
    'git add nvim/.config/nvim/nvim-pack-lock.json && git commit -m "chore(nvim): update lock" && git push'
  nvim.schedule = function(callback)
    scheduled = callback
  end
  nvim.ui.select = function(options, select_options, callback)
    prompt = select_options.prompt
    labels = {}
    for _, option in ipairs(options) do
      labels[#labels + 1] = select_options.format_item(option)
    end
    callback(options[2], 2)
  end

  first:emit({
    type = "permission_requested",
    session_id = "session-1",
    data = {
      operation = { kind = "command", command = { command } },
      options = {
        { optionId = "reject", name = "Deny", kind = "reject_once" },
        { optionId = "allow", name = "Allow Once", kind = "allow_once" },
        { optionId = "allow_always", name = "Always Allow", kind = "allow_always" },
      },
    },
    respond = function(result)
      response = result
      return true
    end,
  })

  assert(scheduled)
  scheduled()
  nvim.schedule = original_schedule
  nvim.ui.select = original_select

  MiniTest.expect.equality(prompt, "louiselm permission (command): " .. nvim.json.encode({ command }) .. " ")
  MiniTest.expect.equality(labels, { "Deny", "Allow Once", "Always Allow" })
  MiniTest.expect.equality(response, { outcome = { outcome = "selected", optionId = "allow" } })
  chat:dispose()
end

T["chat"]["opens permission pickers outside insert mode"] = function()
  local first = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  local original_schedule = nvim.schedule
  local original_select = nvim.ui.select
  local original_stopinsert = nvim.cmd.stopinsert
  local scheduled
  local mode = "i"
  local mode_at_select
  nvim.schedule = function(callback)
    scheduled = callback
  end
  nvim.cmd.stopinsert = function()
    mode = "n"
  end
  nvim.ui.select = function(options, _, callback)
    mode_at_select = mode
    callback(options[2], 2)
  end

  first:emit({
    type = "permission_requested",
    session_id = "session-1",
    data = {
      operation = { kind = "command", command = { "rm", "toto.md" } },
      options = {
        { optionId = "reject", name = "Deny", kind = "reject_once" },
        { optionId = "allow", name = "Allow Once", kind = "allow_once" },
      },
    },
    respond = function()
      return true
    end,
  })

  assert(scheduled)
  scheduled()
  nvim.schedule = original_schedule
  nvim.ui.select = original_select
  nvim.cmd.stopinsert = original_stopinsert

  MiniTest.expect.equality(mode_at_select, "n")
  chat:dispose()
end

T["chat"]["preserves distinct permission option names with the same kind"] = function()
  local first = fake_session("session-1", "codex")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  local original_schedule = nvim.schedule
  local original_select = nvim.ui.select
  local scheduled = {}
  local labels
  local prompt
  local response
  nvim.schedule = function(callback)
    scheduled[#scheduled + 1] = callback
  end

  first:emit({
    type = "permission_requested",
    session_id = "session-1",
    data = {
      operation = { kind = "command" },
      options = {
        { optionId = "session", name = "Allow for This Session", kind = "allow_always" },
        { optionId = "always", name = "Allow and Don't Ask Again", kind = "allow_always" },
      },
    },
    respond = function(result)
      response = result
      return true
    end,
  })

  nvim.ui.select = function(options, select_options, callback)
    prompt = select_options.prompt
    labels = {}
    for _, option in ipairs(options) do
      labels[#labels + 1] = select_options.format_item(option)
    end
    callback(options[2], 2)
  end
  scheduled[1]()
  nvim.schedule = original_schedule
  nvim.ui.select = original_select

  MiniTest.expect.equality(labels, { "Allow for This Session", "Allow and Don't Ask Again" })
  MiniTest.expect.equality(prompt, "louiselm permission (command, details unavailable): ")
  MiniTest.expect.equality(response, { outcome = { outcome = "selected", optionId = "always" } })
  chat:dispose()
end

---@return fun(session: table, id: string) request Emit one command permission request.
---@return fun() run_scheduled Drain callbacks queued through the stubbed `vim.schedule`.
---@return table pickers Recorded `vim.ui.select` calls.
---@return table responses Recorded permission responses.
local function permission_harness()
  local scheduled = {}
  local drained = 0
  local pickers = {}
  local responses = {}
  nvim.schedule = function(callback)
    scheduled[#scheduled + 1] = callback
  end
  nvim.ui.select = function(items, options, callback)
    pickers[#pickers + 1] = { items = items, prompt = options.prompt, callback = callback }
  end
  local function request(session, id)
    session:emit({
      type = "permission_requested",
      session_id = session.state.id,
      data = {
        request_id = id,
        operation = { kind = "command", command = { "git", "status" } },
        options = {
          { optionId = "allow-once", kind = "allow_once" },
          { optionId = "deny", kind = "deny_once" },
        },
      },
      respond = function(result)
        responses[#responses + 1] = { id = id, result = result }
        return true
      end,
    })
  end
  local function run_scheduled()
    while drained < #scheduled do
      drained = drained + 1
      scheduled[drained]()
    end
  end
  return request, run_scheduled, pickers, responses
end

T["chat"]["opens one permission decision at a time across sessions"] = function()
  local first = fake_session("session-1", "claude")
  local second = fake_session("session-2", "codex")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  assert(chat:attach(second))
  local request, run_scheduled, pickers, responses = permission_harness()

  request(first, "first")
  request(second, "second")
  run_scheduled()

  MiniTest.expect.equality(#pickers, 1)
  MiniTest.expect.equality(responses, {})

  pickers[1].callback(pickers[1].items[2], 2)
  run_scheduled()

  MiniTest.expect.equality(#pickers, 2)
  MiniTest.expect.equality(#responses, 1)

  pickers[2].callback(nil)
  run_scheduled()

  MiniTest.expect.equality(responses, {
    { id = "first", result = { outcome = { outcome = "selected", optionId = "allow-once" } } },
    { id = "second", result = { outcome = { outcome = "cancelled" } } },
  })
  chat:dispose()
end

T["chat"]["cancels permission decisions still queued when the chat is disposed"] = function()
  local first = fake_session("session-1", "claude")
  local second = fake_session("session-2", "codex")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  assert(chat:attach(second))
  local request, run_scheduled, pickers, responses = permission_harness()

  request(first, "first")
  request(second, "second")
  run_scheduled()
  MiniTest.expect.equality(#pickers, 1)

  chat:dispose()
  MiniTest.expect.equality(responses, { { id = "second", result = { outcome = { outcome = "cancelled" } } } })

  pickers[1].callback(pickers[1].items[2], 2)
  run_scheduled()

  MiniTest.expect.equality(#pickers, 1)
  MiniTest.expect.equality(responses[2], { id = "first", result = { outcome = { outcome = "cancelled" } } })
end

T["chat"]["refuses command pickers while a permission decision is open"] = function()
  local first = fake_session("session-1", "claude")
  first.state.status = "waiting_permission"
  first.state.config_options = {
    { id = "model", name = "Model", type = "select", current_value = "opus", options = { { value = "large" } } },
  }
  local api = fake_api()
  api.list_permissions = function()
    return {}
  end
  local chat = assert(Chat.new(api, { agents = { "one", "two" } }))
  assert(chat:attach(first))
  local request, run_scheduled, pickers, responses = permission_harness()

  request(first, "first")
  run_scheduled()
  MiniTest.expect.equality(#pickers, 1)

  local busy = "a louiselm permission decision is open; answer it first"
  MiniTest.expect.equality({ chat:pick_skill() }, { false, busy })
  MiniTest.expect.equality({ chat:pick_file() }, { false, busy })
  MiniTest.expect.equality({ chat:switch_session() }, { false, busy })
  MiniTest.expect.equality({ chat:session_options() }, { false, busy })
  MiniTest.expect.equality({ chat:manage_permissions() }, { false, busy })
  MiniTest.expect.equality({ chat:resume_session() }, { false, busy })
  MiniTest.expect.equality({ chat:new_session() }, { nil, busy })
  MiniTest.expect.equality(#pickers, 1)

  -- Closing the session stays reachable: it is the way out when a decision is stuck.
  assert(chat:close_session())
  MiniTest.expect.equality(#pickers, 2)
  MiniTest.expect.equality(pickers[2].prompt, "close active louiselm session? ")

  pickers[1].callback(pickers[1].items[2], 2)
  run_scheduled()
  MiniTest.expect.equality(#responses, 1)
  MiniTest.expect.equality({ chat:switch_session() }, { false, "no chat sessions are attached" })
  chat:dispose()
end

T["chat"]["closes a review and frees the chat when its request is cancelled"] = function()
  local path = nvim.fn.tempname()
  nvim.fn.writefile({ "before" }, path)
  local first = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  local request, run_scheduled, pickers, responses = permission_harness()

  first:emit({
    type = "permission_requested",
    session_id = "session-1",
    data = {
      request_id = 7,
      operation = { kind = "file_edit", path = path },
      toolCall = { rawInput = { path = path, content = "after\n" } },
      options = { { optionId = "allow-once", kind = "allow_once" } },
    },
    respond = function()
      return true
    end,
  })
  request(first, "queued")
  run_scheduled()

  MiniTest.expect.equality(nvim.api.nvim_buf_get_name(chat.diff.buffer), "louiselm-diff://" .. path)
  MiniTest.expect.equality(
    { chat:switch_session() },
    { false, "a louiselm permission decision is open; answer it first" }
  )
  MiniTest.expect.equality(#pickers, 0)

  first.state.status = "cancelling"
  first:emit({ type = "permission_cancelled", session_id = "session-1", data = { request_ids = { 7 } } })
  run_scheduled()

  MiniTest.expect.equality(chat.diff.buffer, nil)
  MiniTest.expect.equality(#pickers, 1)
  MiniTest.expect.equality(responses, {})

  pickers[1].callback(nil)
  run_scheduled()
  MiniTest.expect.equality(responses, { { id = "queued", result = { outcome = { outcome = "cancelled" } } } })
  chat:dispose()
  nvim.fn.delete(path)
end

T["chat"]["drops queued decisions whose requests were cancelled"] = function()
  local first = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  local request, run_scheduled, pickers, responses = permission_harness()

  request(first, "open")
  request(first, "queued")
  run_scheduled()
  MiniTest.expect.equality(#pickers, 1)

  first:emit({
    type = "permission_cancelled",
    session_id = "session-1",
    data = { request_ids = { "open", "queued" } },
  })
  run_scheduled()

  -- Both were answered by the session, so neither may be answered again, and the
  -- cancelled ones must not leave the chat blocked. Getting past the decision guard
  -- to switch_session's own error proves the slot was released.
  MiniTest.expect.equality(#pickers, 1)
  MiniTest.expect.equality(responses, {})
  MiniTest.expect.equality({ chat:switch_session() }, { false, "no chat sessions are attached" })
  chat:dispose()
end

T["chat"]["cancels a permission request delivered after disposal"] = function()
  local first = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  local request, run_scheduled, pickers, responses = permission_harness()

  request(first, "first")
  chat:dispose()
  run_scheduled()

  MiniTest.expect.equality(#pickers, 0)
  MiniTest.expect.equality(responses, { { id = "first", result = { outcome = { outcome = "cancelled" } } } })
end

T["chat"]["cancels a queued permission choice after disposal"] = function()
  local first = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  local original_schedule = nvim.schedule
  local original_select = nvim.ui.select
  local scheduled = {}
  local choose
  local response
  nvim.schedule = function(callback)
    scheduled[#scheduled + 1] = callback
  end
  nvim.ui.select = function(_, _, callback)
    choose = callback
  end

  first:emit({
    type = "permission_requested",
    session_id = "session-1",
    data = { operation = { kind = "unknown" }, options = { "allow", "deny" } },
    respond = function(result)
      response = result
      return true
    end,
  })
  scheduled[1]()
  chat:dispose()
  choose("allow", 1)

  nvim.schedule = original_schedule
  nvim.ui.select = original_select
  MiniTest.expect.equality(response, { outcome = { outcome = "cancelled" } })
end

T["chat"]["queues context items as ACP text before the user prompt"] = function()
  local first = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  assert(chat:queue_context({ label = "file: init.lua", text = "Referenced file: init.lua" }))

  MiniTest.expect.equality(
    buffer_lines(chat:buffer()),
    chat_lines("claude · session-1", "status=ready · display=Your turn", {
      "",
      "> [context: file: init.lua] ",
    })
  )
  assert(chat:submit("Review this"))

  MiniTest.expect.equality(first.prompts, {
    {
      { type = "text", text = "Referenced file: init.lua" },
      { type = "text", text = "Review this" },
    },
  })
  chat:dispose()
end

T["chat"]["renders every accepted context in one closed native fold before the user prompt"] = function()
  local created = fake_session("session-1", "claude")
  created.state.skills_policy = "inject"
  local api = fake_api()
  api.create_session = function()
    return created
  end
  local chat = assert(Chat.new(api, {
    agents = { "claude" },
    skill_catalog = "catalog line one\ncatalog line two",
  }))
  assert(chat:new_session("claude"))
  assert(chat:queue_context({ label = "file: init.lua", text = "first line\nsecond line" }))
  assert(chat:queue_context({ label = "AGENTS.md", uri = "file:///repo/AGENTS.md" }))

  assert(chat:submit("Review this"))

  MiniTest.expect.equality(created.prompts[1], {
    { type = "text", text = "catalog line one\ncatalog line two" },
    { type = "text", text = "first line\nsecond line" },
    { type = "resource_link", uri = "file:///repo/AGENTS.md", name = "AGENTS.md" },
    { type = "text", text = "Review this" },
  })
  MiniTest.expect.equality(buffer_lines(chat:buffer()), {
    "# claude · session-1",
    "Session: status=ready · display=Your turn · skills=inject",
    "ACP options:",
    "Telemetry:",
    "",
    "> [contexts: skill-index · file: init.lua · AGENTS.md]",
    "[context: skill-index]",
    "catalog line one",
    "catalog line two",
    "[context: file: init.lua]",
    "first line",
    "second line",
    "[context: AGENTS.md]",
    "type: resource_link",
    "name: AGENTS.md",
    "uri: file:///repo/AGENTS.md",
    "> Review this",
    "",
    "> ",
  })
  MiniTest.expect.equality(nvim.api.nvim_get_option_value("buftype", { buf = chat:buffer() }), "nofile")
  MiniTest.expect.equality(nvim.api.nvim_get_option_value("swapfile", { buf = chat:buffer() }), false)
  MiniTest.expect.equality(nvim.api.nvim_get_option_value("foldmethod", { win = 0 }), "manual")
  MiniTest.expect.equality({ fold_range(6) }, { 6, 16 })

  nvim.api.nvim_win_set_cursor(0, { 6, 0 })
  nvim.api.nvim_cmd({ cmd = "normal", args = { "zo" }, bang = true }, {})
  MiniTest.expect.equality(fold_range(6), -1)
  nvim.api.nvim_cmd({ cmd = "normal", args = { "zc" }, bang = true }, {})
  assert(chat:submit("Next"))
  MiniTest.expect.equality(created.prompts[2], "Next")
  chat:dispose()
end

T["chat"]["keeps failed context chips without creating a submitted fold"] = function()
  local first = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  assert(chat:queue_context({ label = "file", text = "file body" }))
  first.prompt_error = "write failed"

  local request_id = chat:submit("draft")

  MiniTest.expect.equality(request_id, nil)
  MiniTest.expect.equality(table.concat(buffer_lines(chat:buffer()), "\n"):find("[contexts:", 1, true), nil)
  MiniTest.expect.equality(fold_range(6), -1)
  MiniTest.expect.equality(buffer_lines(chat:buffer())[6], "> [context: file] draft")
  first.prompt_error = nil
  assert(chat:submit())
  MiniTest.expect.equality({ fold_range(6) }, { 6, 8 })
  chat:dispose()
end

T["chat"]["applies a queued context fold when its hidden session buffer is focused"] = function()
  local first = fake_session("session-1", "one")
  local second = fake_session("session-2", "two")
  first.state.status = "prompting"
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  assert(chat:queue_context({ label = "file", text = "queued body" }))
  assert(chat:submit("queued prompt"))
  assert(chat:attach(second))

  first.state.status = "ready"
  first:emit({ type = "turn_done", session_id = "session-1", data = {} })
  nvim.wait(100, function()
    return #first.prompts == 1
  end, 1)
  assert(chat:switch("session-1"))

  MiniTest.expect.equality(first.prompts[1], {
    { type = "text", text = "queued body" },
    { type = "text", text = "queued prompt" },
  })
  MiniTest.expect.equality({ fold_range(6) }, { 6, 8 })
  chat:dispose()
end

T["chat"]["positions the cursor after queued context, not before it"] = function()
  local first = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  assert(chat:queue_context({ label = "file: init.lua", text = "Referenced file: init.lua" }))

  local lines = buffer_lines(chat:buffer())
  local line = lines[#lines]
  local window = nvim.api.nvim_get_current_win()
  -- Headless tests never enter insert mode, so the cursor clamps to the last
  -- valid column; the point being proven is that it moved to line-end (past
  -- the queued "[context: ...]" chip), not that it stayed at the fixed
  -- column 2 the pre-fix code left it at regardless of chip length.
  MiniTest.expect.equality(nvim.api.nvim_win_get_cursor(window), { #lines, #line - 1 })

  chat:dispose()
end

T["chat"]["sends a queued resource-link context item as an ACP resource_link block"] = function()
  local first = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  assert(chat:queue_context({ label = "AGENTS.md", uri = "file:///repo/AGENTS.md" }))
  assert(chat:submit("hello"))

  MiniTest.expect.equality(first.prompts, {
    {
      { type = "resource_link", uri = "file:///repo/AGENTS.md", name = "AGENTS.md" },
      { type = "text", text = "hello" },
    },
  })
  chat:dispose()
end

T["chat"]["queues configured instructions context only for a brand-new session"] = function()
  local created_session = fake_session("session-1", "claude")
  local api = {
    create_session = function()
      return created_session
    end,
  }
  local chat = assert(Chat.new(api, {
    agents = { "claude" },
    instructions_context = { label = "AGENTS.md", uri = "file:///repo/AGENTS.md" },
  }))

  assert(chat:new_session("claude"))
  assert(chat:submit("hello"))

  MiniTest.expect.equality(created_session.prompts, {
    {
      { type = "resource_link", uri = "file:///repo/AGENTS.md", name = "AGENTS.md" },
      { type = "text", text = "hello" },
    },
  })
  chat:dispose()
end

T["chat"]["does not inject instructions context into an attached, already-existing session"] = function()
  local first = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api(), {
    instructions_context = { label = "AGENTS.md", uri = "file:///repo/AGENTS.md" },
  }))
  assert(chat:attach(first))
  assert(chat:submit("hello"))

  MiniTest.expect.equality(first.prompts, { "hello" })
  chat:dispose()
end

T["chat"]["renders state telemetry and reported-only usage"] = function()
  local first = fake_session("session-1", "claude")
  first.state.config_options = {
    { id = "model", name = "Model", category = "model", type = "select", current_value = "opus", options = {} },
    { id = "brave", name = "Brave", type = "boolean", current_value = true },
  }
  first.state.context = { used = 95, size = 100, percentage = 95, pressure = "critical", stale = false }
  first.state.cost = { amount = 1.5, currency = "USD" }
  local original_select = nvim.ui.select
  nvim.ui.select = function(_, _, callback)
    callback(nil)
  end
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  first.state.usage = { input_tokens = 12, cached_read_tokens = 3 }
  first:emit({ type = "turn_done", session_id = "session-1", data = { stopReason = "end_turn" } })
  nvim.wait(100, function()
    return #buffer_lines(chat:buffer()) == 7
  end, 1)
  nvim.ui.select = original_select

  MiniTest.expect.equality(
    buffer_lines(chat:buffer()),
    chat_lines(
      "claude · session-1",
      "status=ready · display=Your turn",
      { "", "[usage] input_tokens=12 · cached_read_tokens=3", "> " },
      "Model=opus · Brave=true",
      "context=95/100 (95%) · cost=1.5 USD"
    )
  )
  chat:dispose()
end

T["chat"]["groups session diagnostics and keeps telemetry in the window bar"] = function()
  local first = fake_session("session-1", "codex")
  first.state.acp_session_id = "acp-session-1"
  first.state.skills_policy = "native"
  first.state.context = { used = 148222, size = 258400, percentage = 57.361455, pressure = "normal", stale = false }
  first.state.cost = { amount = 1.5, currency = "USD" }
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  first.state.config_options = {
    { id = "mode", name = "Mode", type = "select", current_value = "agent", options = {} },
    { id = "fast", name = "Fast mode", type = "boolean", current_value = false },
  }
  first:emit({ type = "config_options_changed", session_id = "session-1", data = first.state.config_options })
  nvim.wait(100, function()
    return buffer_lines(chat:buffer())[3] == "ACP options: Mode=agent · Fast mode=false"
  end, 1)

  MiniTest.expect.equality(nvim.list_slice(buffer_lines(chat:buffer()), 1, 4), {
    "# codex/acp-session-1 · session-1",
    "Session: status=ready · display=Your turn · skills=native",
    "ACP options: Mode=agent · Fast mode=false",
    "Telemetry: context=148222/258400 (57%) · cost=1.5 USD",
  })
  MiniTest.expect.equality(
    nvim.api.nvim_get_option_value("winbar", { win = 0 }),
    "%#LouiselmStatusReady#Your turn%* · %#LouiselmAcpValue#context=148222/258400%* (%#LouiselmDerivedValue#57%%%*) · %#LouiselmAcpValue#cost=1.5 USD%*"
  )
  MiniTest.expect.equality(header_highlights(chat:buffer()), {
    { text = "display=Your turn", group = "LouiselmStatusReady" },
    { text = "Mode=agent", group = "LouiselmAcpValue" },
    { text = "Fast mode=false", group = "LouiselmAcpValue" },
    { text = "context=148222/258400", group = "LouiselmAcpValue" },
    { text = "57%", group = "LouiselmDerivedValue" },
    { text = "cost=1.5 USD", group = "LouiselmAcpValue" },
  })
  chat:dispose()
end

T["chat"]["omits unavailable telemetry and refreshes stale and cleared values"] = function()
  local first = fake_session("session-1", "codex")
  first.state.skills_policy = "inject"
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))

  MiniTest.expect.equality(
    buffer_lines(chat:buffer())[2],
    "Session: status=ready · display=Your turn · skills=inject"
  )
  MiniTest.expect.equality(buffer_lines(chat:buffer())[4], "Telemetry:")
  MiniTest.expect.equality(nvim.api.nvim_get_option_value("winbar", { win = 0 }), "%#LouiselmStatusReady#Your turn%*")

  first.state.context = { used = 50, size = 100, percentage = 50, pressure = "normal", stale = false }
  first.state.cost = { amount = 2, currency = "U%S\nX" }
  first:emit({ type = "usage_updated", session_id = "session-1", data = {} })
  nvim.wait(100, function()
    return buffer_lines(chat:buffer())[4] == "Telemetry: context=50/100 (50%) · cost=2 U%S X"
  end, 1)
  MiniTest.expect.equality(
    nvim.api.nvim_get_option_value("winbar", { win = 0 }),
    "%#LouiselmStatusReady#Your turn%* · %#LouiselmAcpValue#context=50/100%* (%#LouiselmDerivedValue#50%%%*) · %#LouiselmAcpValue#cost=2 U%%S X%*"
  )

  first.state.context.stale = true
  first:emit({ type = "config_options_changed", session_id = "session-1", data = {} })
  nvim.wait(100, function()
    return buffer_lines(chat:buffer())[4]:find("50%% stale", 1) ~= nil
  end, 1)
  MiniTest.expect.equality(buffer_lines(chat:buffer())[4], "Telemetry: context=50/100 (50% stale) · cost=2 U%S X")

  first.state.context = { used = 60, size = 100, percentage = 60, pressure = "normal", stale = false }
  first.state.cost = nil
  first:emit({ type = "usage_updated", session_id = "session-1", data = {} })
  nvim.wait(100, function()
    return buffer_lines(chat:buffer())[4] == "Telemetry: context=60/100 (60%)"
  end, 1)
  MiniTest.expect.equality(
    nvim.api.nvim_get_option_value("winbar", { win = 0 }),
    "%#LouiselmStatusReady#Your turn%* · %#LouiselmAcpValue#context=60/100%* (%#LouiselmDerivedValue#60%%%*)"
  )
  chat:dispose()
end

T["chat"]["uses semantic window bar highlights for every lifecycle state"] = function()
  local cases = {
    { status = "ready", label = "Your turn", group = "LouiselmStatusReady" },
    { status = "prompting", label = "Model responding", group = "LouiselmStatusActive" },
    { status = "waiting_permission", label = "Waiting for permission", group = "LouiselmStatusWarning" },
    { status = "cancelling", label = "Stopping", group = "LouiselmStatusWarning" },
    { status = "starting", label = "Starting", group = "LouiselmStatusActive" },
    { status = "configuring", label = "Starting", group = "LouiselmStatusActive" },
    { status = "error", label = "Error", group = "LouiselmStatusError" },
    { status = "disposed", label = "Unavailable", group = "LouiselmStatusWarning" },
  }
  for _, case in ipairs(cases) do
    local session = fake_session("session-" .. case.status, "codex")
    session.state.status = case.status
    local chat = assert(Chat.new(fake_api()))
    assert(chat:attach(session))
    MiniTest.expect.equality(
      nvim.api.nvim_get_option_value("winbar", { win = 0 }),
      "%#" .. case.group .. "#" .. case.label .. "%*"
    )
    chat:dispose()
  end
end

T["chat"]["preserves user provenance highlight overrides"] = function()
  nvim.api.nvim_set_hl(0, "LouiselmAcpValue", { fg = 0x123456 })
  MiniTest.finally(function()
    nvim.api.nvim_set_hl(0, "LouiselmAcpValue", { link = "Identifier" })
  end)

  local chat = assert(Chat.new(fake_api()))

  MiniTest.expect.equality(nvim.api.nvim_get_hl(0, { name = "LouiselmAcpValue" }).fg, 0x123456)
  chat:dispose()
end

T["chat"]["shows whose turn it is in the session header"] = function()
  local first = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))

  MiniTest.expect.equality(buffer_lines(chat:buffer())[2], "Session: status=ready · display=Your turn")

  first.state.status = "prompting"
  first:emit({
    type = "state_changed",
    session_id = "session-1",
    data = { status = "prompting" },
  })
  nvim.wait(100, function()
    return buffer_lines(chat:buffer())[2] == "Session: status=prompting · display=Model responding"
  end, 1)

  MiniTest.expect.equality(buffer_lines(chat:buffer())[2], "Session: status=prompting · display=Model responding")
  chat:dispose()
end

T["chat"]["keeps the turn label visible in the window bar"] = function()
  local first = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))

  MiniTest.expect.equality(nvim.api.nvim_get_option_value("winbar", { win = 0 }), "%#LouiselmStatusReady#Your turn%*")

  first.state.status = "prompting"
  first:emit({
    type = "state_changed",
    session_id = "session-1",
    data = { status = "prompting" },
  })
  nvim.wait(100, function()
    return nvim.api.nvim_get_option_value("winbar", { win = 0 }) == "%#LouiselmStatusActive#Model responding%*"
  end, 1)

  MiniTest.expect.equality(
    nvim.api.nvim_get_option_value("winbar", { win = 0 }),
    "%#LouiselmStatusActive#Model responding%*"
  )
  chat:dispose()
end

T["chat"]["opens the setup overview and applies a selected option"] = function()
  local first = fake_session("session-1", "claude")
  first.state.config_options = {
    {
      id = "model",
      name = "Model",
      type = "select",
      current_value = "small",
      options = { { value = "small", name = "Small" }, { value = "large", name = "Large" } },
    },
    { id = "brave", name = "Brave", type = "boolean", current_value = false },
  }
  local original_select = nvim.ui.select
  local original_stopinsert = nvim.cmd.stopinsert
  local calls = {}
  local mode = "i"
  local modes_at_select = {}
  nvim.cmd.stopinsert = function()
    mode = "n"
  end
  nvim.ui.select = function(items, options, callback)
    calls[#calls + 1] = { items = items, options = options }
    modes_at_select[#modes_at_select + 1] = mode
    mode = "i"
    if #calls == 1 then
      callback(items[1])
    elseif #calls == 2 then
      callback(items[2])
    else
      callback(nil)
    end
  end
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  nvim.wait(100, function()
    return #calls == 3
  end, 1)
  nvim.ui.select = original_select
  nvim.cmd.stopinsert = original_stopinsert

  MiniTest.expect.equality(calls[1].options.prompt, "louiselm session options: ")
  MiniTest.expect.equality(calls[1].options.format_item(calls[1].items[1]), "Model: small")
  MiniTest.expect.equality(modes_at_select, { "n", "n", "n" })
  MiniTest.expect.equality(first.config_changes, { { id = "model", value = "large" } })
  chat:dispose()
end

T["chat"]["ignores a queued setup overview after disposal"] = function()
  local first = fake_session("session-1", "claude")
  first.state.config_options = {
    { id = "brave", name = "Brave", type = "boolean", current_value = false },
  }
  local original_schedule = nvim.schedule
  local original_select = nvim.ui.select
  local scheduled = {}
  local selects = 0
  nvim.schedule = function(callback)
    scheduled[#scheduled + 1] = callback
  end
  nvim.ui.select = function()
    selects = selects + 1
  end
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  chat:dispose()
  scheduled[1]()
  nvim.schedule = original_schedule
  nvim.ui.select = original_select

  MiniTest.expect.equality(selects, 0)
end

T["chat"]["switches with telemetry rows and closes only the selected session"] = function()
  local first = fake_session("session-1", "one")
  first.state.acp_session_id = "one-acp"
  local second = fake_session("session-2", "two")
  second.state.status = "prompting"
  local api = fake_api()
  api.list_sessions = function()
    return { "session-1", "session-2" }
  end
  api.get_session = function(_, id)
    return id == "session-1" and first or second
  end
  local chat = assert(Chat.new(api))
  assert(chat:attach(first))
  assert(chat:attach(second))
  local original_select = nvim.ui.select
  local prompts = {}
  nvim.ui.select = function(items, options, callback)
    prompts[#prompts + 1] = options
    callback(items[1])
  end

  assert(chat:switch_session())
  MiniTest.expect.equality(chat.current_id, "session-1")
  assert(chat:switch("session-2"))
  assert(chat:close_session())
  nvim.ui.select = original_select

  MiniTest.expect.equality(prompts[1].prompt, "louiselm session: ")
  MiniTest.expect.equality(prompts[1].format_item(first), "one/one-acp · session-1 · ready")
  MiniTest.expect.equality(prompts[2].prompt, "close active louiselm session? ")
  MiniTest.expect.equality(second.disposed, true)
  MiniTest.expect.equality(first.disposed, false)
  MiniTest.expect.equality(chat:buffer("session-2"), nil)
  chat:dispose()
end

T["chat"]["renames the current session and refreshes its header"] = function()
  local first = fake_session("session-1", "claude")
  first.state.name = "First"
  function first:set_name(name)
    self.state.name = name
    self:emit({ type = "state_changed", session_id = self.state.id, data = { status = self.state.status } })
    return true
  end
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))

  assert(chat:rename_session("Review"))
  nvim.wait(100, function()
    return nvim.api.nvim_buf_get_lines(chat:buffer(), 0, 1, false)[1]:find("# claude · Review", 1, true) ~= nil
  end, 1)
  MiniTest.expect.equality(
    nvim.api.nvim_buf_get_lines(chat:buffer(), 0, 1, false)[1],
    "# claude · Review · session-1"
  )
  chat:dispose()
end

T["chat"]["mentions the source buffer rather than the chat scratch buffer"] = function()
  local source = nvim.api.nvim_create_buf(false, true)
  nvim.api.nvim_buf_set_name(source, "/tmp/source.lua")
  nvim.api.nvim_set_current_buf(source)
  local first = fake_session("session-1", "claude")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))

  assert(chat:mention_buffer())
  assert(chat:submit("Review"))

  MiniTest.expect.equality(first.prompts, {
    {
      { type = "text", text = "Current buffer: /tmp/source.lua" },
      { type = "text", text = "Review" },
    },
  })
  chat:dispose()
  nvim.api.nvim_buf_delete(source, { force = true })
end

T["chat"]["switches between attached session buffers"] = function()
  local first = fake_session("session-1", "one")
  local second = fake_session("session-2", "two")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))
  assert(chat:attach(second))

  assert(chat:switch("session-1"))
  MiniTest.expect.equality(nvim.api.nvim_get_current_buf(), chat:buffer("session-1"))
  assert(chat:switch("session-2"))
  MiniTest.expect.equality(nvim.api.nvim_get_current_buf(), chat:buffer("session-2"))

  chat:dispose()
end

T["chat"]["uses the agent picker for a new session"] = function()
  local created
  local session = fake_session("session-1", "two")
  local api = {
    create_session = function(_, agent_name)
      created = agent_name
      return session
    end,
  }
  local chat = assert(Chat.new(api, { agents = { "one", "two" } }))
  local original_select = nvim.ui.select
  nvim.ui.select = function(items, _, callback)
    MiniTest.expect.equality(items, { "one", "two" })
    callback("two")
  end

  local selected = chat:new_session()

  nvim.ui.select = original_select
  MiniTest.expect.equality(selected, nil)
  MiniTest.expect.equality(created, "two")
  MiniTest.expect.equality(chat:buffer("session-1") ~= nil, true)
  chat:dispose()
end

T["chat"]["uses a distinct filetype, not the literal markdown third parties key off"] = function()
  local first = fake_session("session-1", "one")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))

  local buffer = chat:buffer("session-1")
  MiniTest.expect.equality(nvim.bo[buffer].filetype == "markdown", false)

  chat:dispose()
end

T["chat"]["keeps markdown treesitter highlighting despite the distinct filetype"] = function()
  local first = fake_session("session-1", "one")
  local chat = assert(Chat.new(fake_api()))
  assert(chat:attach(first))

  local buffer = chat:buffer("session-1")
  MiniTest.expect.equality(nvim.treesitter.highlighter.active[buffer] ~= nil, true)

  chat:dispose()
end

return T
