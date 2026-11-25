local MiniTest = require("mini.test")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local T = MiniTest.new_set()
local PRIVATE_FILE_ERROR = "Run selection must be an operator-owned mode-0600 JSON file, at most 64KiB"

local function selection_file(bytes, permissions)
  local path = nvim.fn.tempname()
  MiniTest.finally(function()
    nvim.fn.delete(path)
  end)
  nvim.fn.writefile({ bytes }, path, "b")
  assert(nvim.fn.setfperm(path, permissions or "rw-------") == 1)
  return path
end

local function refusal(path, expected)
  -- Real entrypoint and runtime APIs, in a separate editor; no global stubs.
  -- Every fixture has invalid selection data, so none can authorize a Run.
  local result = nvim
    .system({
      nvim.v.progpath,
      "--headless",
      "--noplugin",
      "-u",
      "NONE",
      "-i",
      "NONE",
      "--cmd",
      "set runtimepath^=" .. nvim.fn.fnameescape(nvim.fn.getcwd()),
      "--cmd",
      "let g:louiselm_bead_run_selection=" .. nvim.fn.string(path),
      "-c",
      "luafile scripts/bead-run.lua",
      "-c",
      "qa!",
    }, { text = true, timeout = 5000 })
    :wait()
  assert(result.code == 0, result.stderr)
  assert(result.stderr:find(expected, 1, true), result.stderr)
end

T["owned private file reaches selection validation through the real entrypoint"] = function()
  -- Regression: the live 0.12.5 entrypoint crashed on vim.uv.os_getuid before
  -- operator.new, which the previous installed/unit tests called directly.
  refusal(selection_file("{}"), "invalid closed operator Run selection or options")
end

T["selection file size ceiling is inclusive"] = function()
  refusal(selection_file(string.rep(" ", 65534) .. "{}"), "invalid closed operator Run selection or options")
  refusal(selection_file(string.rep(" ", 65535) .. "{}"), PRIVATE_FILE_ERROR)
end

T["refuses public or executable selection files before JSON decoding"] = function()
  for _, permissions in ipairs({ "rw-r--r--", "rw-rw----", "rwx------" }) do
    refusal(selection_file("not JSON", permissions), PRIVATE_FILE_ERROR)
  end
end

T["refuses missing paths and directories before selection validation"] = function()
  refusal(nvim.fn.tempname(), PRIVATE_FILE_ERROR)
  refusal(nvim.fn.getcwd(), PRIVATE_FILE_ERROR)
end

T["reports malformed private JSON without a Lua crash"] = function()
  refusal(selection_file("not JSON"), "Run selection is invalid JSON")
end

T["waiting entrypoint rejects an invalid registration before starting a process"] = function()
  local result = nvim
    .system({
      nvim.v.progpath,
      "--headless",
      "--noplugin",
      "-u",
      "NONE",
      "-i",
      "NONE",
      "--cmd",
      "set runtimepath^=" .. nvim.fn.fnameescape(nvim.fn.getcwd()),
      "--cmd",
      "let g:louiselm_bead_run_registration='invalid'",
      "--cmd",
      "let g:louiselm_bead_run_revision=1",
      "-c",
      "luafile scripts/bead-run-waiting.lua",
      "-c",
      "qa!",
    }, { text = true, timeout = 5000 })
    :wait()
  assert(result.code == 0, result.stderr)
  assert(result.stderr:find("invalid registered Run request", 1, true), result.stderr)
end

local function operator_selection()
  local digest = "sha256:" .. string.rep("a", 64)
  return {
    schema = "louiselm.operator.bead-run/2",
    envelope = {
      run_id = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee",
      envelope_id = "env",
      envelope_revision = 1,
      controller_uid = 1000,
      expires_at_ms = 0,
      verification_plan_digest = digest,
      max_sessions = 6,
      bead_scope = {
        role = "coordinator",
        issue_ids = { "a", "b", "c" },
        effects = { { kind = "comment_add" } },
        max_mutations = 3,
        expires_at_ms = 0,
      },
      provider_requests = { models = { "gpt-5.6-luna" }, max_effort = "low", max_run_requests = 3, expires_at_ms = 0 },
    },
    beads = { { id = "a", prompt = "A" }, { id = "b", prompt = "B" }, { id = "c", prompt = "C" } },
    manifest = {
      agent = { id = "codex" },
      envelope = { id = "env", revision = 1 },
      skills = { mode = "off", view_digest = digest },
      cache_base_digest = digest,
      isolation = { network = "denied" },
    },
    cache = "/private/cache",
    plan = "/private/plan",
    snapshot_parent = "/private/snapshots",
    input_group = 1500,
    worktree = { path = "/private/run", journal_parent = "/private/journal", head = string.rep("a", 40) },
  }
