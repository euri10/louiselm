local MiniTest = require("mini.test")
local Locator = require("louiselm.session.locator")

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim
local temp_dir
local T = MiniTest.new_set({
  hooks = {
    pre_case = function()
      temp_dir = nvim.fn.tempname()
      assert(nvim.fn.mkdir(temp_dir, "p") == 1)
    end,
    post_case = function()
      nvim.fn.delete(temp_dir, "rf")
      temp_dir = nil
    end,
  },
})

local function write_file(...)
  local path = nvim.fs.joinpath(...)
  assert(nvim.fn.mkdir(nvim.fs.dirname(path), "p") == 1)
  assert(nvim.fn.writefile({ "transcript" }, path) == 0)
  return path
end

local function roots()
  return {
    claude = nvim.fs.joinpath(temp_dir, "claude"),
    codex = nvim.fs.joinpath(temp_dir, "codex"),
    ["openai-compatible"] = nvim.fs.joinpath(temp_dir, "openai-compatible"),
    copilot = nvim.fs.joinpath(temp_dir, "copilot"),
  }
end

T["resolve"] = MiniTest.new_set()

T["resolve"]["resolves every supported transcript layout"] = function()
  local configured_roots = roots()
  local cases = {
    {
      layout = "claude",
      id = "claude-id",
      path = write_file(configured_roots.claude, "projects", "-tmp-project", "claude-id.jsonl"),
    },
    {
      layout = "codex",
      id = "codex-id",
      path = write_file(
        configured_roots.codex,
        "sessions",
        "2026",
        "08",
        "26",
        "rollout-2026-08-26T12-00-00-codex-id.jsonl"
      ),
    },
    {
      layout = "openai-compatible",
      id = "session-deepseek-id",
      path = write_file(configured_roots["openai-compatible"], "sessions", "session-deepseek-id", "history.jsonl"),
    },
    {
      layout = "copilot",
      id = "copilot-id",
      path = write_file(configured_roots.copilot, "session-state", "copilot-id", "events.jsonl"),
    },
  }

  for _, case in ipairs(cases) do
    local path, error_message = Locator.resolve("label/" .. case.id, {
      label = { transcript_layout = case.layout },
    }, { roots = configured_roots })

    MiniTest.expect.equality(error_message, nil)
    MiniTest.expect.equality(path, case.path)

    assert(nvim.fn.delete(case.path) == 0)
    path, error_message = Locator.resolve("label/" .. case.id, {
      label = { transcript_layout = case.layout },
    }, { roots = configured_roots })
    MiniTest.expect.equality(path, nil)
    MiniTest.expect.equality(
      error_message,
      "transcript not found for Session 'label/" .. case.id .. "' in configured layout: " .. case.layout
    )
  end
end

T["resolve"]["keys resolution on layout rather than configured Agent name"] = function()
  local configured_roots = roots()
  local path = write_file(configured_roots.claude, "projects", "-tmp-project", "historical-id.jsonl")
  local definitions = {
    renamed = { transcript_layout = "claude" },
    second_name = { transcript_layout = "claude" },
  }

  MiniTest.expect.equality(
    Locator.resolve("original-name/historical-id", definitions, { roots = configured_roots }),
    path
  )
  MiniTest.expect.equality(Locator.resolve("renamed/historical-id", definitions, { roots = configured_roots }), path)
  MiniTest.expect.equality(
    Locator.resolve("second_name/historical-id", definitions, { roots = configured_roots }),
    path
  )
end

T["resolve"]["distinguishes malformed ids, unknown layouts, and missing transcripts"] = function()
  local configured_roots = roots()
  local path, error_message = Locator.resolve("missing-separator", {}, { roots = configured_roots })
  MiniTest.expect.equality(path, nil)
  MiniTest.expect.equality(error_message, "invalid Session id: expected <agent>/<session-id>")

  path, error_message = Locator.resolve("agent/session-id", {
    agent = { transcript_layout = "mystery" },
  }, { roots = configured_roots })
  MiniTest.expect.equality(path, nil)
  MiniTest.expect.equality(error_message, "unknown transcript layout 'mystery'")

  path, error_message = Locator.resolve("agent/session-id", {
    agent = { transcript_layout = "copilot" },
  }, { roots = configured_roots })
  MiniTest.expect.equality(path, nil)
  MiniTest.expect.equality(
    error_message,
    "transcript not found for Session 'agent/session-id' in configured layout: copilot"
  )
end

return T
