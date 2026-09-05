local MiniTest = require("mini.test")

local Sources = require("louiselm.provenance.sources")

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim
local T = MiniTest.new_set()

local function fake_runtime()
  local original_system = nvim.system
  local original_schedule = nvim.schedule
  local processes = {}
  local scheduled = {}
  rawset(nvim, "system", function(command, options, callback)
    local process = { command = command, options = options, callback = callback }
    processes[#processes + 1] = process
    return process
  end)
  rawset(nvim, "schedule", function(callback)
    scheduled[#scheduled + 1] = callback
  end)
  return {
    processes = processes,
    scheduled = scheduled,
    restore = function()
      rawset(nvim, "system", original_system)
      rawset(nvim, "schedule", original_schedule)
    end,
  }
end

T["git log"] = MiniTest.new_set()

T["git log"]["uses an argument array and schedules completion"] = function()
  local runtime = fake_runtime()
  local ok, error_message = pcall(function()
    local completed
    local started, start_error = Sources.git_log("/repo", "HEAD~2..HEAD", function(commits, callback_error)
      completed = { commits = commits, error_value = callback_error }
    end)
    MiniTest.expect.equality(started, true)
    MiniTest.expect.equality(start_error, nil)
    MiniTest.expect.equality(runtime.processes[1].command, {
      "git",
      "log",
      "--no-decorate",
      "--no-color",
      "--format=%H%x00%B%x00%x1e",
      "HEAD~2..HEAD",
    })
    MiniTest.expect.equality(runtime.processes[1].options.cwd, "/repo")
    MiniTest.expect.equality(completed, nil)

    runtime.processes[1].callback({
      code = 0,
      stdout = "deadbeef" .. string.char(0) .. "chore: test\n" .. string.char(0) .. string.char(30),
      stderr = "",
    })
    MiniTest.expect.equality(completed, nil)
    MiniTest.expect.equality(#runtime.scheduled, 1)
    runtime.scheduled[1]()
    MiniTest.expect.equality(completed.error_value, nil)
    MiniTest.expect.equality(completed.commits[1], { id = "deadbeef", message = "chore: test\n" })
  end)
  runtime.restore()
  assert(ok, error_message)
end

T["git log"]["reports process failure as structured callback error"] = function()
  local runtime = fake_runtime()
  local ok, error_message = pcall(function()
    local completed
    assert(Sources.git_log("/repo", "HEAD", function(commits, callback_error)
      completed = { commits = commits, error_value = callback_error }
    end))
    runtime.processes[1].callback({ code = 128, stdout = "", stderr = "not a repository\n" })
    runtime.scheduled[1]()
    MiniTest.expect.equality(completed.commits, nil)
    MiniTest.expect.equality(completed.error_value.code, "git_failed")
    MiniTest.expect.equality(completed.error_value.exit_code, 128)
    MiniTest.expect.equality(completed.error_value.detail, "not a repository")
  end)
  runtime.restore()
  assert(ok, error_message)
end

T["git log"]["schedules real process completion onto the main loop"] = function()
  local temp_dir = nvim.fn.tempname()
  assert(nvim.fn.mkdir(temp_dir, "p") == 1)
  local ok, error_message = pcall(function()
    local completed
    -- A local non-repository exercises process failure without history or network.
    assert(Sources.git_log(temp_dir, "HEAD", function(commits, callback_error)
      completed = { commits = commits, error_value = callback_error, fast = nvim.in_fast_event() }
    end))
    MiniTest.expect.equality(
      nvim.wait(1000, function()
        return completed ~= nil
      end),
      true
    )
    MiniTest.expect.equality(completed.fast, false)
    MiniTest.expect.equality(completed.commits, nil)
    MiniTest.expect.equality(completed.error_value.code, "git_failed")
  end)
  nvim.fn.delete(temp_dir, "rf")
  assert(ok, error_message)
end

-- Captured from `git log -s --format=fuller` and `git log --format=%H%x00%B%x00%x1e`
-- against 097bcd6d2f5c6cb021a0ff31f78280d9c02a3d5e in this repository
-- (louiselm-wi0y). Git appends its own newline after every formatted record,
-- including the final one, so the record separator is followed by `\n` here.
T["git log"]["parses a single commit whose record separator is followed by git's trailing newline"] = function()
  local output = "097bcd6d2f5c6cb021a0ff31f78280d9c02a3d5e"
    .. string.char(0)
    .. "fix(provenance): example commit\n"
    .. string.char(0)
    .. string.char(30)
    .. "\n"
  local commits, parse_error = Sources.parse_git_log(output)
  MiniTest.expect.equality(parse_error, nil)
  MiniTest.expect.equality(commits, {
    { id = "097bcd6d2f5c6cb021a0ff31f78280d9c02a3d5e", message = "fix(provenance): example commit\n" },
  })
end

T["git log"]["parses multiple commits when every record separator is followed by git's trailing newline"] = function()
  local output = table.concat({
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
      .. string.char(0)
      .. "first\n"
      .. string.char(0)
      .. string.char(30)
      .. "\n",
    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
      .. string.char(0)
      .. "second\n"
      .. string.char(0)
      .. string.char(30)
      .. "\n",
  })
  local commits, parse_error = Sources.parse_git_log(output)
  MiniTest.expect.equality(parse_error, nil)
  MiniTest.expect.equality(commits, {
    { id = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", message = "first\n" },
    { id = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", message = "second\n" },
  })
end

T["git log"]["rejects missing inputs before spawning"] = function()
  local runtime = fake_runtime()
  local ok, error_message = pcall(function()
    local started, start_error = Sources.git_log("", "HEAD", function() end)
    MiniTest.expect.equality(started, false)
    assert(start_error ~= nil)
    MiniTest.expect.equality(start_error.code, "invalid_cwd")
    MiniTest.expect.equality(#runtime.processes, 0)
  end)
  runtime.restore()
  assert(ok, error_message)
end

T["bvr history"] = MiniTest.new_set()

T["bvr history"]["parses issue milestones and correlation metadata"] = function()
  local output = nvim.json.encode({
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
            short_sha = "close-s",
            message = "fix: spacing",
            author = "euri10",
            author_email = "benoit@example.com",
            timestamp = "2026-08-24T14:55:01+02:00",
            method = "co_committed",
            confidence = 0.95,
          },
        },
      },
    },
  })

  local history, error_value = Sources.parse_bvr_history(output, "louiselm-kpod")
  assert(error_value == nil)
  assert(history ~= nil)
  MiniTest.expect.equality(history.title, "Usage spacing")
  MiniTest.expect.equality(history.status, "closed")
  MiniTest.expect.equality(history.milestones.created.commit_sha, "create-sha")
  MiniTest.expect.equality(history.commits[1].method, "co_committed")
  MiniTest.expect.equality(history.commits[1].confidence, 0.95)
end

T["beads issue"] = MiniTest.new_set()
T["beads issues"] = MiniTest.new_set()

T["beads issue"]["parses actor fields from br"] = function()
  local issue, error_value = Sources.parse_beads_issue(
    '[{"id":"louiselm-kpod","assignee":"codex/session-1","created_by":"assistant"}]',
    "louiselm-kpod"
  )
  assert(error_value == nil)
  assert(issue ~= nil)
  MiniTest.expect.equality(issue, {
    id = "louiselm-kpod",
    assignee = "codex/session-1",
    created_by = "assistant",
  })
end

T["beads issue"]["preserves an empty assignee"] = function()
  local issue, error_value =
    Sources.parse_beads_issue('[{"id":"louiselm-kpod","assignee":"","created_by":"lotso"}]', "louiselm-kpod")
  assert(error_value == nil)
  assert(issue ~= nil)
  MiniTest.expect.equality(issue.assignee, "")
end

T["beads issue"]["uses br with an argument array and schedules completion"] = function()
  local runtime = fake_runtime()
  local ok, error_message = pcall(function()
    local completed
    local started, start_error = Sources.beads_issue("/repo", "louiselm-kpod", function(issue, callback_error)
      completed = { issue = issue, error_value = callback_error }
    end)
    MiniTest.expect.equality(started, true)
    MiniTest.expect.equality(start_error, nil)
    MiniTest.expect.equality(runtime.processes[1].command, { "br", "show", "louiselm-kpod", "--json" })
    MiniTest.expect.equality(runtime.processes[1].options.cwd, "/repo")

    runtime.processes[1].callback({
      code = 0,
      stdout = '[{"id":"louiselm-kpod","assignee":"","created_by":"lotso"}]',
      stderr = "",
    })
    MiniTest.expect.equality(completed, nil)
    MiniTest.expect.equality(#runtime.scheduled, 1)
    runtime.scheduled[1]()
    MiniTest.expect.equality(completed.error_value, nil)
    MiniTest.expect.equality(completed.issue.created_by, "lotso")
  end)
  runtime.restore()
  assert(ok, error_message)
end

T["beads issues"]["parses actor fields from the br list response"] = function()
  local issues, error_value = Sources.parse_beads_issues(
    '{"issues":[{"id":"louiselm-one","assignee":"codex/session-1","created_by":"assistant"},{"id":"louiselm-two","assignee":"","created_by":"lotso"}]} '
  )
  assert(error_value == nil)
  MiniTest.expect.equality(issues, {
    { id = "louiselm-one", assignee = "codex/session-1", created_by = "assistant" },
    { id = "louiselm-two", assignee = "", created_by = "lotso" },
  })
end

T["beads issues"]["uses br list with an argument array and schedules completion"] = function()
  local runtime = fake_runtime()
  local ok, error_message = pcall(function()
    local completed
    local started, start_error = Sources.beads_issues("/repo", function(issues, callback_error)
      completed = { issues = issues, error_value = callback_error }
    end)
    MiniTest.expect.equality(started, true)
    MiniTest.expect.equality(start_error, nil)
    MiniTest.expect.equality(runtime.processes[1].command, { "br", "list", "--status", "all", "--json" })
    MiniTest.expect.equality(runtime.processes[1].options.cwd, "/repo")

    runtime.processes[1].callback({
      code = 0,
      stdout = '{"issues":[{"id":"louiselm-one","assignee":"","created_by":"lotso"}]}',
      stderr = "",
    })
    MiniTest.expect.equality(completed, nil)
    MiniTest.expect.equality(#runtime.scheduled, 1)
    runtime.scheduled[1]()
    MiniTest.expect.equality(completed.error_value, nil)
    MiniTest.expect.equality(completed.issues[1].created_by, "lotso")
  end)
  runtime.restore()
  assert(ok, error_message)
end

T["beads questions"] = MiniTest.new_set()

T["beads questions"]["parses Decision-relevant fields from the br list response"] = function()
  local issues, error_value = Sources.parse_beads_questions(
    '{"issues":[{"id":"louiselm-old","issue_type":"question","title":"Old","status":"closed","close_reason":"Resolved","updated_at":"2026-08-20T00:00:00Z","description":"Decision relation: supersedes louiselm-x"}]}'
  )
  assert(error_value == nil)
  MiniTest.expect.equality(issues, {
    {
      id = "louiselm-old",
      issue_type = "question",
      title = "Old",
      status = "closed",
      close_reason = "Resolved",
      updated_at = "2026-08-20T00:00:00Z",
      description = "Decision relation: supersedes louiselm-x",
    },
  })
end

T["beads questions"]["uses br list scoped to question issues and schedules completion"] = function()
  local runtime = fake_runtime()
  local ok, error_message = pcall(function()
    local completed
    local started, start_error = Sources.beads_questions("/repo", function(issues, callback_error)
      completed = { issues = issues, error_value = callback_error }
    end)
    MiniTest.expect.equality(started, true)
    MiniTest.expect.equality(start_error, nil)
    MiniTest.expect.equality(
      runtime.processes[1].command,
      { "br", "list", "--type", "question", "--status", "all", "--json" }
    )
    MiniTest.expect.equality(runtime.processes[1].options.cwd, "/repo")

    runtime.processes[1].callback({
      code = 0,
      stdout = '{"issues":[{"id":"louiselm-one","issue_type":"question","status":"open"}]}',
      stderr = "",
    })
    MiniTest.expect.equality(completed, nil)
    MiniTest.expect.equality(#runtime.scheduled, 1)
    runtime.scheduled[1]()
    MiniTest.expect.equality(completed.error_value, nil)
    MiniTest.expect.equality(completed.issues[1].status, "open")
  end)
  runtime.restore()
  assert(ok, error_message)
end

T["bvr history"]["returns an empty history when the issue is outside the range"] = function()
  local history, error_value = Sources.parse_bvr_history('{"histories":{}}', "louiselm-kpod")
  assert(error_value == nil)
  MiniTest.expect.equality(history, { bead_id = "louiselm-kpod", commits = {}, milestones = {} })
end

T["bvr history"]["uses an explicit history range and schedules completion"] = function()
  local runtime = fake_runtime()
  local ok, error_message = pcall(function()
    local completed
    local started, start_error = Sources.bvr_history("/repo", "louiselm-kpod", function(history, callback_error)
      completed = { history = history, error_value = callback_error }
    end)
    MiniTest.expect.equality(started, true)
    MiniTest.expect.equality(start_error, nil)
    MiniTest.expect.equality(runtime.processes[1].command, {
      "bvr",
      "--robot-history",
      "--bead-history",
      "louiselm-kpod",
      "--history-since",
      "2026-08-10",
      "--history-limit",
      "500",
    })
    MiniTest.expect.equality(runtime.processes[1].options.cwd, "/repo")
    MiniTest.expect.equality(completed, nil)

    runtime.processes[1].callback({ code = 0, stdout = '{"histories":{}}', stderr = "" })
    MiniTest.expect.equality(#runtime.scheduled, 1)
    runtime.scheduled[1]()
    MiniTest.expect.equality(completed.error_value, nil)
    MiniTest.expect.equality(completed.history.bead_id, "louiselm-kpod")
  end)
  runtime.restore()
  assert(ok, error_message)
end

T["bvr history"]["rejects malformed output"] = function()
  local history, error_value = Sources.parse_bvr_history("not json", "louiselm-kpod")
  MiniTest.expect.equality(history, nil)
  assert(error_value ~= nil)
  MiniTest.expect.equality(error_value.code, "invalid_bvr_history")
end

return T
