local MiniTest = require("mini.test")
local Workflow = require("louiselm.workflow")

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

local function write_skill(name, lines)
  local directory = nvim.fs.joinpath(temp_dir, name)
  assert(nvim.fn.mkdir(directory, "p") == 1)
  local path = nvim.fs.joinpath(directory, "SKILL.md")
  assert(nvim.fn.writefile(lines, path) == 0)
  return path
end

T["discovers a selected workflow manifest from skill frontmatter"] = function()
  write_skill("execute", {
    "---",
    "name: execute",
    "description: Execute the work",
    "workflow: qa-review",
    "entry: true",
    "generated-work:",
    "  max: 5",
    "park-expiry: 1h",
    "outcomes:",
    "  - name: review",
    "    to: qa-review",
    "---",
  })
  write_skill("qa-review", {
    "---",
    "name: qa-review",
    "description: Review the work",
    "workflow: qa-review",
    "outcomes:",
    "  - name: accepted",
    "    terminal: true",
    "---",
  })
  write_skill("other", {
    "---",
    "name: other",
    "description: Other workflow",
    "workflow: other",
    "entry: true",
    "---",
  })
  write_skill("ordinary", {
    "---",
    "name: ordinary",
    "description: Not a workflow stage",
    "---",
  })

  local manifest, diagnostics = Workflow.discover("qa-review", { temp_dir })

  MiniTest.expect.equality(diagnostics, {})
  MiniTest.expect.equality(Workflow.validate("qa-review", manifest).ok, true)
  MiniTest.expect.equality(manifest.execute["generated-work"].max, 5)
  MiniTest.expect.equality(manifest["qa-review"].outcomes[1].to, nil)
  MiniTest.expect.equality(manifest.other, nil)
  MiniTest.expect.equality(manifest.ordinary, nil)

  local executor = assert(Workflow.new_executor("qa-review", manifest))
  ---@type louiselm.workflow.TransitionResult?
  local transition
  assert(executor:advance("review", nil, function(result)
    transition = result
  end))
  nvim.wait(1000, function()
    return transition ~= nil
  end)
  local first_transition = assert(transition)
  MiniTest.expect.equality(first_transition.to, "qa-review")

  transition = nil
  assert(executor:advance("accepted", nil, function(result)
    transition = result
  end))
  nvim.wait(1000, function()
    return transition ~= nil
  end)
  local final_transition = assert(transition)
  MiniTest.expect.equality(final_transition.terminal, true)
end

T["keeps malformed stage fields visible for Validation to reject"] = function()
  write_skill("broken", {
    "---",
    "name: broken",
    "description: Broken workflow stage",
    "workflow: qa-review",
    "entry: true",
    "outcomes: not-an-array",
    "park-expiry: 1h",
    "---",
  })

  local manifest, diagnostics = Workflow.discover("qa-review", { temp_dir })
  local result = Workflow.validate("qa-review", manifest)

  MiniTest.expect.equality(diagnostics, {})
  MiniTest.expect.equality(result.ok, false)
  MiniTest.expect.equality(result.rejections[1].reason, "wrong_field_type")
  MiniTest.expect.equality(result.rejections[1].field, "outcomes")
end

T["reports a workflow marker with an invalid type"] = function()
  local path = write_skill("broken", {
    "---",
    "name: broken",
    "description: Broken workflow marker",
    "workflow: 42",
    "---",
  })

  local manifest, diagnostics = Workflow.discover("qa-review", { temp_dir })

  MiniTest.expect.equality(manifest, {})
  MiniTest.expect.equality(diagnostics, {
    {
      path = path,
      message = "workflow frontmatter must name a non-empty workflow",
    },
  })
end

return T
