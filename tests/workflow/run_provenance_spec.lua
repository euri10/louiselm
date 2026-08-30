---Generated work resolves back to the Session that generated it.
---
---Both halves of this claim are already tested apart: the broker stamps the
---attached Run's Session as the Beads actor, and Provenance classifies actor
---strings into Session identities. Nothing joined them, so nothing proved that
---an issue a Run actually created can be traced to the Run that created it —
---which is the only form of the claim that matters when asking who filed this.

local MiniTest = require("mini.test")
local Correlate = require("louiselm.provenance.correlate")
---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local RUN_ID = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"
local SESSION_ID = "claude/provenance-acceptance"

local T = MiniTest.new_set()

T["an issue created through a Run resolves to that Run's Session"] = function()
  local built = nvim.fn.getcwd() .. "/capture-service/target/debug/louiselm-capture"
  local capture = nvim.fn.executable(built) == 1 and built or nvim.fn.exepath("louiselm-capture")
  local beads = nvim.fn.exepath("br")
  if capture == "" or beads == "" then
    MiniTest.skip("louiselm-capture and br must both be available for Provenance acceptance")
  end
  local root = nvim.fn.tempname()
  nvim.fn.mkdir(root, "p")
  MiniTest.expect.equality(nvim.system({ beads, "init" }, { cwd = root, text = true }):wait().code, 0)
  local environment = {
    LOUISELM_CAPTURE_DATA_DIR = root .. "/data",
    LOUISELM_CAPTURE_STATE_DIR = root .. "/state",
    LOUISELM_CAPTURE_CONFIG_DIR = root .. "/state",
  }
  local admission = nvim
    .system({
      capture,
      "run",
      "admit",
      "--id",
      RUN_ID,
      "--generated-work-max",
      "2",
      "--park-ttl-ms",
      "3600000",
    }, { text = true, env = environment })
    :wait()
  MiniTest.expect.equality(admission.code, 0)
  MiniTest.expect.equality(
    nvim
      .system({
        capture,
        "run",
        "attach",
        "--id",
        RUN_ID,
        "--session-id",
        SESSION_ID,
        "--agent",
        "claude",
        "--acp-session-id",
        "acp-1",
        "--cwd",
        root,
        "--load-session",
        "true",
      }, { text = true, env = environment })
      :wait().code,
    0
  )

  local database = root .. "/.beads/beads.db"
  local created = nvim
    .system({ nvim.fn.getcwd() .. "/scripts/run-tools/br", "create", "Finding from a Run" }, {
      text = true,
      env = nvim.tbl_extend("force", {}, environment, {
        LOUISELM_RUN_ID = RUN_ID,
        LOUISELM_RUN_TOKEN = nvim.json.decode(admission.stdout).token,
        LOUISELM_CAPTURE = capture,
        LOUISELM_REAL_BR = beads,
        BEADS_DB = database,
      }),
    })
    :wait()
  MiniTest.expect.equality(created.code, 0)

  -- Read the issue back out of Beads rather than trusting the create reply.
  local issue_id = nvim.json.decode(created.stdout).id
  local shown = nvim.system({ beads, "show", issue_id, "--db", database, "--json" }, { text = true }):wait()
  MiniTest.expect.equality(shown.code, 0)
  local issue = nvim.json.decode(shown.stdout)[1]

  -- The actor Beads recorded is the Run's attached Session, not the OS user
  -- and not the capture service, so the generated work is attributable.
  MiniTest.expect.equality(issue.created_by, SESSION_ID)
  local resolved, error_value = Correlate.resolve_actor(issue.created_by)
  MiniTest.expect.equality(error_value, nil)
  MiniTest.expect.equality(resolved, {
    raw = SESSION_ID,
    kind = "session",
    session_id = SESSION_ID,
  })

  local edges = assert(Correlate.issue_actors(issue))
  local sessions = {}
  for _, edge in ipairs(edges) do
    if edge.target.kind == "session" then
      sessions[#sessions + 1] = edge.target.id
    end
  end
  MiniTest.expect.equality(sessions, { SESSION_ID })
end

return T