end

T["operator start refuses an invalid explicit phone registration before rehearsal"] = function()
  local path = selection_file(nvim.json.encode(operator_selection()))
  local result = nvim
    .system({
      nvim.v.progpath,
      "--headless",
      "--noplugin",
      "-u",
      "NONE",
      "-i",
      "NONE",
      "--cmd",
      "set runtimepath^=" .. nvim.fn.fnameescape(nvim.fn.getcwd()),
      "--cmd",
      "let g:louiselm_bead_run_selection=" .. nvim.fn.string(path),
      "--cmd",
      "let g:louiselm_bead_run_registration='invalid'",
      "--cmd",
      "let g:louiselm_bead_run_revision=1",
      "-c",
      "luafile scripts/bead-run.lua",
      "-c",
      "qa!",
    }, { text = true, timeout = 5000 })
    :wait()
  assert(result.code == 0, result.stderr)
  assert(result.stderr:find("invalid registered Run request", 1, true), result.stderr)
end

T["operator start binds the exact published bytes before readiness"] = function()
  local selection = operator_selection()
  local selection_json = nvim.json.encode(selection)
  local path = selection_file(selection_json)
  -- Preserve a real trailing newline: the owner hashes the exact file bytes.
  nvim.fn.writefile({ selection_json, "" }, path, "b")
  local directory = nvim.fn.tempname()
  assert(nvim.fn.mkdir(directory, "p", 448) == 1)
  MiniTest.finally(function()
    nvim.fn.delete(directory, "rf")
  end)
  assert(
    nvim.uv.fs_symlink(
      nvim.fn.getcwd() .. "/tests/fixtures/phone_prepared_open_stub.sh",
      directory .. "/louiselm-capture"
    )
  )
  local registration = string.rep("1", 32)
  local function invoke(saved_selection)
    local response = nvim.json.encode({
      v = 1,
      type = "prepared_waiting",
      registration_id = registration,
      revision = 1,
      binding = {
        run_id = selection.envelope.run_id,
        project_id = string.rep("2", 32),
        source_id = string.rep("3", 32),
        checkout_id = string.rep("4", 32),
        agents = { "codex" },
      },
      selection = saved_selection,
    })
    local result = nvim
      .system({
        nvim.v.progpath,
        "--headless",
        "--noplugin",
        "-u",
        "NONE",
        "-i",
        "NONE",
        "--cmd",
        "set runtimepath^=" .. nvim.fn.fnameescape(nvim.fn.getcwd()),
        "--cmd",
        "let g:louiselm_bead_run_selection=" .. nvim.fn.string(path),
        "--cmd",
        "let g:louiselm_bead_run_registration=" .. nvim.fn.string(registration),
        "--cmd",
        "let g:louiselm_bead_run_revision=1",
        "-c",
        "luafile scripts/bead-run.lua",
        "-c",
        "lua vim.wait(500)",
        "-c",
        "qa!",
      }, {
        text = true,
        timeout = 5000,
        env = {
          PATH = directory .. ":" .. (nvim.env.PATH or ""),
          LOUISELM_TEST_PREPARED_RESPONSE = response,
        },
      })
      :wait()
    assert(result.code == 0, result.stderr)
    return result.stderr
  end
  local changed = nvim.deepcopy(selection)
  changed.beads[1].prompt = "changed after publication"
  local mismatch = invoke(nvim.json.encode(changed))
  assert(mismatch:find("Phone-published selection differs", 1, true), mismatch)
  local matching = invoke(selection_json .. "\n")
  assert(matching:find("could not create temporary Run readiness selection", 1, true), matching)
end

return T
