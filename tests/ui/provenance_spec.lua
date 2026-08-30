local MiniTest = require("mini.test")
local Provenance = require("louiselm.ui.provenance")

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim
local original_schedule = nvim.schedule
local original_system = nvim.system
local source_buffers = {}

local T = MiniTest.new_set({
  hooks = {
    post_case = function()
      rawset(nvim, "schedule", original_schedule)
      rawset(nvim, "system", original_system)
      for _, buffer in ipairs(nvim.api.nvim_list_bufs()) do
        if nvim.api.nvim_buf_get_name(buffer):match("^louiselm://provenance/") then
          nvim.api.nvim_buf_delete(buffer, { force = true })
        end
      end
      for _, buffer in ipairs(source_buffers) do
        if nvim.api.nvim_buf_is_valid(buffer) then
          nvim.api.nvim_buf_delete(buffer, { force = true })
        end
      end
      source_buffers = {}
    end,
  },
})

---@param line string
---@param column integer
---@return integer buffer
local function source_buffer(line, column)
  local buffer = nvim.api.nvim_create_buf(false, true)
  source_buffers[#source_buffers + 1] = buffer
  nvim.api.nvim_buf_set_lines(buffer, 0, -1, false, { line })
  nvim.api.nvim_set_current_buf(buffer)
  nvim.api.nvim_win_set_cursor(0, { 1, column })
  return buffer
end

---@return table[] calls
local function fake_system()
  local calls = {}
  rawset(nvim, "system", function(command, options, on_exit)
    calls[#calls + 1] = { command = command, options = options, on_exit = on_exit }
    return {}
  end)
  return calls
end

---@param calls table[]
---@param message string
local function finish(calls, message)
  local scheduled = {}
  rawset(nvim, "schedule", function(callback)
    scheduled[#scheduled + 1] = callback
  end)
  calls[1].on_exit({
    code = 0,
    stdout = "0123456789abcdef0123456789abcdef01234567" .. string.char(0) .. message .. string.char(0) .. string.char(
      30
    ),
    stderr = "",
  })
  MiniTest.expect.equality(#scheduled, 1)
  scheduled[1]()
end

---@param calls table[]
---@param output string
local function finish_bvr(calls, output)
  local scheduled = {}
  rawset(nvim, "schedule", function(callback)
    scheduled[#scheduled + 1] = callback
  end)
  calls[1].on_exit({ code = 0, stdout = output, stderr = "" })
  MiniTest.expect.equality(#scheduled, 1)
  scheduled[1]()
end

local function finish_br(calls, output)
  local scheduled = {}
  rawset(nvim, "schedule", function(callback)
    scheduled[#scheduled + 1] = callback
  end)
  calls[2].on_exit({ code = 0, stdout = output, stderr = "" })
  MiniTest.expect.equality(#scheduled, 1)
  scheduled[1]()
end

local function finish_beads_issues(calls, output)
  local scheduled = {}
  rawset(nvim, "schedule", function(callback)
    scheduled[#scheduled + 1] = callback
  end)
  calls[2].on_exit({ code = 0, stdout = output, stderr = "" })
  MiniTest.expect.equality(#scheduled, 1)
  scheduled[1]()
end

---@param calls table[]
---@param index integer
---@param output string
local function finish_call(calls, index, output)
  local scheduled = {}
  rawset(nvim, "schedule", function(callback)
    scheduled[#scheduled + 1] = callback
  end)
  calls[index].on_exit({ code = 0, stdout = output, stderr = "" })
  MiniTest.expect.equality(#scheduled, 1)
  scheduled[1]()
end

T["commit Provenance"] = MiniTest.new_set()

T["commit Provenance"]["renders commit references and unresolved Sessions"] = function()
  local buffer = source_buffer("commit 0123456789abcdef0123456789abcdef01234567", 10)
  local calls = fake_system()

  assert(Provenance.inspect(buffer, { definitions = {} }))
  MiniTest.expect.equality(calls[1].command, {
    "git",
    "log",
    "--no-decorate",
    "--no-color",
    "--format=%H%x00%B%x00%x1e",
    "0123456789abcdef0123456789abcdef01234567^..0123456789abcdef0123456789abcdef01234567",
  })
  finish(calls, "fix: preserve intent\n\nRefs louiselm-kpod\nRefs codex/session-123\n")

  local popup = nvim.api.nvim_get_current_buf()
  MiniTest.expect.equality(
    nvim.api.nvim_buf_get_name(popup),
    "louiselm://provenance/commit/0123456789abcdef0123456789abcdef01234567"
  )
  MiniTest.expect.equality(nvim.api.nvim_buf_get_lines(popup, 0, -1, false), {
    "# fix: preserve intent",
    "",
    "Commit: 0123456789abcdef0123456789abcdef01234567",
    "",
    "Issues served:",
    "- louiselm-kpod",
    "",
    "Sessions:",
    "- codex/session-123 → unresolved (transcript not found for Session 'codex/session-123' in supported layouts: claude, codex, copilot, openai-compatible)",
  })
  MiniTest.expect.equality(nvim.api.nvim_get_option_value("modifiable", { buf = popup }), false)
end

T["commit Provenance"]["renders a partial answer when the commit has no trailers"] = function()
  local buffer = source_buffer("0123456789abcdef0123456789abcdef01234567", 10)
  local calls = fake_system()
  assert(Provenance.inspect(buffer))
  finish(calls, "chore: tidy history\n")

  MiniTest.expect.equality(nvim.api.nvim_buf_get_lines(nvim.api.nvim_get_current_buf(), 0, -1, false), {
    "# chore: tidy history",
    "",
    "Commit: 0123456789abcdef0123456789abcdef01234567",
    "",
    "Issues served:",
    "- none recorded",
    "",
    "Sessions:",
    "- none recorded",
  })
end

T["commit Provenance"]["ignores a scheduled result after the view becomes inactive"] = function()
  local buffer = source_buffer("0123456789abcdef0123456789abcdef01234567", 10)
  local calls = fake_system()
  local scheduled = {}
  rawset(nvim, "schedule", function(callback)
    scheduled[#scheduled + 1] = callback
  end)

  assert(Provenance.inspect(buffer, {
    is_active = function()
      return false
    end,
  }))
  calls[1].on_exit({ code = 0, stdout = "", stderr = "" })
  MiniTest.expect.equality(#scheduled, 1)
  scheduled[1]()
  MiniTest.expect.equality(#nvim.api.nvim_list_bufs(), #source_buffers + 1)
end

T["issue Provenance"] = MiniTest.new_set()

T["issue Provenance"]["renders bvr correlations and milestones"] = function()
  local buffer = source_buffer("Inspect louiselm-kpod", 10)
  local calls = fake_system()

  assert(Provenance.inspect(buffer))
  MiniTest.expect.equality(calls[1].command, {
    "bvr",
    "--robot-history",
    "--bead-history",
    "louiselm-kpod",
    "--history-since",
    "2026-08-10",
    "--history-limit",
    "500",
  })
  finish_bvr(
    calls,
    nvim.json.encode({
      histories = {
        ["louiselm-kpod"] = {
          bead_id = "louiselm-kpod",
          title = "Usage spacing",
          status = "closed",
          milestones = {
            created = { timestamp = "2026-08-24T05:30:21+02:00", commit_sha = "create-sha" },
            closed = { timestamp = "2026-08-24T14:55:01+02:00", commit_sha = "close-sha" },
          },
          commits = {
            {
              sha = "close-sha",
              method = "co_committed",
              confidence = 0.95,
              message = "fix: spacing\n\nRefs codex/session-1\n",
            },
          },
        },
      },
    })
  )
  MiniTest.expect.equality(calls[2].command, { "br", "show", "louiselm-kpod", "--json" })
  finish_br(calls, '[{"id":"louiselm-kpod","assignee":"codex/session-1","created_by":"assistant"}]')

  MiniTest.expect.equality(nvim.api.nvim_buf_get_lines(nvim.api.nvim_get_current_buf(), 0, -1, false), {
    "# Usage spacing",
    "",
    "ID: louiselm-kpod",
    "Status: closed",
    "",
    "Milestones:",
    "- created: 2026-08-24T05:30:21+02:00 (create-sha)",
    "- closed: 2026-08-24T14:55:01+02:00 (close-sha)",
    "",
    "Commits:",
    "- close-sha · co_committed · 95% confidence",
    "",
    "Sessions:",
    "- codex/session-1 → unresolved (transcript not found for Session 'codex/session-1' in supported layouts: claude, codex, copilot, openai-compatible)",
    "",
    "Actors:",
    "- assistant (non-Session actor)",
  })
  MiniTest.expect.equality(
    nvim.api.nvim_buf_get_name(nvim.api.nvim_get_current_buf()),
    "louiselm://provenance/issue/louiselm-kpod"
  )
end

T["issue Provenance"]["renders an issue with no correlated commits"] = function()
  local buffer = source_buffer("louiselm-kpod", 5)
  local calls = fake_system()
  assert(Provenance.inspect(buffer))
  finish_bvr(calls, '{"histories":{}}')
  finish_br(calls, '[{"id":"louiselm-kpod","assignee":"","created_by":"lotso"}]')

  MiniTest.expect.equality(nvim.api.nvim_buf_get_lines(nvim.api.nvim_get_current_buf(), 0, -1, false), {
    "# louiselm-kpod",
    "",
    "ID: louiselm-kpod",
    "Status: unknown",
    "",
    "Milestones:",
    "- none recorded",
    "",
    "Commits:",
    "- none correlated",
    "",
    "Sessions:",
    "- none recorded",
    "",
    "Actors:",
    "- lotso (non-Session actor)",
  })
end

T["Session Provenance"] = MiniTest.new_set()

T["Session Provenance"]["renders reverse commit and issue edges"] = function()
  local buffer = source_buffer("Session codex/session-1", 10)
  local calls = fake_system()

  assert(Provenance.inspect(buffer, { definitions = {} }))
  MiniTest.expect.equality(calls[1].command, {
    "git",
    "log",
    "--no-decorate",
    "--no-color",
    "--format=%H%x00%B%x00%x1e",
    "--all",
  })
  finish(calls, "feat: work\n\nRefs codex/session-1\n")
  MiniTest.expect.equality(calls[2].command, { "br", "list", "--status", "all", "--json" })
  finish_beads_issues(
    calls,
    '{"issues":[{"id":"louiselm-kpod","assignee":"codex/session-1","created_by":"assistant"}]}'
  )

  MiniTest.expect.equality(nvim.api.nvim_buf_get_lines(nvim.api.nvim_get_current_buf(), 0, -1, false), {
    "# Session codex/session-1",
    "",
    "ID: codex/session-1",
    "",
    "Transcript:",
    "- unresolved (transcript not found for Session 'codex/session-1' in supported layouts: claude, codex, copilot, openai-compatible)",
    "",
    "Commits:",
    "- 0123456789abcdef0123456789abcdef01234567",
    "",
    "Issues:",
    "- louiselm-kpod",
  })
  MiniTest.expect.equality(
    nvim.api.nvim_buf_get_name(nvim.api.nvim_get_current_buf()),
    "louiselm://provenance/session/codex/session-1"
  )
end

T["Session Provenance"]["renders empty sides for a Session with no work"] = function()
  local buffer = source_buffer("codex/session-1", 5)
  local calls = fake_system()
  assert(Provenance.inspect(buffer, { definitions = {} }))
  finish(calls, "chore: unrelated\n")
  finish_beads_issues(calls, '{"issues":[]}')

  MiniTest.expect.equality(nvim.api.nvim_buf_get_lines(nvim.api.nvim_get_current_buf(), 0, -1, false), {
    "# Session codex/session-1",
    "",
    "ID: codex/session-1",
    "",
    "Transcript:",
    "- unresolved (transcript not found for Session 'codex/session-1' in supported layouts: claude, codex, copilot, openai-compatible)",
    "",
    "Commits:",
    "- none recorded",
    "",
    "Issues:",
    "- none recorded",
  })
end

T["Decision index"] = MiniTest.new_set()

---@param calls table[]
---@param output string
local function finish_beads_questions(calls, output)
  local scheduled = {}
  rawset(nvim, "schedule", function(callback)
    scheduled[#scheduled + 1] = callback
  end)
  calls[1].on_exit({ code = 0, stdout = output, stderr = "" })
  MiniTest.expect.equality(#scheduled, 1)
  scheduled[1]()
end

T["Decision index"]["renders Decision anchors with state, sorted by recent activity"] = function()
  local calls = fake_system()
  assert(Provenance.show_decisions({ cwd = "/repo" }))
  MiniTest.expect.equality(calls[1].command, { "br", "list", "--type", "question", "--status", "all", "--json" })
  finish_beads_questions(
    calls,
    nvim.json.encode({
      issues = {
        {
          id = "louiselm-old",
          issue_type = "question",
          title = "Old decision",
          status = "closed",
          close_reason = "Resolved",
          updated_at = "2026-08-20T00:00:00Z",
        },
        {
          id = "louiselm-new",
          issue_type = "question",
          title = "New decision",
          status = "open",
          updated_at = "2026-08-26T00:00:00Z",
        },
      },
    })
  )

  local popup = nvim.api.nvim_get_current_buf()
  MiniTest.expect.equality(nvim.api.nvim_buf_get_name(popup), "louiselm://provenance/decisions")
  MiniTest.expect.equality(nvim.api.nvim_buf_get_lines(popup, 0, -1, false), {
    "# Provenance Decisions",
    "",
    "- [open] New decision (louiselm-new) · 2026-08-26T00:00:00Z",
    "- [accepted] Old decision (louiselm-old) · 2026-08-20T00:00:00Z",
  })
  MiniTest.expect.equality(nvim.api.nvim_get_option_value("modifiable", { buf = popup }), false)
end

T["Decision index"]["renders a legible empty state with no Decision anchors"] = function()
  local calls = fake_system()
  assert(Provenance.show_decisions({}))
  finish_beads_questions(calls, '{"issues":[]}')

  MiniTest.expect.equality(nvim.api.nvim_buf_get_lines(nvim.api.nvim_get_current_buf(), 0, -1, false), {
    "# Provenance Decisions",
    "",
    "No Decision anchors found.",
  })
end

T["Decision index"]["reuses an existing named buffer"] = function()
  local calls = fake_system()
  local existing_buffer = nvim.api.nvim_create_buf(false, true)
  nvim.api.nvim_buf_set_name(existing_buffer, "louiselm://provenance/decisions")
  assert(Provenance.show_decisions({}))
  finish_beads_questions(calls, '{"issues":[]}')

  MiniTest.expect.equality(nvim.api.nvim_get_current_buf(), existing_buffer)
end

T["Decision index"]["navigates a selected row to its issue Provenance"] = function()
  local calls = fake_system()
  assert(Provenance.show_decisions({}))
  finish_beads_questions(
    calls,
    nvim.json.encode({
      issues = {
        { id = "louiselm-old", issue_type = "question", title = "Old decision", status = "open" },
      },
    })
  )

  nvim.api.nvim_win_set_cursor(0, { 3, 0 })
  nvim.api.nvim_feedkeys(nvim.api.nvim_replace_termcodes("<CR>", true, false, true), "mx", false)

  MiniTest.expect.equality(
    nvim.api.nvim_buf_get_name(nvim.api.nvim_get_current_buf()),
    "louiselm://provenance/decision/louiselm-old"
  )
  MiniTest.expect.equality(nvim.api.nvim_buf_get_lines(nvim.api.nvim_get_current_buf(), 0, -1, false), {
    "# Decision louiselm-old",
    "",
    "Loading Provenance evidence...",
  })

  MiniTest.expect.equality(calls[2].command, {
    "bvr",
    "--robot-history",
    "--bead-history",
    "louiselm-old",
    "--history-since",
    "2026-08-10",
    "--history-limit",
    "500",
  })
end

T["Decision index"]["resizes an existing window after loading evidence"] = function()
  local calls = fake_system()
  assert(Provenance.show_decisions({}))
  finish_beads_questions(
    calls,
    nvim.json.encode({
      issues = {
        { id = "louiselm-decision", issue_type = "question", title = "Evidence boundary", status = "closed" },
      },
    })
  )

  nvim.api.nvim_win_set_cursor(0, { 3, 0 })
  nvim.api.nvim_feedkeys(nvim.api.nvim_replace_termcodes("<CR>", true, false, true), "mx", false)
  local window = nvim.api.nvim_get_current_win()
  local loading_height = nvim.api.nvim_win_get_height(window)

  finish_call(
    calls,
    2,
    nvim.json.encode({
      histories = {
        ["louiselm-decision"] = {
          bead_id = "louiselm-decision",
          status = "closed",
          milestones = {},
          commits = {
            { sha = "abc123456789", method = "explicit_id", confidence = 1 },
          },
        },
      },
    })
  )
  finish_call(
    calls,
    3,
    nvim.json.encode({
      {
        id = "louiselm-decision",
        issue_type = "question",
        title = "Evidence boundary",
        status = "closed",
        close_reason = "Resolved",
        comments = {},
      },
    })
  )

  MiniTest.expect.equality(nvim.api.nvim_get_current_win(), window)
  MiniTest.expect.no_equality(nvim.api.nvim_win_get_height(window), loading_height)
end

T["Decision index"]["opens a read-only evidence timeline with recorded and inferred paths"] = function()
  local calls = fake_system()
  assert(Provenance.show_decisions({ definitions = {} }))
  finish_beads_questions(
    calls,
    nvim.json.encode({
      issues = {
        { id = "louiselm-decision", issue_type = "question", title = "Evidence boundary", status = "closed" },
      },
    })
  )

  nvim.api.nvim_win_set_cursor(0, { 3, 0 })
  nvim.api.nvim_feedkeys(nvim.api.nvim_replace_termcodes("<CR>", true, false, true), "mx", false)
  finish_call(
    calls,
    2,
    nvim.json.encode({
      histories = {
        ["louiselm-decision"] = {
          bead_id = "louiselm-decision",
          status = "closed",
          milestones = {},
          commits = {
            {
              sha = "abc123456789",
              method = "explicit_id",
              confidence = 1,
              message = "feat: evidence boundary\n\nRefs codex/session-1\n",
            },
            { sha = "def5678", method = "co_committed", confidence = 0.75 },
          },
        },
      },
    })
  )
  finish_call(
    calls,
    3,
    nvim.json.encode({
      {
        id = "louiselm-decision",
        issue_type = "question",
        title = "Evidence boundary",
        status = "closed",
        close_reason = "Resolved",
        assignee = "codex/session-1",
        created_by = "lotso",
        comments = {
          {
            author = "lotso",
            text = "QA accepted: abc1234\nScope: louiselm-decision\nEvidence: forensics-1",
          },
        },
      },
    })
  )

  MiniTest.expect.equality(nvim.api.nvim_buf_get_lines(nvim.api.nvim_get_current_buf(), 0, -1, false), {
    "# Decision Evidence boundary",
    "",
    "ID: louiselm-decision",
    "State: accepted",
    "Status: closed",
    "",
    "Implementation:",
    "- abc123456789 · recorded · explicit_id · 100% confidence",
    "- def5678 · inferred · co_committed · 75% confidence",
    "",
    "Sessions:",
    "- codex/session-1 → unresolved (transcript not found for Session 'codex/session-1' in supported layouts: claude, codex, copilot, openai-compatible) · recorded · 100% confidence",
    "",
    "QA acceptance:",
    "- lotso · abc1234 · louiselm-decision · forensics-1",
    "",
    "Forensics:",
    "- forensics-1",
  })
  MiniTest.expect.equality(
    nvim.api.nvim_buf_get_name(nvim.api.nvim_get_current_buf()),
    "louiselm://provenance/decision/louiselm-decision"
  )
  MiniTest.expect.equality(
    nvim.api.nvim_get_option_value("modifiable", { buf = nvim.api.nvim_get_current_buf() }),
    false
  )
end

T["Decision index"]["keeps missing evidence explicit"] = function()
  local calls = fake_system()
  assert(Provenance.show_decisions({}))
  finish_beads_questions(
    calls,
    nvim.json.encode({
      issues = {
        { id = "louiselm-decision", issue_type = "question", title = "Unresolved", status = "open" },
      },
    })
  )
  nvim.api.nvim_win_set_cursor(0, { 3, 0 })
  nvim.api.nvim_feedkeys(nvim.api.nvim_replace_termcodes("<CR>", true, false, true), "mx", false)
  finish_call(calls, 2, '{"histories":{}}')
  finish_call(
    calls,
    3,
    '[{"id":"louiselm-decision","issue_type":"question","title":"Unresolved","status":"open","comments":[]}]'
  )

  local lines = nvim.api.nvim_buf_get_lines(nvim.api.nvim_get_current_buf(), 0, -1, false)
  MiniTest.expect.equality(lines[7], "Implementation:")
  MiniTest.expect.equality(lines[8], "- unresolved (no correlated commits)")
  MiniTest.expect.equality(lines[11], "- unresolved (no attributable Session links)")
  MiniTest.expect.equality(lines[14], "- unresolved (no attributable acceptance tied to a correlated commit)")
  MiniTest.expect.equality(lines[17], "- unresolved (no pointer recorded)")
  MiniTest.expect.equality(
    nvim.api.nvim_get_option_value("modifiable", { buf = nvim.api.nvim_get_current_buf() }),
    false
  )
end

T["Decision index"]["shows the backlog at a closed Decision and its current drift"] = function()
  local calls = fake_system()
  local root = nvim.fn.tempname()
  assert(nvim.fn.mkdir(nvim.fs.joinpath(root, ".beads"), "p") == 1)
  assert(nvim.fn.writefile({
    '{"id":"current","status":"open"}',
    '{"id":"louiselm-decision","status":"closed"}',
  }, nvim.fs.joinpath(root, ".beads", "issues.jsonl")) == 0)
  assert(Provenance.show_decisions({ cwd = root }))
  finish_beads_questions(
    calls,
    nvim.json.encode({
      issues = {
        { id = "louiselm-decision", issue_type = "question", title = "Closed", status = "closed" },
      },
    })
  )
  nvim.api.nvim_win_set_cursor(0, { 3, 0 })
  nvim.api.nvim_feedkeys(nvim.api.nvim_replace_termcodes("<CR>", true, false, true), "mx", false)
  finish_call(
    calls,
    2,
    nvim.json.encode({
      histories = {
        ["louiselm-decision"] = {
          bead_id = "louiselm-decision",
          status = "closed",
          milestones = { closed = { timestamp = "now", commit_sha = "close-sha" } },
          commits = {},
        },
      },
    })
  )
  finish_call(calls, 3, '[{"id":"louiselm-decision","issue_type":"question","status":"closed","comments":[]}]')
  MiniTest.expect.equality(calls[4].command, { "git", "show", "close-sha:.beads/issues.jsonl" })
  finish_call(
    calls,
    4,
    table.concat({
      '{"id":"historical","status":"custom"}',
      '{"id":"louiselm-decision","status":"custom"}',
      "",
    }, "\n")
  )

  local lines = nvim.api.nvim_buf_get_lines(nvim.api.nvim_get_current_buf(), 0, -1, false)
  MiniTest.expect.equality(lines[#lines - 3], "- ref: close-sha")
  MiniTest.expect.equality(lines[#lines - 2], "- 2 issues at decision time")
  MiniTest.expect.equality(lines[#lines - 1], "- anchor state: custom")
  MiniTest.expect.equality(lines[#lines], "- since then: 1 added, 1 removed, 1 changed")
  nvim.fn.delete(root, "rf")
end

T["Decision index"]["ignores a scheduled result after the view becomes inactive"] = function()
  local calls = fake_system()
  local scheduled = {}
  rawset(nvim, "schedule", function(callback)
    scheduled[#scheduled + 1] = callback
  end)
  assert(Provenance.show_decisions({
    is_active = function()
      return false
    end,
  }))
  calls[1].on_exit({ code = 0, stdout = '{"issues":[]}', stderr = "" })
  MiniTest.expect.equality(#scheduled, 1)
  scheduled[1]()
  for _, buffer in ipairs(nvim.api.nvim_list_bufs()) do
    assert(not nvim.api.nvim_buf_get_name(buffer):match("^louiselm://provenance/decisions"))
  end
end

return T
